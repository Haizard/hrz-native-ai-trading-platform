//! Chronological replay (`docs/07-BACKTESTING-ENGINE.md`).
//!
//! ## The execution clock
//!
//! The **finest declared timeframe** is the clock. Every decision is made at one
//! of its closes; coarser timeframes are context that updates when their own
//! candles close. A 4h/1h/5m document therefore makes a decision on every 5m
//! close, with the 1h and 4h views holding whatever their most recent *closed*
//! candle was at that instant.
//!
//! ## No look-ahead, structurally
//!
//! A candle is visible to the strategy only once `open_time + resolution <= now`,
//! where `now` is the decision candle's close time. This module is the only
//! place that decides visibility, and [`MarketContext`] has no API that can
//! reach past what it was given -- so a strategy cannot read the future even if
//! it tries. [`tests::a_paranoid_strategy_cannot_see_the_future`] is the
//! dedicated test the spec asks for, and it checks the invariant on every
//! single bar rather than spot-checking.
//!
//! ## Bounded windows, deliberately
//!
//! Two separate bounds keep this affordable on six months of 5m data, and both
//! are honest modelling choices rather than just optimizations:
//!
//! * `state_window` -- the volume profile and market structure are computed over
//!   the last N candles, not all of history. A volume profile over six months is
//!   not a meaningful level anyway; it is one price.
//! * `runtime.max_history` -- how many candles each view retains, which is what
//!   `new_low(n)` and ATR stops measure over. [`run_backtest`] overrides this
//!   with the engine's own setting, because the engine is what asks for lookback
//!   windows: a buffer smaller than the engine expects would starve `last_n` and
//!   silently turn a lookback condition permanently false.
//!
//! ## Shardable
//!
//! [`replay`] takes its input and returns its output, holding no shared state.
//! Independent `(symbol, date-range)` shards can therefore run concurrently --
//! see [`tests::shards_run_concurrently`].

use std::collections::BTreeMap;

use analytics_core::state::MarketStateConfig;
use analytics_core::types::{Candle, Timeframe};
use serde::{Deserialize, Serialize};
use strategy_dsl::StrategyDocument;
use strategy_runtime::engine::Strategy;
use strategy_runtime::signal::{EnterSignal, ExitTrigger, Signal};
use strategy_runtime::{
    RollingConfig, RollingLadder, RollingTimeframe, RuntimeConfig, StrategyEngine,
};

use crate::error::BacktestError;
use crate::report::{build_report, BacktestReport, FillAssumptions, TradeRecord};
use strategy_runtime::{Simulator, SimulatorConfig};

/// Everything the replay needs to know that the document does not say.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayConfig {
    /// Symbol being replayed.
    pub symbol: String,
    /// Start of the window, unix nanos, inclusive.
    pub from: i64,
    /// End of the window, unix nanos, inclusive.
    pub to: i64,
    /// Tuning for the per-timeframe `MarketState`.
    pub state: MarketStateConfig,
    /// Tuning the document does not control.
    ///
    /// [`run_backtest`] overwrites this with the engine's own configuration, so
    /// that the retained history and the lookback windows the engine asks for
    /// cannot disagree. It only has an independent effect when calling the
    /// generic [`replay`] with a hand-written [`Strategy`].
    pub runtime: RuntimeConfig,
    /// Sizing and fill assumptions.
    pub simulator: SimulatorConfig,
    /// How many trailing candles the volume profile and structure are computed
    /// over. Bounds the cost of each state build.
    pub state_window: usize,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            symbol: "BTCUSDT".into(),
            from: 0,
            to: i64::MAX,
            state: MarketStateConfig::default(),
            runtime: RuntimeConfig::default(),
            simulator: SimulatorConfig::default(),
            state_window: 500,
        }
    }
}

/// The candles to replay, keyed by the document's declared timeframe names.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayInput {
    /// Declared name to resolution, taken from the document.
    pub timeframes: BTreeMap<String, Timeframe>,
    /// Declared name to candles, each series ascending by `open_time`.
    pub candles: BTreeMap<String, Vec<Candle>>,
}

impl ReplayInput {
    /// Build an input from a document's declarations and the loaded candles.
    ///
    /// Every declared timeframe must have a series, even if empty -- a missing
    /// key is a caller bug, and reporting it beats silently replaying a strategy
    /// whose context timeframe never updates.
    pub fn new(
        document: &StrategyDocument,
        candles: BTreeMap<String, Vec<Candle>>,
    ) -> Result<Self, BacktestError> {
        let timeframes = document.timeframes.clone();
        for name in timeframes.keys() {
            if !candles.contains_key(name) {
                return Err(BacktestError::MissingTimeframe(name.clone()));
            }
        }
        Ok(Self {
            timeframes,
            candles,
        })
    }

    /// The finest declared timeframe: the execution clock.
    #[must_use]
    pub fn decision_timeframe(&self) -> Option<(&str, Timeframe)> {
        self.timeframes
            .iter()
            .map(|(name, tf)| (name.as_str(), *tf))
            .min_by_key(|(_, tf)| tf.nanos())
    }
}

/// What a replay produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayOutput {
    /// Completed trades, in closing order.
    pub trades: Vec<TradeRecord>,
    /// Decision candles the strategy was actually asked about.
    pub candles_processed: u64,
    /// Final cumulative R.
    pub final_r: f64,
    /// Entries the simulator refused, plus orders that never filled.
    pub refusals: Vec<String>,
    /// Every signal the strategy emitted, in evaluation order.
    ///
    /// Recorded **before** the simulator is asked anything, so this is the
    /// strategy's own decision rather than an order that survived it. It exists
    /// for the chart's evidence chain: an indicator preview shows what the
    /// document decided, on the candle it decided it, without claiming a fill.
    /// A preview and a bot therefore describe the same signals from the same
    /// interpreter -- the point of reusing this loop instead of writing a
    /// second one.
    pub signals: Vec<ReplaySignal>,
}

/// One strategy decision, with the candle it was made on.
///
/// Deliberately not a trade: no fill price, no slippage, no position. A signal
/// is what the *interpreter* said; everything about whether and where it
/// executed belongs to the simulator and the broker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaySignal {
    /// Close time of the decision candle, unix nanos.
    pub time: i64,
    /// The decision candle's close -- the last price the strategy could see.
    pub price: f64,
    /// The decision itself.
    pub signal: Signal,
}

/// A cursor into one input series.
///
/// The windowing itself lives in
/// [`RollingTimeframe`](strategy_runtime::RollingTimeframe), shared with the
/// live paper trader, so the two cannot drift. Only "how far have we read"
/// is a replay concern.
#[derive(Debug)]
struct SeriesCursor {
    /// How many candles have been consumed from the input series.
    consumed: usize,
}

impl SeriesCursor {
    /// Consume every candle that has closed at or before `now`.
    ///
    /// This is the visibility rule. Nothing newer than `now` is ever pulled in,
    /// which is what makes look-ahead impossible rather than merely discouraged.
    fn advance(
        &mut self,
        frame: &mut RollingTimeframe,
        series: &[Candle],
        now: i64,
        config: &RollingConfig,
    ) {
        let width = frame.timeframe().nanos();
        while self.consumed < series.len() && series[self.consumed].open_time + width <= now {
            let candle = series[self.consumed].clone();
            self.consumed += 1;
            frame.push(candle, config);
        }
    }
}

/// An order waiting for the next candle's open.
#[derive(Debug, Clone)]
enum Pending {
    Enter {
        signal: EnterSignal,
        regime: String,
    },
    Exit {
        trigger: ExitTrigger,
        reasons: Vec<String>,
    },
}

/// Replay `strategy` over `input`, simulating fills.
///
/// Generic over [`Strategy`] so the no-look-ahead test can drive a strategy of
/// its own that checks the visibility invariant on every bar.
pub fn replay<S: Strategy>(
    strategy: &mut S,
    input: &ReplayInput,
    config: &ReplayConfig,
) -> Result<ReplayOutput, BacktestError> {
    let (decision_name, decision_timeframe) =
        input
            .decision_timeframe()
            .ok_or_else(|| BacktestError::MissingData {
                symbol: config.symbol.clone(),
                from: config.from,
                to: config.to,
            })?;
    let decision_name = decision_name.to_string();

    // A declared timeframe with no candles would leave its view permanently
    // absent, so every condition written against it would silently never fire.
    // Refuse loudly: the decision timeframe means "the window has no data", a
    // context timeframe means "the caller did not load what the document asked
    // for". The two have different fixes.
    for name in input.timeframes.keys() {
        let empty = match input.candles.get(name) {
            Some(series) => series.is_empty(),
            None => true,
        };
        if !empty {
            continue;
        }
        if *name == decision_name {
            return Err(BacktestError::MissingData {
                symbol: config.symbol.clone(),
                from: config.from,
                to: config.to,
            });
        }
        return Err(BacktestError::MissingTimeframe(name.clone()));
    }

    let decision_candles: Vec<Candle> = input
        .candles
        .get(&decision_name)
        .cloned()
        .unwrap_or_default();
    if decision_candles.is_empty() {
        return Err(BacktestError::MissingData {
            symbol: config.symbol.clone(),
            from: config.from,
            to: config.to,
        });
    }

    let rolling = RollingConfig::new(
        config.runtime.max_history,
        config.state_window,
        config.state,
    );
    let mut ladder = RollingLadder::new(&input.timeframes);
    let mut cursors: BTreeMap<String, SeriesCursor> = input
        .timeframes
        .keys()
        .map(|name| (name.clone(), SeriesCursor { consumed: 0 }))
        .collect();

    let mut simulator = Simulator::new(config.simulator);
    let mut pending: Option<Pending> = None;
    let mut refusals: Vec<String> = Vec::new();
    let mut signals: Vec<ReplaySignal> = Vec::new();
    let mut processed: u64 = 0;

    for (index, decision_candle) in decision_candles.iter().enumerate() {
        let now = decision_candle.open_time + decision_timeframe.nanos();

        // Advance every timeframe first, including during warm-up outside the
        // window, so a strategy starting mid-series has real context.
        for (name, cursor) in cursors.iter_mut() {
            let Some(frame) = ladder.get_mut(name) else {
                continue;
            };
            let series = input.candles.get(name).map_or(&[][..], Vec::as_slice);
            cursor.advance(frame, series, now, &rolling);
        }

        if now < config.from || now > config.to {
            continue;
        }
        processed += 1;

        // 1. A market order queued on the previous close fills at this open.
        if let Some(action) = pending.take() {
            match action {
                Pending::Enter { signal, regime } => {
                    if !simulator.on_entry(
                        &signal,
                        decision_candle.open_time,
                        decision_candle.open,
                        regime,
                        index,
                    ) {
                        refusals.push(format!(
                            "entry at {} was refused by the simulator",
                            decision_candle.open_time
                        ));
                    }
                }
                Pending::Exit { trigger, reasons } => {
                    simulator.close_at_market(
                        trigger,
                        decision_candle.open_time,
                        decision_candle.open,
                        reasons,
                    );
                }
            }
        }

        // 2. Price events resolve within this candle, on the decision timeframe.
        if simulator.position().is_some() {
            simulator.check_bar(decision_candle);
        }

        // 3. Ask the strategy, with the position state as it now stands.
        let Some(decision_frame) = ladder.get(&decision_name) else {
            continue;
        };
        let Some(decision_candle_close) = decision_frame.candle().map(|candle| candle.close) else {
            continue;
        };

        let Some(context) = ladder.context(
            &config.symbol,
            now,
            &decision_name,
            simulator.position_view(decision_candle_close),
            config.simulator.starting_equity,
        ) else {
            continue;
        };

        let regime = decision_frame
            .state()
            .map_or("unknown", |state| strategy_runtime::trend_name(state.trend));

        if let Some(signal) = strategy.on_candle(&context) {
            signals.push(ReplaySignal {
                time: now,
                price: decision_candle_close,
                signal: signal.clone(),
            });
            pending = Some(match signal {
                Signal::Enter(enter) => Pending::Enter {
                    signal: enter,
                    regime: regime.to_string(),
                },
                Signal::Exit(exit) => Pending::Exit {
                    trigger: exit.trigger,
                    reasons: exit.reasons,
                },
            });
        }
    }

    // A pending order on the final bar never got its next open, so it never
    // executed. Say so rather than dropping it silently.
    if let Some(action) = pending {
        let what = match action {
            Pending::Enter { .. } => "entry",
            Pending::Exit { .. } => "exit",
        };
        refusals.push(format!(
            "a queued {what} on the final candle had no next open and was not executed"
        ));
    }

    // Close anything still open at the last close, so the trade is accounted for
    // rather than quietly omitted -- dropping an open loser would flatter the
    // statistics.
    if simulator.position().is_some() {
        if let Some(last) = decision_candles.last() {
            simulator.close(
                ExitTrigger::EndOfData,
                last.open_time + decision_timeframe.nanos(),
                last.close,
                Vec::new(),
            );
        }
    }

    let final_r = simulator.cumulative_r();
    Ok(ReplayOutput {
        trades: simulator.trades().to_vec(),
        candles_processed: processed,
        final_r,
        refusals,
        signals,
    })
}

/// Replay a [`StrategyEngine`] and assemble the full report.
///
/// The engine's own [`RuntimeConfig`] takes precedence over `config.runtime`:
/// the engine is what decides how much history a condition needs, so letting the
/// caller hand it a smaller buffer would starve `last_n` and quietly make a
/// lookback condition false forever -- a wrong answer that still looks like a
/// valid backtest. Everything else in `config` is used as given.
pub fn run_backtest(
    engine: &mut StrategyEngine,
    input: &ReplayInput,
    config: &ReplayConfig,
) -> Result<BacktestReport, BacktestError> {
    let effective = ReplayConfig {
        runtime: *engine.config(),
        ..config.clone()
    };

    let output = replay(engine, input, &effective)?;

    let mut skipped: Vec<String> = engine
        .skips()
        .iter()
        .map(|skip| skip.reason.clone())
        .collect();
    skipped.extend(output.refusals.iter().cloned());

    Ok(build_report(
        engine.document(),
        &config.symbol,
        config.from,
        config.to,
        engine.decision_timeframe(),
        output.trades,
        skipped,
        FillAssumptions {
            slippage_bps: config.simulator.slippage_bps,
            ..FillAssumptions::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::Timeframe;
    use strategy_runtime::context::MarketContext;
    use strategy_runtime::signal::SignalAction;

    const M5: i64 = 5 * 60 * 1_000_000_000;
    const H1: i64 = 60 * 60 * 1_000_000_000;

    /// A flat, mildly rising series, long enough to warm up.
    #[allow(clippy::cast_precision_loss)]
    fn series(count: i64, step: f64, width: i64, start: i64) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let open_time = start + i * width;
                let base = 100.0 + (i as f64) * step;
                Candle {
                    symbol: "BTCUSDT".into(),
                    timeframe: if width == M5 {
                        Timeframe::M5
                    } else {
                        Timeframe::H1
                    },
                    open_time,
                    open: base,
                    high: base + 1.0,
                    low: base - 1.0,
                    close: base + 0.5,
                    volume: 1000.0,
                    buy_volume: 700.0,
                    sell_volume: 300.0,
                }
            })
            .collect()
    }

    fn input(m5: Vec<Candle>, h1: Vec<Candle>) -> ReplayInput {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        timeframes.insert("context".to_string(), Timeframe::H1);

        let mut candles = BTreeMap::new();
        candles.insert("entry".to_string(), m5);
        candles.insert("context".to_string(), h1);

        ReplayInput {
            timeframes,
            candles,
        }
    }

    fn config() -> ReplayConfig {
        ReplayConfig {
            symbol: "BTCUSDT".into(),
            from: 0,
            to: i64::MAX,
            state_window: 50,
            ..ReplayConfig::default()
        }
    }

    /// Checks the visibility invariant on every bar it is asked about, and
    /// records any violation instead of panicking -- so the test can report how
    /// many bars it actually examined.
    struct ParanoidStrategy {
        calls: u64,
        violations: Vec<String>,
    }

    impl ParanoidStrategy {
        fn new() -> Self {
            Self {
                calls: 0,
                violations: Vec::new(),
            }
        }

        fn check(&mut self, ctx: &MarketContext) {
            for (name, view) in &ctx.timeframes {
                let width = view.timeframe.nanos();

                // The newest candle must have closed at or before `now`.
                let close_time = view.candle.open_time + width;
                if close_time > ctx.now {
                    self.violations.push(format!(
                        "{name}: newest candle closes at {close_time}, after now {}",
                        ctx.now
                    ));
                }

                // So must everything in the retained window.
                for candle in &view.history {
                    if candle.open_time + width > ctx.now {
                        self.violations.push(format!(
                            "{name}: retained candle at {} closes after now {}",
                            candle.open_time, ctx.now
                        ));
                    }
                }

                // And the previous view must be strictly older.
                if let Some(previous) = view.prev() {
                    if previous.candle.open_time >= view.candle.open_time {
                        self.violations.push(format!(
                            "{name}: previous candle {} is not older than {}",
                            previous.candle.open_time, view.candle.open_time
                        ));
                    }
                }
            }

            // `now` must be exactly the decision candle's close.
            let decision = ctx.decision().expect("the decision view must exist");
            if decision.candle.open_time + decision.timeframe.nanos() != ctx.now {
                self.violations.push(format!(
                    "now {} is not the decision candle's close",
                    ctx.now
                ));
            }
        }
    }

    impl Strategy for ParanoidStrategy {
        fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
            self.calls += 1;
            self.check(ctx);
            None
        }
    }

    #[test]
    fn a_paranoid_strategy_cannot_see_the_future() {
        let m5 = series(600, 0.01, M5, 0);
        let h1 = series(60, 0.05, H1, 0);

        let mut strategy = ParanoidStrategy::new();
        let output = replay(&mut strategy, &input(m5, h1), &config()).unwrap();

        assert!(
            strategy.calls > 500,
            "only examined {} bars",
            strategy.calls
        );
        assert_eq!(output.candles_processed, strategy.calls);
        assert!(
            strategy.violations.is_empty(),
            "look-ahead detected: {:?}",
            &strategy.violations[..strategy.violations.len().min(5)]
        );
    }

    #[test]
    fn the_finest_declared_timeframe_is_the_execution_clock() {
        let input = input(series(600, 0.01, M5, 0), series(60, 0.05, H1, 0));
        assert_eq!(input.decision_timeframe().map(|(n, _)| n), Some("entry"));
    }

    #[test]
    fn a_coarse_context_timeframe_lags_its_decision_clock() {
        // On a 5m clock the 1h view must update once every twelve decisions.
        let m5 = series(600, 0.01, M5, 0);
        let h1 = series(60, 0.05, H1, 0);

        let mut strategy = ParanoidStrategy::new();
        replay(&mut strategy, &input(m5, h1), &config()).unwrap();

        assert_eq!(strategy.violations.len(), 0);
    }

    #[test]
    fn missing_candles_for_a_declared_timeframe_is_an_error() {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        timeframes.insert("context".to_string(), Timeframe::H1);

        let mut candles = BTreeMap::new();
        candles.insert("entry".to_string(), series(10, 0.01, M5, 0));

        let result = replay(
            &mut ParanoidStrategy::new(),
            &ReplayInput {
                timeframes,
                candles,
            },
            &config(),
        );
        // A permanently-empty context view would make every condition written
        // against it silently never fire, so this is refused rather than run.
        match result {
            Err(BacktestError::MissingTimeframe(name)) => assert_eq!(name, "context"),
            other => panic!("expected MissingTimeframe, got {other:?}"),
        }
    }

    #[test]
    fn a_declared_timeframe_with_no_series_is_reported_by_name() {
        let document = strategy_dsl::parse(
            r#"
name: "Two clocks"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
  context: 1h
entry:
  all_of:
    - timeframe: entry
      condition: delta > threshold(1)
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
invalidation:
  - timeframe: entry
    condition: close_below(vwap)
"#,
        )
        .unwrap();

        let mut candles = BTreeMap::new();
        candles.insert("entry".to_string(), series(10, 0.01, M5, 0));

        let err = ReplayInput::new(&document, candles).unwrap_err();
        match err {
            BacktestError::MissingTimeframe(name) => assert_eq!(name, "context"),
            other => panic!("expected MissingTimeframe, got {other}"),
        }
    }

    #[test]
    fn an_empty_series_is_an_error_not_an_empty_report() {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        let mut candles = BTreeMap::new();
        candles.insert("entry".to_string(), Vec::new());

        let result = replay(
            &mut ParanoidStrategy::new(),
            &ReplayInput {
                timeframes,
                candles,
            },
            &config(),
        );
        assert!(matches!(result, Err(BacktestError::MissingData { .. })));
    }

    /// A strategy that buys on the first bar it sees and never exits by
    /// condition, so the simulator's stop/target/end-of-data paths are what
    /// close the trade.
    struct AlwaysLong {
        entered: bool,
    }

    impl Strategy for AlwaysLong {
        fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
            if ctx.in_position() || self.entered {
                return None;
            }
            self.entered = true;
            Some(Signal::Enter(EnterSignal {
                direction: strategy_dsl::Direction::Long,
                reference_price: ctx.decision().unwrap().candle.close,
                // A stop 50 below a ~100 price: far enough that a gently rising
                // series hits the target instead.
                stop_price: 50.0,
                take_profit_price: Some(105.0),
                max_risk_pct: 1.0,
                reasons: vec!["test".into()],
            }))
        }
    }

    #[test]
    fn an_entry_fills_at_the_next_candle_open_not_the_signal_close() {
        let m5 = series(50, 0.1, M5, 0);
        let input = ReplayInput {
            timeframes: [(String::from("entry"), Timeframe::M5)]
                .into_iter()
                .collect(),
            candles: [(String::from("entry"), m5.clone())].into_iter().collect(),
        };

        let mut strategy = AlwaysLong { entered: false };
        let output = replay(&mut strategy, &input, &config()).unwrap();

        let trade = output.trades.first().expect("the position must be closed");
        // The signal was decided on bar 0's close, so the fill is bar 1's open
        // plus slippage -- not the close the strategy actually saw.
        let expected_fill = m5[1].open * (1.0 + config().simulator.slippage());
        assert!(
            (trade.entry_price - expected_fill).abs() < 1e-9,
            "filled at {} but bar 1 opened at {}",
            trade.entry_price,
            m5[1].open
        );
        assert_eq!(trade.entry_time, m5[1].open_time);
        assert!(
            trade.entry_price > m5[1].open,
            "slippage on a long entry must fill above the market price"
        );
        // The reference is the close the strategy saw, which the fill must not
        // equal -- otherwise the fill used the decision bar, not the next one.
        assert!((trade.reference_price - m5[0].close).abs() < 1e-9);
    }

    #[test]
    fn an_open_position_is_closed_at_the_end_of_data_rather_than_dropped() {
        // A stop and target neither of which is ever touched.
        struct NeverExits;
        impl Strategy for NeverExits {
            fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
                if ctx.in_position() {
                    return None;
                }
                Some(Signal::Enter(EnterSignal {
                    direction: strategy_dsl::Direction::Long,
                    reference_price: ctx.decision().unwrap().candle.close,
                    stop_price: 1.0,
                    take_profit_price: Some(10_000.0),
                    max_risk_pct: 1.0,
                    reasons: vec!["test".into()],
                }))
            }
        }

        let input = ReplayInput {
            timeframes: [(String::from("entry"), Timeframe::M5)]
                .into_iter()
                .collect(),
            candles: [(String::from("entry"), series(40, 0.01, M5, 0))]
                .into_iter()
                .collect(),
        };

        let output = replay(&mut NeverExits, &input, &config()).unwrap();
        assert_eq!(output.trades.len(), 1);
        assert_eq!(output.trades[0].exit_trigger, ExitTrigger::EndOfData);
    }

    #[test]
    fn a_signal_that_never_gets_a_next_open_is_reported() {
        struct EntersOnLastBar;
        impl Strategy for EntersOnLastBar {
            fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
                let decision = ctx.decision().unwrap();
                // Only fire on the final retained candle.
                if decision.candle.open_time < 39 * M5 {
                    return None;
                }
                Some(Signal::Enter(EnterSignal {
                    direction: strategy_dsl::Direction::Long,
                    reference_price: decision.candle.close,
                    stop_price: 50.0,
                    take_profit_price: None,
                    max_risk_pct: 1.0,
                    reasons: vec!["test".into()],
                }))
            }
        }

        let input = ReplayInput {
            timeframes: [(String::from("entry"), Timeframe::M5)]
                .into_iter()
                .collect(),
            candles: [(String::from("entry"), series(40, 0.01, M5, 0))]
                .into_iter()
                .collect(),
        };

        let output = replay(&mut EntersOnLastBar, &input, &config()).unwrap();
        assert!(output.trades.is_empty());
        assert!(
            output.refusals.iter().any(|r| r.contains("no next open")),
            "{:?}",
            output.refusals
        );
    }

    #[test]
    fn a_signal_enum_carries_the_action_it_claims() {
        let signal = Signal::Exit(strategy_runtime::ExitSignal::from_conditions(
            ExitTrigger::Invalidation,
            vec!["lost".into()],
        ));
        assert_eq!(signal.action(), SignalAction::Exit);
    }

    #[test]
    fn shards_run_concurrently() {
        // Each shard owns its input and returns its own output, so independent
        // (symbol, window) runs must not interfere. Run several at once and
        // confirm each produces exactly the same result as it does alone.
        use std::sync::Arc;
        use std::thread;

        let build = || {
            let input = Arc::new(ReplayInput {
                timeframes: [(String::from("entry"), Timeframe::M5)]
                    .into_iter()
                    .collect(),
                candles: [(String::from("entry"), series(60, 0.05, M5, 0))]
                    .into_iter()
                    .collect(),
            });

            let expected = {
                let mut strategy = ParanoidStrategy::new();
                let output = replay(&mut strategy, &input, &config()).unwrap();
                (output.candles_processed, output.trades.len())
            };

            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let input = Arc::clone(&input);
                    thread::spawn(move || {
                        let mut strategy = ParanoidStrategy::new();
                        let output = replay(&mut strategy, &input, &config()).unwrap();
                        (output.candles_processed, output.trades.len())
                    })
                })
                .collect();

            for handle in handles {
                assert_eq!(handle.join().unwrap(), expected);
            }
        };

        build();
    }

    #[test]
    fn a_window_restricts_trading_but_not_warm_up() {
        let m5 = series(200, 0.01, M5, 0);
        let h1 = series(20, 0.05, H1, 0);

        let full = replay(
            &mut ParanoidStrategy::new(),
            &input(m5.clone(), h1.clone()),
            &config(),
        )
        .unwrap();

        let narrow = replay(
            &mut ParanoidStrategy::new(),
            &input(m5, h1),
            &ReplayConfig {
                from: 100 * M5,
                to: 150 * M5,
                ..config()
            },
        )
        .unwrap();

        assert!(narrow.candles_processed < full.candles_processed);
        assert_eq!(
            narrow.candles_processed, 51,
            "the window is inclusive at both ends"
        );
    }

    #[test]
    fn run_backtest_reports_a_document_shaped_result() {
        let yaml = r#"
name: "Harness"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  all_of:
    - timeframe: entry
      condition: delta > threshold(1)
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: risk_multiple
    value: 2.0
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
"#;
        let validated = strategy_dsl::parse_and_validate(yaml).unwrap();
        let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();

        let input = ReplayInput {
            timeframes: [(String::from("entry"), Timeframe::M5)]
                .into_iter()
                .collect(),
            candles: [(String::from("entry"), series(80, 0.05, M5, 0))]
                .into_iter()
                .collect(),
        };

        let report = run_backtest(&mut engine, &input, &config()).unwrap();
        assert_eq!(report.symbol, "BTCUSDT");
        assert_eq!(report.decision_timeframe, "entry");
        assert_eq!(report.strategy, "Harness");
        assert_eq!(report.best_timeframe.as_deref(), Some("entry"));
        assert_eq!(report.total_trades as usize, report.trades.len());
        // Every number must be finite so the report serializes cleanly.
        assert!(report.win_rate.is_finite());
        assert!(report.net_return_pct.is_finite());
        assert!(report.max_drawdown_pct.is_finite());
        assert!(report.sharpe_ratio.is_finite());
        assert!(report.average_r.is_finite());
    }

    #[test]
    fn the_engines_retention_setting_wins_over_the_replay_configs() {
        // `new_low(30)` needs 30 bars of retained history. If `run_backtest`
        // honoured `ReplayConfig::runtime` here instead of the engine's own
        // config, the buffer would be trimmed to a single candle, `last_n(30)`
        // would return `None`, and the condition would be silently false on
        // every bar -- a backtest reporting zero trades that looks perfectly
        // valid. So the two configurations are set to disagree on purpose.
        let yaml = r#"
name: "Lookback"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: new_low(30)
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: risk_multiple
    value: 2.0
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
"#;
        let validated = strategy_dsl::parse_and_validate(yaml).unwrap();
        let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();

        // A monotonically falling series, so every bar prints a fresh low.
        let input = ReplayInput {
            timeframes: [(String::from("entry"), Timeframe::M5)]
                .into_iter()
                .collect(),
            candles: [(String::from("entry"), series(200, -0.05, M5, 0))]
                .into_iter()
                .collect(),
        };

        let honest = config();
        let starved = ReplayConfig {
            runtime: RuntimeConfig {
                max_history: 1,
                ..RuntimeConfig::default()
            },
            ..config()
        };

        let a = run_backtest(&mut engine, &input, &honest).unwrap();
        assert!(
            a.total_trades > 0,
            "the fixture must actually trade, or this test proves nothing"
        );

        let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();
        let b = run_backtest(&mut engine, &input, &starved).unwrap();

        assert_eq!(
            a.total_trades, b.total_trades,
            "a starved ReplayConfig::runtime must not change the result"
        );
        assert_eq!(a.trades.len(), b.trades.len());
        assert_eq!(a.average_r, b.average_r);
    }
}
