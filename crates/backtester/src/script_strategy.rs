//! Pine-lite scripts as a [`Strategy`] source (`docs/23` Phase 7).
//!
//! The adapter wraps a vetted script and a rolling window of candles: on every
//! `on_candle` it runs the script over the visible window, lowers the
//! recorded `strategy.*` intents into the same [`Signal`]s the DSL engine
//! emits, and lets the replay's existing simulator handle fills, stops and
//! reports -- so the backtester cannot tell a script strategy from a document
//! one.
//!
//! Pine's fill rule is already the simulator's rule (decide on a closed bar,
//! fill at the next open, slippage on top), so no second fill model exists.

use std::collections::BTreeMap;

use analytics_core::types::{Candle, Timeframe};
use strategy_runtime::signal::{EnterSignal, ExitSignal, ExitTrigger, Signal};
use strategy_dsl::Direction;
use strategy_runtime::Strategy;

use pine_lite::{run as run_script, Inputs, Output};

/// A vetted Pine-lite script used as a trading strategy.
pub struct ScriptStrategy {
    /// The parsed script (the output of `pine_lite::vet`).
    script: pine_lite::parse::Script,
    /// Host-supplied input values.
    inputs: Inputs,
    /// Maximum `strategy.entry` positions held at once (`pyramiding=0` means
    /// one). v1 keeps a single position, like the DSL engine.
    direction: Option<Direction>,
    /// The stop the script declared with its most recent entry, carried so an
    /// `strategy.exit(stop=...)` recorded later still prices correctly.
    last_stop: Option<f64>,
    /// Candles seen so far, oldest first: the script's history window.
    window: Vec<Candle>,
    /// How much history to keep -- `max_bars_back` from the header plus slack.
    max_window: usize,
}

impl ScriptStrategy {
    /// Vet the source and build the strategy. Fails with the vetting error
    /// list rendered; a script that does not vet never becomes a strategy.
    pub fn new(source: &str) -> Result<Self, String> {
        let (header, script) = pine_lite::vet(source).map_err(|errs| {
            errs.iter()
                .map(|e| format!("line {}: {}", e.span.line, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        Ok(Self {
            script,
            inputs: Inputs::default(),
            direction: None,
            last_stop: None,
            window: Vec::new(),
            max_window: (header.max_bars_back + 8).max(64),
        })
    }

    /// Supply host input values for the script's `input.*` declarations.
    #[must_use]
    pub fn with_inputs(mut self, inputs: Inputs) -> Self {
        self.inputs = inputs;
        self
    }
}

/// Lower one run's `strategy.*` intents into the replay's signal vocabulary.
///
/// The intents arrive with the bar they fired on; only the last bar's intents
/// matter for this decision -- earlier bars' intents were already lowered on
/// their own `on_candle` calls.
#[must_use]
pub fn intents_to_signal(
    output: &Output,
    bar: usize,
    candles: &[Candle],
    in_position: bool,
    last_stop: &mut Option<f64>,
) -> Option<Signal> {
    let intents: Vec<&(usize, pine_lite::interp::Intent)> = output
        .intents
        .iter()
        .filter(|(b, _)| *b == bar)
        .collect();
    // Empty means "no decision this bar" -- the overwhelmingly common case.
    if intents.is_empty() {
        return None;
    }
    // Pine order: later calls win, and an exit/close cancels a same-bar entry
    // (the engine's own one-decision-per-bar rule keeps this simple).
    let mut close = false;
    let mut entry_long: Option<bool> = None;
    for (_, intent) in intents {
        match intent {
            pine_lite::interp::Intent::Close { .. } => close = true,
            pine_lite::interp::Intent::Exit { stop, limit, .. } => {
                // docs/24 S4: a stop-carrying `strategy.exit` ARMS a standing
                // stop (the VM's simulator fills it intrabar when price
                // touches) -- it is not a market exit. Only a naked exit (no
                // stop, no limit) closes at market, mirroring the VM's
                // `Intent::Exit { stop: None }` handling. The replay carries
                // the stop as declared at entry; RE-ARMING it while the
                // position is open (a trailing stop) is the replay's v2 axis
                // and stays refused as bracket id-theft (docs/24).
                if stop.is_some() || limit.is_some() {
                    *last_stop = *stop;
                } else {
                    *last_stop = None;
                    close = true;
                }
            }
            pine_lite::interp::Intent::Entry { long, .. } => entry_long = Some(*long),
        }
    }
    let _ = entry_long;
    let candle = &candles[bar];
    if close && in_position {
        return Some(Signal::Exit(ExitSignal::from_conditions(
            ExitTrigger::ExitCondition,
            vec!["strategy.close".to_string()],
        )));
    }
    // An entry while already in a position is refused, not silently ignored --
    // pyramiding is a v2 axis (docs/23).
    if !in_position {
        if let Some(long) = entry_long {
            let direction = if long { Direction::Long } else { Direction::Short };
            // The stop: the script's own `strategy.exit(stop=)` recorded on
            // this bar, or a default of `2 ATR(14)` below/above the reference
            // -- which is what the DSL's `atr` stop kind resolves to.
            // The undeclared stop: 2 × ATR(14) from the decision close —
            // the rule now lives in pine_lite::sim so the VM's simulation
            // and this lowering share one definition (docs/24 S1).
            let stop = last_stop.unwrap_or_else(|| {
                let long = direction == Direction::Long;
                pine_lite::sim::default_stop(candles, bar, long)
                    .unwrap_or_else(|| candle.close * if long { 0.98 } else { 1.02 })
            });
            let signal = EnterSignal {
                direction,
                reference_price: candle.close,
                stop_price: stop,
                take_profit_price: None,
                max_risk_pct: 1.0,
                reasons: vec!["script strategy.entry".to_string()],
            };
            if signal.stop_is_valid() {
                return Some(Signal::Enter(signal));
            }
            // An invalid stop is a malformed trade: no signal, not a bad one.
            return None;
        }
    }
    None
}

impl Strategy for ScriptStrategy {
    fn on_candle(&mut self, ctx: &strategy_runtime::context::MarketContext) -> Option<Signal> {
        // The engine hands us the whole visible context; the decision
        // timeframe's view is where the script's candles come from.
        let view = ctx.timeframes.get("entry")?;
        self.window.push(view.candle.clone());
        if self.window.len() > self.max_window {
            self.window.remove(0);
        }
        let bar = self.window.len() - 1;
        let output = run_script(&self.script, &self.window, &self.inputs).ok()?;
        // Position state: the replay drives us, and tells us through the
        // signal it accepted -- but the trait has no position read, so track
        // it locally from our own decisions.
        if let Some(signal) = intents_to_signal(
            &output,
            bar,
            &self.window,
            self.direction.is_some(),
            &mut self.last_stop,
        ) {
            match &signal {
                Signal::Enter(enter) => {
                    self.direction = Some(enter.direction);
                    self.last_stop = Some(enter.stop_price);
                }
                Signal::Exit(_) => {
                    self.direction = None;
                    self.last_stop = None;
                }
            }
            return Some(signal);
        }
        None
    }
}

/// Build the replay input for a script strategy: one timeframe, the script's
/// own window, named `entry` -- the name the engine's single-timeframe
/// documents declare.
#[must_use]
pub fn script_replay_input(candles: &[Candle], timeframe: Timeframe) -> BTreeMap<String, Vec<Candle>> {
    let mut map = BTreeMap::new();
    map.insert("entry".to_string(), candles.to_vec());
    let _ = timeframe;
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(i: usize, close: f64) -> Candle {
        Candle {
            symbol: "T".into(),
            timeframe: Timeframe::M1,
            open_time: i as i64 * 60_000_000_000,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
            buy_volume: 5.0,
            sell_volume: 5.0,
        }
    }

    #[test]
    fn an_entry_intent_becomes_an_enter_signal() {
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0 + ((i % 9) as f64))).collect();
        let src = concat!(
            "//@pine_lite version=1\n",
            "up = ta.crossover(close, ta.sma(close, 5))\n",
            "if up\n",
            "    strategy.entry(\"long\", direction=\"long\")\n",
            "plot(close)\n",
        );
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let output = run_script(&parsed, &candles, &Inputs::default()).expect("run");
        // Bar 21 is where the crossover fires (see the fixture: closes 100
        // ..108 cycling with period 9; close crosses its sma(5) there).
        let mut last_stop = None;
        let signal = intents_to_signal(&output, 21, &candles, false, &mut last_stop);
        assert!(matches!(signal, Some(Signal::Enter(_))), "{signal:?}");
    }

    #[test]
    fn a_bars_signal_never_becomes_a_trade() {
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0 + ((i % 9) as f64))).collect();
        let src = concat!(
            "//@pine_lite version=1\n",
            "plot(ta.rsi(close, 14))\n",
        );
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let output = run_script(&parsed, &candles, &Inputs::default()).expect("run");
        let mut last_stop = None;
        let signal = intents_to_signal(&output, 29, &candles, false, &mut last_stop);
        assert!(signal.is_none());
    }

    #[test]
    fn a_script_strategy_is_a_strategy() {
        // The trait bound is the whole Phase 7 contract: a vetted script can
        // drive `replay` exactly where a validated document can.
        fn assert_strategy<S: Strategy>(_: &S) {}
        let src = "//@pine_lite version=1\nplot(close)\n";
        let strategy = ScriptStrategy::new(src).expect("strategy");
        assert_strategy(&strategy);
    }
}
