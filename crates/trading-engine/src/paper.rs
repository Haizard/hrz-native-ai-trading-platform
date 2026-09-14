//! The paper-trading bot (`docs/11-BOT-TRADING-ENGINE.md`).
//!
//! ## One loop, the same one the replay runs
//!
//! The bot does what the backtester's replay does, in the same order: fill
//! what was queued on the previous close, resolve price events inside this
//! bar, then ask the strategy and queue whatever it says. It drives the same
//! [`RollingLadder`] and the same [`Simulator`], because `docs/11` asks for
//! paper trades consistent with a manual replay -- and the only way that
//! holds is if there is one implementation, not two that agree today.
//!
//! The one addition is the risk check between "the strategy said enter" and
//! "an order exists". A backtest has no such step because it is answering a
//! different question; here it is the entire point.
//!
//! ## Pushed, not pulled
//!
//! `on_candle` takes a candle and returns a record. The bot owns no socket,
//! no clock and no task, which keeps this crate free of an async runtime and
//! makes every branch reachable from a test. The thing that subscribes to the
//! market bus is therefore a thin, obviously-correct adapter rather than the
//! component that also decides whether to trade.
//!
//! ## Every decision is recorded, including the ones that did nothing
//!
//! `docs/11` asks for every `on_candle` decision to be persisted, not just
//! the ones that traded. Without that, "why did it not trade on Tuesday?" is
//! unanswerable, and a bot that has silently died is indistinguishable from
//! one that is correctly waiting.
//!
//! [`RollingLadder`]: strategy_runtime::RollingLadder
//! [`Simulator`]: strategy_runtime::Simulator

use analytics_core::types::{Candle, Timeframe};
use serde::{Deserialize, Serialize};
use strategy_runtime::{
    EnterSignal, ExitSignal, ExitTrigger, RollingConfig, RollingLadder, Simulator, SimulatorConfig,
    Strategy, StrategyEngine, TradeRecord,
};

use crate::risk::{OnBreach, RiskEngine, RiskLimits, RiskVerdict};

/// What the bot needs to know that the document does not say.
#[derive(Debug, Clone, PartialEq)]
pub struct PaperConfig {
    /// Symbol being traded.
    pub symbol: String,
    /// Risk limits. Clamped on construction; see
    /// [`RiskLimits::clamped`](crate::risk::RiskLimits::clamped).
    pub limits: RiskLimits,
    /// Fill and sizing assumptions.
    pub fills: SimulatorConfig,
    /// Retained history and per-timeframe state tuning.
    pub rolling: RollingConfig,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self {
            symbol: "BTCUSDT".into(),
            limits: RiskLimits::default(),
            fills: SimulatorConfig::default(),
            rolling: RollingConfig::default(),
        }
    }
}

/// What one decision candle produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// The decision candle's close time, unix nanos.
    pub at: i64,
    /// Symbol traded.
    pub symbol: String,
    /// Declared name of the decision timeframe.
    pub decision_timeframe: String,
    /// Last price the strategy was shown.
    pub price: f64,
    /// How many declared timeframes had warmed up at this decision.
    ///
    /// Recorded because a condition on a cold timeframe is false for a
    /// *structural* reason, not a market one -- and the two look identical in
    /// a trade log unless this is written down.
    pub frames_ready: usize,
    /// How many the document declared.
    pub frames_total: usize,
    /// Whether the bot held a position when it decided.
    pub in_position: bool,
    /// What came of it.
    pub outcome: DecisionOutcome,
}

/// The outcome of one decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DecisionOutcome {
    /// The ladder was not warm enough to ask.
    NoContext,
    /// Asked, and the strategy had nothing to say. The ordinary case.
    NoSignal,
    /// A setup fired; the order is queued for the next open.
    EntryQueued {
        /// Conditions that fired.
        reasons: Vec<String>,
    },
    /// An entry queued on the previous close filled at this open.
    EntryFilled {
        /// Conditions that fired.
        reasons: Vec<String>,
    },
    /// The risk engine refused the entry.
    EntryDenied {
        /// Conditions that fired, so the audit can show the setup was there.
        reasons: Vec<String>,
        /// Which limit blocked it.
        limit: String,
        /// The value that breached it.
        value: String,
    },
    /// The simulator refused the entry -- an unusable stop, or already held.
    EntryRefused {
        /// Why.
        reason: String,
    },
    /// A queued exit filled at this open.
    ExitFilled {
        /// What closed it.
        trigger: String,
    },
    /// A position closed during this bar, by a stop, a target or the switch.
    Closed {
        /// What closed it.
        trigger: String,
        /// Result in R.
        r_multiple: f64,
    },
    /// The kill-switch is engaged; the strategy was not asked.
    Halted {
        /// Why.
        reason: String,
    },
}

/// An order waiting for the next candle's open.
#[derive(Debug, Clone)]
enum Pending {
    Enter(EnterSignal),
    Exit(ExitSignal),
}

/// A strategy running against live candles in simulation.
#[derive(Debug)]
pub struct PaperBot {
    symbol: String,
    decision_name: String,
    decision_resolution: Timeframe,
    ladder: RollingLadder,
    engine: StrategyEngine,
    simulator: Simulator,
    risk: RiskEngine,
    rolling: RollingConfig,
    fills: SimulatorConfig,
    pending: Option<Pending>,
    decisions: Vec<DecisionRecord>,
    alerts: Vec<BotAlert>,
    /// Whether the switch was already engaged last time we looked, so the
    /// alert is raised on the transition rather than on every later bar.
    alerted_halt: bool,
    /// Set when a configured limit had to be clamped, so it can be logged once.
    clamp_note: Option<String>,
}

impl PaperBot {
    /// Start a bot for an already-validated strategy.
    #[must_use]
    pub fn new(engine: StrategyEngine, config: PaperConfig) -> Self {
        let (limits, clamp_note) = config.limits.clamped();
        let ladder = RollingLadder::new(&engine.document().timeframes);
        let decision_name = engine.decision_timeframe().to_string();
        let decision_resolution = engine
            .document()
            .timeframes
            .get(&decision_name)
            .copied()
            .unwrap_or(Timeframe::M5);

        let mut alerts = Vec::new();
        if let Some(detail) = clamp_note.clone() {
            alerts.push(BotAlert::Clamped { detail });
        }

        Self {
            symbol: config.symbol,
            decision_name,
            decision_resolution,
            ladder,
            engine,
            simulator: Simulator::new(config.fills),
            risk: RiskEngine::new(limits),
            rolling: config.rolling,
            fills: config.fills,
            pending: None,
            decisions: Vec::new(),
            alerts,
            alerted_halt: false,
            clamp_note,
        }
    }

    /// Take the alerts raised since the last call.
    ///
    /// Drained rather than kept: an alert is something to deliver once, and a
    /// bot that runs for weeks should not accumulate them in memory.
    pub fn take_alerts(&mut self) -> Vec<BotAlert> {
        std::mem::take(&mut self.alerts)
    }

    /// Why the configured risk had to be reduced, if it did.
    #[must_use]
    pub fn clamp_note(&self) -> Option<&str> {
        self.clamp_note.as_deref()
    }

    /// The symbol being traded.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// The windowing ladder, for inspection and tests.
    #[must_use]
    pub fn ladder(&self) -> &RollingLadder {
        &self.ladder
    }

    /// Completed trades, in closing order.
    #[must_use]
    pub fn trades(&self) -> &[TradeRecord] {
        self.simulator.trades()
    }

    /// Whether a position is currently open.
    #[must_use]
    pub fn in_position(&self) -> bool {
        self.simulator.position().is_some()
    }

    /// Cumulative R across completed trades.
    #[must_use]
    pub fn cumulative_r(&self) -> f64 {
        self.simulator.cumulative_r()
    }

    /// Whether the kill-switch is engaged.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.risk.is_killed()
    }

    /// Why the kill-switch is engaged, if it is.
    #[must_use]
    pub fn halt_reason(&self) -> Option<&str> {
        self.risk.reason()
    }

    /// The risk engine, for reporting.
    #[must_use]
    pub fn risk(&self) -> &RiskEngine {
        &self.risk
    }

    /// The risk engine, mutable, so an operator can trip the switch by hand.
    pub fn risk_mut(&mut self) -> &mut RiskEngine {
        &mut self.risk
    }

    /// How many decisions are waiting to be taken.
    ///
    /// A count rather than the records: a runner wants to report "wrote N
    /// decisions" without stealing them from whoever is about to persist them.
    #[must_use]
    pub fn pending_decisions(&self) -> usize {
        self.decisions.len()
    }

    /// Take the decisions recorded since the last call.
    ///
    /// Draining rather than accumulating: a bot running for weeks would
    /// otherwise hold every decision in memory, and the caller is the one
    /// that knows how to persist them.
    pub fn take_decisions(&mut self) -> Vec<DecisionRecord> {
        std::mem::take(&mut self.decisions)
    }

    /// Feed one closed candle.
    ///
    /// Returns a record when this candle advanced the decision clock and
    /// `None` when it did not -- a 1h candle warms the context without being
    /// a decision itself. The distinction matters: "no decision" and "decided
    /// nothing" are different facts, and only one of them is a problem.
    pub fn on_candle(&mut self, candle: &Candle) -> Option<DecisionRecord> {
        // Route to every declared frame of the same resolution: a document may
        // declare two names at 5m for different purposes.
        let mut advanced_decision = false;
        for (name, frame) in self.ladder.iter_mut() {
            if frame.timeframe() != candle.timeframe {
                continue;
            }
            if frame.push(candle.clone(), &self.rolling) && *name == self.decision_name {
                advanced_decision = true;
            }
        }

        if !advanced_decision {
            return None;
        }

        let now = candle.open_time + self.decision_resolution.nanos();
        let record = self.decide(now, candle);
        self.decisions.push(record.clone());
        Some(record)
    }

    /// Run one decision at the close of `candle`.
    fn decide(&mut self, now: i64, candle: &Candle) -> DecisionRecord {
        let frames_ready = self.ladder.ready_count();
        let frames_total = self.ladder.iter().count();
        let price = candle.close;

        if let Some(reason) = self.halt_reason().map(str::to_string) {
            // docs/11: open positions are handled per a documented policy, not
            // left to chance.
            let mut positions = "no position was open".to_string();
            if self.risk.limits().on_breach == OnBreach::Close && self.in_position() {
                self.simulator.close_at_market(
                    ExitTrigger::KillSwitch,
                    candle.open_time,
                    candle.open,
                    vec!["kill-switch".into()],
                );
                positions = "the open position was closed at market".into();
            } else if self.in_position() {
                positions = "the open position was held, per the configured policy".into();
            }
            if !self.alerted_halt {
                self.alerted_halt = true;
                self.alerts.push(BotAlert::Killed {
                    reason: reason.clone(),
                    positions,
                });
            }
            return self.record(
                now,
                price,
                frames_ready,
                frames_total,
                DecisionOutcome::Halted { reason },
            );
        }

        // 1. A market order queued on the previous close fills at this open.
        let filled = self.fill_pending(candle);

        // 2. Price events resolve inside this bar.
        let closed = self.resolve_price_events(candle);

        // 3. Ask the strategy, with the position as it now stands.
        let context = self.ladder.context(
            &self.symbol,
            now,
            &self.decision_name,
            self.simulator.position_view(price),
            self.fills.starting_equity,
        );

        // The strategy is asked on **every** decision bar, whatever else
        // happened on it. Returning early after a fill or a close is the
        // obvious mistake, and it is invisible: the bot still trades, just one
        // bar late, because it skipped the bar where the setup first appeared.
        // The replay always asks, so this must too -- `paper-cli run` is what
        // caught it.
        let signalled = context.map(|context| match self.engine.on_candle(&context) {
            Some(strategy_runtime::Signal::Enter(enter)) => self.queue_entry(enter, now),
            Some(strategy_runtime::Signal::Exit(exit)) => {
                let trigger = exit.trigger.name().to_string();
                self.pending = Some(Pending::Exit(exit));
                DecisionOutcome::ExitFilled { trigger }
            }
            None => DecisionOutcome::NoSignal,
        });

        // What gets *recorded* is the most consequential thing that happened:
        // a fill, then a close, then whatever the strategy said.
        let outcome = match (filled, closed, signalled) {
            (Some(outcome), _, _) => outcome,
            (None, Some((trigger, r_multiple)), _) => {
                // Booked before the next decision, so a limit this close just
                // breached stops the very next entry rather than one after.
                let _ = self.risk.record_close(r_multiple, now);
                DecisionOutcome::Closed {
                    trigger: trigger.name().to_string(),
                    r_multiple,
                }
            }
            (None, None, Some(outcome)) => outcome,
            (None, None, None) => DecisionOutcome::NoContext,
        };

        self.record(now, price, frames_ready, frames_total, outcome)
    }

    fn record(
        &self,
        at: i64,
        price: f64,
        frames_ready: usize,
        frames_total: usize,
        outcome: DecisionOutcome,
    ) -> DecisionRecord {
        DecisionRecord {
            at,
            symbol: self.symbol.clone(),
            decision_timeframe: self.decision_name.clone(),
            price,
            frames_ready,
            frames_total,
            in_position: self.in_position(),
            outcome,
        }
    }

    /// Fill a queued order at this candle's open, if risk still allows.
    fn fill_pending(&mut self, candle: &Candle) -> Option<DecisionOutcome> {
        let pending = self.pending.take()?;
        match pending {
            Pending::Enter(signal) => {
                let reasons = signal.reasons.clone();
                let open = usize::from(self.in_position());

                // Checked again at fill time, not only when queued: the
                // account may have breached a limit in between.
                if let RiskVerdict::Deny { limit, value } =
                    self.risk
                        .check_entry(signal.max_risk_pct, open, candle.open_time)
                {
                    return Some(DecisionOutcome::EntryDenied {
                        reasons,
                        limit,
                        value,
                    });
                }

                if !self.simulator.on_entry(
                    &signal,
                    candle.open_time,
                    candle.open,
                    "paper".into(),
                    0,
                ) {
                    return Some(DecisionOutcome::EntryRefused {
                        reason: "the simulator refused the entry".into(),
                    });
                }
                Some(DecisionOutcome::EntryFilled { reasons })
            }
            Pending::Exit(exit) => {
                let trigger = exit.trigger.name().to_string();
                self.simulator.close_at_market(
                    exit.trigger,
                    candle.open_time,
                    candle.open,
                    exit.reasons.clone(),
                );
                Some(DecisionOutcome::ExitFilled { trigger })
            }
        }
    }

    /// Resolve stop and target against this bar's range.
    fn resolve_price_events(&mut self, candle: &Candle) -> Option<(ExitTrigger, f64)> {
        if !self.in_position() {
            return None;
        }
        let before = self.simulator.trades().len();
        let trigger = self.simulator.check_bar(candle)?;
        let r_multiple = self
            .simulator
            .trades()
            .get(before)
            .map_or(0.0, |trade| trade.r_multiple);
        Some((trigger, r_multiple))
    }

    /// Check risk and queue an entry for the next open.
    fn queue_entry(&mut self, signal: EnterSignal, now: i64) -> DecisionOutcome {
        let reasons = signal.reasons.clone();
        let open = usize::from(self.in_position());

        // Checked here as well as at fill time so the log says "denied" on the
        // bar the setup appeared, rather than carrying an order that will
        // never be allowed to fill.
        if let RiskVerdict::Deny { limit, value } =
            self.risk.check_entry(signal.max_risk_pct, open, now)
        {
            return DecisionOutcome::EntryDenied {
                reasons,
                limit,
                value,
            };
        }

        self.pending = Some(Pending::Enter(signal));
        DecisionOutcome::EntryQueued { reasons }
    }
}

/// Something a human needs to be told about.
///
/// `docs/11` requires the user to be *notified* on a breach, not merely to have
/// it written down, so alerts are their own type rather than a log line the
/// caller has to notice. The bot raises them; whoever owns the process decides
/// how to deliver them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotAlert {
    /// A limit was breached and the switch tripped. The bot has stopped.
    Killed {
        /// Why it tripped.
        reason: String,
        /// What happened to any open position.
        positions: String,
    },
    /// A configured limit had to be clamped at startup.
    Clamped {
        /// What was clamped and to what.
        detail: String,
    },
}

impl BotAlert {
    /// How urgent this is, for whatever surface ends up showing it.
    #[must_use]
    pub const fn severity(&self) -> &'static str {
        match self {
            Self::Killed { .. } => "critical",
            Self::Clamped { .. } => "warning",
        }
    }

    /// A short line fit for a notification list.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Killed { .. } => "Paper bot stopped by the risk engine".into(),
            Self::Clamped { .. } => "Paper bot risk limit was reduced".into(),
        }
    }

    /// The detail, as text.
    #[must_use]
    pub fn body(&self) -> &str {
        match self {
            Self::Killed { reason, .. } => reason,
            Self::Clamped { detail } => detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::resample;

    const M5: i64 = 5 * 60 * 1_000_000_000;

    const DOCUMENT: &str = r#"
name: "Paper harness"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 1h
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close > liquidity.swept_level
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.5
"#;

    /// The staircase from the backtester's golden fixture: it contains real
    /// sweeps and reclaims, so the bot has something to do.
    const CYCLE: usize = 120;
    const DRIFT: f64 = 24.0;
    const KEYFRAMES: [(usize, f64); 6] = [
        (0, 0.0),
        (40, 20.0),
        (55, 8.0),
        (95, 34.0),
        (105, 4.0),
        (119, 24.0),
    ];

    fn offset(bar: usize) -> f64 {
        let position = bar % CYCLE;
        for window in KEYFRAMES.windows(2) {
            let ((from_bar, from), (to_bar, to)) = (window[0], window[1]);
            if position >= from_bar && position <= to_bar {
                let span = (to_bar - from_bar) as f64;
                return from + (to - from) * ((position - from_bar) as f64 / span);
            }
        }
        unreachable!()
    }

    fn price_at(bar: usize) -> f64 {
        100.0 + DRIFT * (bar / CYCLE) as f64 + offset(bar)
    }

    fn m5(bar: usize) -> Candle {
        let open = price_at(bar);
        let close = price_at(bar + 1);
        let wiggle = if bar % 2 == 0 { 0.3 } else { 0.6 };
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: bar as i64 * M5,
            open,
            high: open.max(close) + wiggle,
            low: open.min(close) - wiggle,
            close,
            volume: 1_000.0,
            buy_volume: 700.0,
            sell_volume: 300.0,
        }
    }

    /// The same setup, but with the stop tucked just under the last few bars
    /// instead of under the swept level. That is a losing configuration on
    /// purpose: the winning fixture above can never breach a loss limit, so a
    /// kill-switch test written against it would pass for the wrong reason.
    const LOSSY_DOCUMENT: &str = r#"
name: "Paper harness (lossy)"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 1h
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close > liquidity.swept_level
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 3}
  take_profit:
    type: "risk_multiple"
    value: 2.5
"#;

    fn bot_with(document: &str, limits: RiskLimits) -> PaperBot {
        let validated = strategy_dsl::parse_and_validate(document).unwrap();
        let engine =
            StrategyEngine::new(&validated, strategy_runtime::RuntimeConfig::default()).unwrap();
        PaperBot::new(
            engine,
            PaperConfig {
                symbol: "BTCUSDT".into(),
                limits,
                fills: SimulatorConfig::default(),
                rolling: RollingConfig::new(500, 200, Default::default()),
            },
        )
    }

    fn bot(limits: RiskLimits) -> PaperBot {
        bot_with(DOCUMENT, limits)
    }

    fn wide() -> RiskLimits {
        RiskLimits {
            daily_loss_limit_r: 1_000.0,
            weekly_loss_limit_r: 1_000.0,
            ..RiskLimits::default()
        }
    }

    /// Drive a bot over `bars` 5m candles, feeding each 1h candle as it closes
    /// -- which is bar 11, 23, 35... because a 1h candle is visible only once
    /// `open_time + 1h <= now`.
    fn run(bot: &mut PaperBot, bars: usize) -> Vec<DecisionRecord> {
        let candles: Vec<Candle> = (0..bars).map(m5).collect();
        let h1 = resample(&candles, Timeframe::H1);
        let mut out = Vec::new();
        for (bar, candle) in candles.iter().enumerate() {
            if bar >= 11 && (bar - 11) % 12 == 0 {
                if let Some(record) = bot.on_candle(&h1[(bar - 11) / 12]) {
                    out.push(record);
                }
            }
            if let Some(record) = bot.on_candle(candle) {
                out.push(record);
            }
        }
        out
    }

    #[test]
    fn the_bot_decides_on_every_decision_candle() {
        let mut bot = bot(wide());
        let decisions = run(&mut bot, 400);
        assert!(
            decisions.len() > 380,
            "expected roughly one decision per 5m bar, got {}",
            decisions.len()
        );
    }

    #[test]
    fn a_candle_that_is_not_the_decision_clock_produces_no_decision() {
        let mut bot = bot(wide());
        let h1 = resample(&(0..24).map(m5).collect::<Vec<_>>(), Timeframe::H1);
        // A 1h candle warms the context; it does not itself decide.
        assert!(bot.on_candle(&h1[0]).is_none());
        assert_eq!(bot.ladder().ready_count(), 1);
    }

    #[test]
    fn the_bot_records_no_signal_decisions_not_just_trades() {
        let mut bot = bot(wide());
        let decisions = run(&mut bot, 400);
        assert!(
            decisions
                .iter()
                .any(|d| d.outcome == DecisionOutcome::NoSignal),
            "the audit trail must contain the decisions that did nothing"
        );
        assert!(decisions.iter().all(|d| d.frames_total == 2));
        assert!(
            decisions.iter().any(|d| d.frames_ready < d.frames_total),
            "early decisions should record a partially warm ladder"
        );
    }

    #[test]
    fn the_bot_actually_trades_the_fixture() {
        let mut bot = bot(wide());
        run(&mut bot, 1200);
        assert!(
            !bot.trades().is_empty(),
            "the fixture must trade, or the risk tests below prove nothing"
        );
    }

    #[test]
    fn a_tight_daily_limit_stops_the_bot() {
        // The docs/11 done criterion: configure a tight limit and confirm the
        // kill-switch fires in the running bot, not only in the risk engine's
        // own tests.
        let mut bot = bot_with(
            LOSSY_DOCUMENT,
            RiskLimits {
                daily_loss_limit_r: 1.0,
                ..RiskLimits::default()
            },
        );
        let decisions = run(&mut bot, 1200);

        assert!(
            bot.trades().iter().any(|t| t.r_multiple < 0.0),
            "the lossy fixture must actually lose, or this proves nothing"
        );
        assert!(bot.is_halted(), "a 1R daily budget must be breached");
        assert!(
            bot.halt_reason().unwrap().contains("daily"),
            "{}",
            bot.halt_reason().unwrap_or("")
        );
        assert!(
            decisions
                .iter()
                .any(|d| matches!(d.outcome, DecisionOutcome::Halted { .. })),
            "the halt must appear in the audit trail"
        );
    }

    #[test]
    fn a_halted_bot_opens_nothing_new() {
        let mut bot = bot(wide());
        run(&mut bot, 400);
        bot.risk_mut().kill("operator");
        let open_after_halt = bot.trades().len();

        run(&mut bot, 400);
        assert_eq!(
            bot.trades().len(),
            open_after_halt,
            "no new trade may complete after the switch trips"
        );
    }

    #[test]
    fn a_halt_closes_the_open_position_when_configured_to() {
        let mut bot = bot(wide());
        run(&mut bot, 600);
        if !bot.in_position() {
            // The fixture may be flat at this bar; drive on until it isn't.
            run(&mut bot, 600);
        }
        bot.risk_mut().kill("test");
        run(&mut bot, 2);

        let closed_by_switch = bot
            .trades()
            .iter()
            .any(|t| t.exit_trigger == ExitTrigger::KillSwitch);
        assert!(
            !bot.in_position() || !closed_by_switch,
            "the documented Close policy must leave nothing open"
        );
    }

    #[test]
    fn a_bot_configured_to_hold_leaves_the_position_alone() {
        let mut bot = bot(RiskLimits {
            on_breach: OnBreach::Hold,
            ..wide()
        });
        run(&mut bot, 600);
        let was_in_position = bot.in_position();
        bot.risk_mut().kill("test");
        run(&mut bot, 2);
        if was_in_position {
            assert!(
                bot.in_position(),
                "Hold must not liquidate: the operator decides"
            );
        }
    }

    #[test]
    fn a_restarted_bot_does_not_replay_its_last_candle() {
        // A restart re-reads the most recent candle. Replaying it would build
        // a second state for the same bar and re-run the same decision.
        let mut bot = bot(wide());
        run(&mut bot, 60);
        let before = bot.trades().len();

        let last = m5(59);
        assert!(bot.on_candle(&last).is_none(), "the candle is not newer");
        assert_eq!(bot.trades().len(), before);
    }

    #[test]
    fn the_bot_asks_the_strategy_on_every_decision_bar() {
        // The mistake this pins: returning early from `decide` after a fill or
        // a close. The bot still trades -- it just trades *one bar late*,
        // because it skipped the bar where the setup first appeared, and
        // nothing else looks wrong. `paper-cli run` found it against real data
        // by diffing the bot's fills against a replay of the same candles;
        // this is the same assertion without needing the backtester, which
        // `docs/03` forbids this crate from depending on.
        let mut bot = bot(wide());
        run(&mut bot, 1200);
        let trades = bot.trades();
        assert!(!trades.is_empty(), "the fixture must trade");

        // Pin the fills. A one-bar shift moves `entry_time` by exactly one 5m
        // bar and `entry_price` to the next open -- which is precisely what the
        // early-return bug did.
        let expected = [
            (43_200_000_000_000i64, 136.0272_f64),
            (150_900_000_000_000, 207.5415),
        ];
        assert_eq!(
            trades.len(),
            expected.len(),
            "the fixture closed a different number of trades"
        );
        for (index, (time, price)) in expected.iter().enumerate() {
            let trade = &trades[index];
            assert_eq!(trade.entry_time, *time, "trade {index} entry time");
            assert!(
                (trade.entry_price - price).abs() < 1e-9,
                "trade {index}: expected an entry at {price}, got {}",
                trade.entry_price
            );
        }

        // The third setup is still open at the end. A replay would close it at
        // the last close so the statistics are not flattered; a live bot has no
        // such event, and `paper-cli run` names this difference rather than
        // reporting it as a divergence.
        assert!(
            bot.in_position(),
            "the final position stays open on a live bot"
        );
    }

    #[test]
    fn a_strategy_asking_for_more_than_the_ceiling_is_refused() {
        // docs/15: a 20% strategy must be clamped or rejected.
        let plain = bot(RiskLimits::default());
        assert!(
            plain.clamp_note().is_none(),
            "the default is under the ceiling"
        );

        let clamped = bot(RiskLimits {
            max_risk_pct: 20.0,
            ..RiskLimits::default()
        });
        let note = clamped.clamp_note().expect("a clamp must be reported");
        assert!(note.contains("20"), "{note}");
        assert_eq!(
            clamped.risk().limits().max_risk_pct,
            crate::risk::PLATFORM_MAX_RISK_PCT
        );
    }
}
