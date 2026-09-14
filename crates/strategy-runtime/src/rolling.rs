//! Rolling per-timeframe windows: the one component that decides what a
//! strategy is allowed to see.
//!
//! ## Why this lives in the runtime and not in the backtester
//!
//! `docs/11` requires the paper trader to feed each closed candle into the
//! strategy "exactly as the backtester does". The only way that stays true is
//! if both drive the *same* code. Left to two implementations they drift, and
//! the drift is invisible: a strategy would pass a backtest and then trade
//! differently live, with nothing in the trade log to explain why.
//!
//! So the windowing is here, and both the backtester's replay and the paper
//! trader's live loop call [`RollingTimeframe::push`].
//!
//! ## Visibility is enforced by construction
//!
//! A [`RollingTimeframe`] only ever holds candles that have been handed to it,
//! oldest first, and [`RollingTimeframe::view`] can only describe the newest
//! of them. There is no API that reaches a future bar, which is what makes
//! look-ahead impossible rather than merely discouraged.

use std::collections::BTreeMap;

use analytics_core::types::{Candle, Timeframe};
use analytics_core::{build_market_state, MarketState, MarketStateConfig};

use crate::context::{MarketContext, PositionView, TimeframeView};

/// How much history to retain and over what window to build state.
#[derive(Debug, Clone, PartialEq)]
pub struct RollingConfig {
    /// Closed candles each view retains.
    ///
    /// This is the bound `new_low(n)` is defined against, so it must agree with
    /// the runtime's own [`RuntimeConfig::max_history`] or a lookback
    /// condition silently changes meaning.
    ///
    /// [`RuntimeConfig::max_history`]: crate::RuntimeConfig::max_history
    pub max_history: usize,
    /// Trailing candles the volume profile and market structure are computed
    /// over. Bounds the cost of each state build.
    pub state_window: usize,
    /// Tuning for the per-timeframe `MarketState`.
    pub state: MarketStateConfig,
}

impl Default for RollingConfig {
    fn default() -> Self {
        Self {
            max_history: crate::RuntimeConfig::default().max_history,
            state_window: 500,
            state: MarketStateConfig::default(),
        }
    }
}

impl RollingConfig {
    /// A config with explicit history and state windows.
    #[must_use]
    pub fn new(max_history: usize, state_window: usize, state: MarketStateConfig) -> Self {
        Self {
            max_history,
            state_window,
            state,
        }
    }
}

/// One declared timeframe, advanced one closed candle at a time.
#[derive(Debug, Clone)]
pub struct RollingTimeframe {
    name: String,
    timeframe: Timeframe,
    history: Vec<Candle>,
    candle: Option<Candle>,
    state: Option<MarketState>,
    previous: Option<MarketState>,
}

impl RollingTimeframe {
    /// An empty window for a declared timeframe name.
    #[must_use]
    pub fn new(name: impl Into<String>, timeframe: Timeframe) -> Self {
        Self {
            name: name.into(),
            timeframe,
            history: Vec::new(),
            candle: None,
            state: None,
            previous: None,
        }
    }

    /// The declared name this window belongs to.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The resolution this window tracks.
    #[must_use]
    pub const fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    /// The newest closed candle.
    #[must_use]
    pub fn candle(&self) -> Option<&Candle> {
        self.candle.as_ref()
    }

    /// The state for the window ending at the newest candle.
    #[must_use]
    pub fn state(&self) -> Option<&MarketState> {
        self.state.as_ref()
    }

    /// Every retained candle, oldest first.
    #[must_use]
    pub fn history(&self) -> &[Candle] {
        &self.history
    }

    /// Whether this window has a candle to decide on yet.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.candle.is_some()
    }

    /// Add one closed candle.
    ///
    /// Returns `false` without changing anything when the candle is not newer
    /// than the newest one held. That matters more live than in replay: a
    /// restarted paper bot re-reads its last candle, and replaying it would
    /// build a second identical state and re-run the same decision.
    pub fn push(&mut self, candle: Candle, config: &RollingConfig) -> bool {
        if let Some(current) = &self.candle {
            if candle.open_time <= current.open_time {
                return false;
            }
        }

        // The state we were holding described the previous candle.
        self.previous = self.state.take();
        self.history.push(candle.clone());
        self.candle = Some(candle);

        // Trim lazily: draining on every bar would move the whole buffer each
        // time. `max_history` is therefore a floor on the retained window, not
        // an exact cap.
        let max_history = config.max_history.max(1);
        if self.history.len() > max_history * 2 {
            let excess = self.history.len() - max_history;
            self.history.drain(..excess);
        }

        self.state = self.build_state(config);
        true
    }

    fn build_state(&self, config: &RollingConfig) -> Option<MarketState> {
        let start = self
            .history
            .len()
            .saturating_sub(config.state_window.max(1));
        build_market_state(&self.history[start..], &[], &config.state)
    }

    /// Materialize the view handed to the strategy.
    #[must_use]
    pub fn view(&self) -> Option<TimeframeView> {
        let candle = self.candle.clone()?;
        let state = self.state.clone()?;

        // The previous view exists only so `crosses_above` has a bar to compare
        // against, so it carries no history: the grammar cannot express a
        // lookback inside `crosses_above` (both operands are numbers).
        let previous = match (&self.previous, self.history.len().checked_sub(2)) {
            (Some(state), Some(index)) => self.history.get(index).map(|candle| {
                Box::new(TimeframeView {
                    name: self.name.clone(),
                    timeframe: self.timeframe,
                    candle: candle.clone(),
                    state: state.clone(),
                    previous: None,
                    history: Vec::new(),
                })
            }),
            _ => None,
        };

        Some(TimeframeView {
            name: self.name.clone(),
            timeframe: self.timeframe,
            candle,
            state,
            previous,
            history: self.history.clone(),
        })
    }
}

/// Every declared timeframe, keyed by the name the document used.
#[derive(Debug, Clone, Default)]
pub struct RollingLadder {
    frames: BTreeMap<String, RollingTimeframe>,
}

impl RollingLadder {
    /// A ladder from a document's declared timeframes.
    #[must_use]
    pub fn new(timeframes: &BTreeMap<String, Timeframe>) -> Self {
        Self {
            frames: timeframes
                .iter()
                .map(|(name, tf)| (name.clone(), RollingTimeframe::new(name.clone(), *tf)))
                .collect(),
        }
    }

    /// The window for one declared name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&RollingTimeframe> {
        self.frames.get(name)
    }

    /// The window for one declared name, mutable.
    #[must_use]
    pub fn get_mut(&mut self, name: &str) -> Option<&mut RollingTimeframe> {
        self.frames.get_mut(name)
    }

    /// Iterate the windows in declared-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &RollingTimeframe)> {
        self.frames.iter()
    }

    /// Iterate the windows mutably, in declared-name order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut RollingTimeframe)> {
        self.frames.iter_mut()
    }

    /// Whether every declared window has produced a candle yet.
    ///
    /// This is the "warm-up finished" indicator, not a gate on deciding: see
    /// [`RollingLadder::context`].
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.frames.values().all(RollingTimeframe::is_ready)
    }

    /// How many declared windows have produced a candle.
    ///
    /// Recorded alongside every decision so an audit trail can show that a
    /// setup did not fire because half its context was still warming up,
    /// rather than because the market disagreed.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.frames.values().filter(|f| f.is_ready()).count()
    }

    /// Build the context for a decision, or `None` if one cannot be built.
    ///
    /// Only the **decision** window must be warm. A coarser window that has
    /// not closed yet is simply omitted, and the engine treats a condition
    /// written against a missing view as not fired -- which is the correct
    /// answer: at the first 5m close of a run there *is* no 1h candle to
    /// evaluate.
    ///
    /// Gating on [`RollingLadder::is_ready`] instead would look safer but
    /// would be a second, subtler rule: `any_of` would lose its chance to
    /// fire on the frames that *are* ready, and the replay and the live
    /// trader would each have to implement the same exception to stay in
    /// agreement. One rule, one implementation.
    ///
    /// `decision_timeframe` must be a declared name; when it is not, this
    /// returns `None` rather than building a context the engine would reject
    /// with a hard error on every bar.
    #[must_use]
    pub fn context(
        &self,
        symbol: &str,
        now: i64,
        decision_timeframe: &str,
        position: Option<PositionView>,
        equity: f64,
    ) -> Option<MarketContext> {
        if !self.frames.contains_key(decision_timeframe) {
            return None;
        }

        let mut timeframes = BTreeMap::new();
        for (name, frame) in &self.frames {
            if let Some(view) = frame.view() {
                timeframes.insert(name.clone(), view);
            }
        }
        if !timeframes.contains_key(decision_timeframe) {
            return None;
        }

        Some(MarketContext {
            symbol: symbol.into(),
            now,
            decision_timeframe: decision_timeframe.into(),
            timeframes,
            position,
            equity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle_at(symbol: &str, timeframe: Timeframe, index: i64) -> Candle {
        let price = 100.0 + index as f64;
        Candle {
            symbol: symbol.into(),
            timeframe,
            open_time: index * timeframe.nanos(),
            open: price,
            high: price + 1.0,
            low: price - 1.0,
            close: price + 0.5,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    #[test]
    fn a_frame_only_ever_describes_the_candles_it_was_given() {
        let mut frame = RollingTimeframe::new("entry", Timeframe::M5);
        let config = RollingConfig::default();

        assert!(!frame.is_ready());
        assert!(frame.push(candle_at("BTCUSDT", Timeframe::M5, 0), &config));
        assert!(frame.is_ready());

        let view = frame.view().expect("ready");
        assert_eq!(view.candle.open_time, 0);
        assert_eq!(view.history.len(), 1);
        // No previous bar yet, so no `crosses_above` comparison is possible.
        assert!(view.previous.is_none());

        assert!(frame.push(candle_at("BTCUSDT", Timeframe::M5, 1), &config));
        let view = frame.view().expect("ready");
        assert_eq!(view.candle.open_time, Timeframe::M5.nanos());
        assert!(view.previous.is_some());
    }

    #[test]
    fn a_candle_that_is_not_newer_is_ignored() {
        // A restarted bot re-reads its last candle. Replaying it would build a
        // second state for the same bar and re-run the same decision.
        let mut frame = RollingTimeframe::new("entry", Timeframe::M5);
        let config = RollingConfig::default();

        assert!(frame.push(candle_at("BTCUSDT", Timeframe::M5, 5), &config));
        assert!(!frame.push(candle_at("BTCUSDT", Timeframe::M5, 5), &config));
        assert!(!frame.push(candle_at("BTCUSDT", Timeframe::M5, 4), &config));
        assert_eq!(frame.history().len(), 1);
    }

    #[test]
    fn history_is_bounded_by_the_config() {
        let mut frame = RollingTimeframe::new("entry", Timeframe::M5);
        let config = RollingConfig::new(10, 50, MarketStateConfig::default());

        for index in 0..200 {
            frame.push(candle_at("BTCUSDT", Timeframe::M5, index), &config);
        }
        // Trimmed lazily, so between max_history and 2x is expected.
        assert!(frame.history().len() <= 20, "got {}", frame.history().len());
        assert!(frame.history().len() >= 10, "got {}", frame.history().len());
    }

    #[test]
    fn a_ladder_is_not_ready_until_every_declared_frame_has_a_candle() {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        timeframes.insert("trend".to_string(), Timeframe::H1);

        let mut ladder = RollingLadder::new(&timeframes);
        let config = RollingConfig::default();

        assert!(!ladder.is_ready());
        assert_eq!(ladder.ready_count(), 0);
        ladder
            .get_mut("entry")
            .unwrap()
            .push(candle_at("BTCUSDT", Timeframe::M5, 1), &config);
        assert!(!ladder.is_ready(), "trend still has nothing");
        assert_eq!(ladder.ready_count(), 1);

        ladder
            .get_mut("trend")
            .unwrap()
            .push(candle_at("BTCUSDT", Timeframe::H1, 0), &config);
        assert!(ladder.is_ready());
        assert_eq!(ladder.ready_count(), 2);

        let context = ladder
            .context("BTCUSDT", 1, "entry", None, 10_000.0)
            .expect("ready");
        assert_eq!(context.decision_timeframe, "entry");
        assert_eq!(context.timeframes.len(), 2);
        assert_eq!(context.equity, 10_000.0);
    }

    #[test]
    fn a_coarse_frame_still_warming_up_is_omitted_not_fatal() {
        // At the first 5m close of a run there is no 1h candle to evaluate.
        // The engine's answer to that is "the condition did not fire", which
        // it can only reach if it is handed a context -- so the ladder must
        // build one with the warmed frames and omit the rest.
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        timeframes.insert("trend".to_string(), Timeframe::H1);

        let mut ladder = RollingLadder::new(&timeframes);
        ladder.get_mut("entry").unwrap().push(
            candle_at("BTCUSDT", Timeframe::M5, 1),
            &RollingConfig::default(),
        );

        let context = ladder
            .context("BTCUSDT", 1, "entry", None, 10_000.0)
            .expect("the decision frame is warm");
        assert_eq!(context.timeframes.len(), 1);
        assert!(context.view("trend").is_none());
        assert!(context.view("entry").is_some());
    }

    #[test]
    fn a_cold_decision_frame_produces_no_context() {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        timeframes.insert("trend".to_string(), Timeframe::H1);

        let mut ladder = RollingLadder::new(&timeframes);
        // Only the *coarse* frame has a candle. The decision clock has not
        // ticked, so there is nothing to decide on.
        ladder.get_mut("trend").unwrap().push(
            candle_at("BTCUSDT", Timeframe::H1, 0),
            &RollingConfig::default(),
        );

        assert!(ladder
            .context("BTCUSDT", 1, "entry", None, 10_000.0)
            .is_none());
    }

    #[test]
    fn an_undeclared_decision_timeframe_produces_no_context() {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), Timeframe::M5);
        let mut ladder = RollingLadder::new(&timeframes);
        ladder.get_mut("entry").unwrap().push(
            candle_at("BTCUSDT", Timeframe::M5, 1),
            &RollingConfig::default(),
        );

        assert!(ladder
            .context("BTCUSDT", 1, "typo", None, 10_000.0)
            .is_none());
    }
}
