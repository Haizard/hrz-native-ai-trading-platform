//! Order, position and PnL simulation (`docs/07-BACKTESTING-ENGINE.md`, `docs/11-BOT-TRADING-ENGINE.md`).
//!
//! ## Why this lives in the runtime and not in the backtester
//!
//! `docs/11` requires the paper trader to simulate fills the same way a replay
//! does, and `docs/03` forbids `trading-engine` from depending on `backtester`.
//! The only way to satisfy both is for the fill model to sit below both of
//! them -- here, next to the windowing in [`crate::rolling`].
//!
//! Left as two implementations, the two would drift, and the drift would be
//! invisible: paper results would disagree with the backtest that approved the
//! strategy, with nothing in either to explain the gap.
//!
//! ## Two kinds of exit, and why they live in different places
//!
//! * **Price events** -- stop and target -- are resolved *here*, against the
//!   candle's high and low. The strategy never sees them; it decides from
//!   closed candles only.
//! * **Condition events** -- invalidation and the exit block -- come from the
//!   engine, are decided on a *closed* candle, and therefore fill at the next
//!   candle's open, exactly like an entry.
//!
//! Keeping that split means this module needs no notion of what a strategy
//! "meant", and the engine needs no notion of an intra-bar price path.
//!
//! ## Ambiguous bars resolve against the trade
//!
//! When one candle touches both the stop and the target, the OHLC data does not
//! say which came first. This simulator assumes **the stop**. That is the
//! conservative reading, and it is the only defensible default: assuming the
//! target would make every ambiguous bar a winner and inflate the reported win
//! rate in exactly the cases where the data is least informative.
//!
//! ## R, not currency
//!
//! Trade accounting is in pure R multiples. One R is the risk the trade
//! accepted when it was decided, so a trade that risks 1% and gains 2.5x its
//! stop distance is `+2.5R` regardless of account size. Phase 3 does not
//! compound, and it does not pretend to: the alternative -- a currency equity
//! curve built on a fixed notional -- looks more impressive and measures less.
//! The consequence is documented in [`FillAssumptions`] and in the report.

use crate::{EnterSignal, ExitTrigger, PositionView};
use analytics_core::types::Candle;
use serde::{Deserialize, Serialize};
use strategy_dsl::Direction;

/// Sizing and fill assumptions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SimulatorConfig {
    /// Account equity used for position sizing.
    ///
    /// Constant across the run: Phase 3 does not compound, so a fixed equity is
    /// the honest input to a fixed-fractional sizing rule.
    pub starting_equity: f64,
    /// Slippage applied to market orders, in basis points, always against the
    /// trade.
    pub slippage_bps: f64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            starting_equity: 10_000.0,
            slippage_bps: 2.0,
        }
    }
}

impl SimulatorConfig {
    /// Slippage as a fraction, e.g. `0.0002`.
    #[must_use]
    pub fn slippage(&self) -> f64 {
        self.slippage_bps / 10_000.0
    }
}

/// A position the simulator is holding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenPosition {
    /// Which way the position is.
    pub direction: Direction,
    /// When it was filled (unix nanos).
    pub entry_time: i64,
    /// Price it was filled at, after slippage.
    pub entry_price: f64,
    /// The stop the entry signal resolved.
    pub stop_price: f64,
    /// Its take-profit level, if any.
    pub take_profit_price: Option<f64>,
    /// Units held.
    pub size: f64,
    /// Risk per unit at entry, always positive.
    pub risk_per_unit: f64,
    /// The signal's reference price, kept so the log shows what the strategy saw.
    pub reference_price: f64,
    /// Labels of the conditions that opened the trade.
    pub entry_reasons: Vec<String>,
    /// Structural trend on the decision timeframe at entry.
    pub regime: String,
    /// Closed candles since the fill.
    pub bars_held: usize,
    /// Index of the decision candle the fill happened on.
    pub entry_index: usize,
}

impl OpenPosition {
    /// Open profit/loss in R multiples at `price`.
    #[must_use]
    pub fn unrealized_r(&self, price: f64) -> f64 {
        if self.risk_per_unit <= 0.0 {
            return 0.0;
        }
        let move_ = match self.direction {
            Direction::Long => price - self.entry_price,
            Direction::Short => self.entry_price - price,
        };
        move_ / self.risk_per_unit
    }

    /// The public view of this position for a [`MarketContext`].
    ///
    /// [`MarketContext`]: strategy_runtime::MarketContext
    #[must_use]
    pub fn view(&self, price: f64) -> PositionView {
        PositionView {
            direction: self.direction,
            entry_price: self.entry_price,
            entry_time: self.entry_time,
            stop_price: self.stop_price,
            take_profit_price: self.take_profit_price,
            size: self.size,
            bars_in_trade: self.bars_held,
            unrealized_r: self.unrealized_r(price),
        }
    }
}

/// Simulates one position at a time, recording every completed trade.
#[derive(Debug)]
pub struct Simulator {
    config: SimulatorConfig,
    position: Option<OpenPosition>,
    trades: Vec<TradeRecord>,
    equity_curve: Vec<f64>,
    cumulative_r: f64,
    refused: Vec<String>,
}

impl Simulator {
    /// A fresh simulator.
    #[must_use]
    pub fn new(config: SimulatorConfig) -> Self {
        Self {
            config,
            position: None,
            trades: Vec::new(),
            equity_curve: Vec::new(),
            cumulative_r: 0.0,
            refused: Vec::new(),
        }
    }

    /// The open position, if any.
    #[must_use]
    pub fn position(&self) -> Option<&OpenPosition> {
        self.position.as_ref()
    }

    /// The public view of the open position, for the context.
    #[must_use]
    pub fn position_view(&self, price: f64) -> Option<PositionView> {
        self.position.as_ref().map(|p| p.view(price))
    }

    /// Completed trades, in closing order.
    #[must_use]
    pub fn trades(&self) -> &[TradeRecord] {
        &self.trades
    }

    /// Cumulative R after each completed trade.
    #[must_use]
    pub fn equity_curve(&self) -> &[f64] {
        &self.equity_curve
    }

    /// Entries the simulator itself refused, with reasons.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refused
    }

    /// Apply slippage against the trade.
    fn fill_price(&self, market_price: f64, buying: bool) -> f64 {
        let slip = self.config.slippage();
        if buying {
            market_price * (1.0 + slip)
        } else {
            market_price * (1.0 - slip)
        }
    }

    /// Open a position from an entry signal.
    ///
    /// Returns `false` when the signal cannot be turned into a position, which
    /// can only happen if the risk distance is zero -- the engine already
    /// refuses stops on the wrong side of price.
    pub fn on_entry(
        &mut self,
        signal: &EnterSignal,
        fill_time: i64,
        market_price: f64,
        regime: String,
        index: usize,
    ) -> bool {
        if self.position.is_some() {
            self.refused
                .push("entry signal while already in a position".into());
            return false;
        }

        let risk_per_unit = signal.risk_per_unit();
        if !risk_per_unit.is_finite() || risk_per_unit <= 0.0 {
            self.refused.push(format!(
                "entry signal with zero risk distance (stop {} vs reference {})",
                signal.stop_price, signal.reference_price
            ));
            return false;
        }

        let buying = signal.direction == Direction::Long;
        let entry_price = self.fill_price(market_price, buying);

        // Fixed-fractional sizing against a constant equity. The size is
        // recorded so the log shows a realistic position, but it does not feed
        // the P&L -- see the module note on R accounting.
        let size = (self.config.starting_equity * signal.max_risk_pct / 100.0) / risk_per_unit;

        self.position = Some(OpenPosition {
            direction: signal.direction,
            entry_time: fill_time,
            entry_price,
            stop_price: signal.stop_price,
            take_profit_price: signal.take_profit_price,
            size,
            risk_per_unit,
            reference_price: signal.reference_price,
            entry_reasons: signal.reasons.clone(),
            regime,
            bars_held: 0,
            entry_index: index,
        });
        true
    }

    /// Check one candle for a stop or target touch.
    ///
    /// Returns the trigger if the position was closed. Both touches in the same
    /// candle resolve to the stop.
    pub fn check_bar(&mut self, candle: &Candle) -> Option<ExitTrigger> {
        let position = self.position.as_mut()?;
        position.bars_held += 1;

        let (stop_hit, target_hit) = match position.direction {
            Direction::Long => (
                candle.low <= position.stop_price,
                position
                    .take_profit_price
                    .is_some_and(|target| candle.high >= target),
            ),
            Direction::Short => (
                candle.high >= position.stop_price,
                position
                    .take_profit_price
                    .is_some_and(|target| candle.low <= target),
            ),
        };

        let trigger = match (stop_hit, target_hit) {
            // Ambiguous: assume the stop. Never flatter the result.
            (true, true) => ExitTrigger::Stop,
            (true, false) => ExitTrigger::Stop,
            (false, true) => ExitTrigger::Target,
            (false, false) => return None,
        };

        // The spec's documented simplification: a stop or target fills at the
        // touched price, with no slippage beyond the level itself.
        let price = match trigger {
            ExitTrigger::Stop => position.stop_price,
            _ => position.take_profit_price.unwrap_or(position.stop_price),
        };

        // Timestamped at the candle's *close*, not its open. OHLC data cannot say
        // where inside the bar the level was touched, so the bar's close is the
        // earliest moment at which the touch is known to have happened. Using the
        // open would date every intrabar exit to before the bar it occurred in --
        // and, when the stop is hit on the very bar the entry filled on, would
        // record a trade that opened and closed at the same instant.
        self.close(
            trigger,
            candle.open_time + candle.timeframe.nanos(),
            price,
            Vec::new(),
        );
        Some(trigger)
    }

    /// Close the open position.
    ///
    /// `market_price` is the pre-slippage price; slippage is applied against the
    /// trade. Price-event exits pass the touched level and `slippage: false`
    /// semantics via [`Simulator::check_bar`], which does not route through here.
    pub fn close(
        &mut self,
        trigger: ExitTrigger,
        exit_time: i64,
        price: f64,
        exit_reasons: Vec<String>,
    ) -> Option<TradeRecord> {
        let position = self.position.take()?;

        let r_multiple = position.unrealized_r(price);
        self.cumulative_r += r_multiple;
        self.equity_curve.push(self.cumulative_r);

        let record = TradeRecord {
            direction: position.direction,
            entry_time: position.entry_time,
            entry_price: position.entry_price,
            exit_time,
            exit_price: price,
            reference_price: position.reference_price,
            stop_price: position.stop_price,
            take_profit_price: position.take_profit_price,
            size: position.size,
            risk_per_unit: position.risk_per_unit,
            r_multiple,
            bars_held: position.bars_held,
            exit_trigger: trigger,
            entry_reasons: position.entry_reasons,
            exit_reasons,
            regime: position.regime,
        };

        self.trades.push(record.clone());
        Some(record)
    }

    /// Close the open position at a market price, applying slippage against the
    /// trade. Used for condition-driven exits, which fill at the next open.
    pub fn close_at_market(
        &mut self,
        trigger: ExitTrigger,
        exit_time: i64,
        market_price: f64,
        exit_reasons: Vec<String>,
    ) -> Option<TradeRecord> {
        let buying = match self.position.as_ref()?.direction {
            // Closing a long is a sell; closing a short is a buy.
            Direction::Long => false,
            Direction::Short => true,
        };
        let price = self.fill_price(market_price, buying);
        self.close(trigger, exit_time, price, exit_reasons)
    }

    /// The current cumulative R.
    #[must_use]
    pub const fn cumulative_r(&self) -> f64 {
        self.cumulative_r
    }

    /// The fill assumptions this run used, for the report.
    #[must_use]
    pub fn assumptions(&self) -> FillAssumptions {
        FillAssumptions {
            slippage_bps: self.config.slippage_bps,
            ..FillAssumptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::Timeframe;

    fn candle(open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: 0,
            open,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn long_signal(stop: f64, target: Option<f64>) -> EnterSignal {
        EnterSignal {
            direction: Direction::Long,
            reference_price: 100.0,
            stop_price: stop,
            take_profit_price: target,
            max_risk_pct: 1.0,
            reasons: vec!["test".into()],
        }
    }

    fn short_signal(stop: f64, target: Option<f64>) -> EnterSignal {
        EnterSignal {
            direction: Direction::Short,
            ..long_signal(stop, target)
        }
    }

    /// A simulator with no slippage, so arithmetic in the tests is exact.
    fn clean() -> Simulator {
        Simulator::new(SimulatorConfig {
            starting_equity: 10_000.0,
            slippage_bps: 0.0,
        })
    }

    #[test]
    fn a_long_sized_at_one_percent_risks_100_on_a_10k_account() {
        let mut sim = clean();
        // Stop 5 below entry: risking 1% of 10,000 = 100, so 20 units.
        assert!(sim.on_entry(&long_signal(95.0, None), 0, 100.0, "ranging".into(), 0));
        let position = sim.position().unwrap();
        assert!((position.size - 20.0).abs() < 1e-9);
        assert!((position.risk_per_unit - 5.0).abs() < 1e-9);
    }

    #[test]
    fn slippage_is_always_applied_against_the_trade() {
        let mut sim = Simulator::new(SimulatorConfig {
            starting_equity: 10_000.0,
            slippage_bps: 100.0, // 1%, deliberately large so it is visible
        });
        // Buying: fills above the market.
        sim.on_entry(&long_signal(95.0, None), 0, 100.0, "ranging".into(), 0);
        assert!((sim.position().unwrap().entry_price - 101.0).abs() < 1e-9);

        // Selling: fills below the market.
        let mut sim = Simulator::new(SimulatorConfig {
            starting_equity: 10_000.0,
            slippage_bps: 100.0,
        });
        sim.on_entry(&short_signal(105.0, None), 0, 100.0, "ranging".into(), 0);
        assert!((sim.position().unwrap().entry_price - 99.0).abs() < 1e-9);
    }

    #[test]
    fn a_stop_touch_closes_the_trade_at_the_stop() {
        let mut sim = clean();
        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        let trigger = sim.check_bar(&candle(100.0, 101.0, 94.0, 96.0));
        assert_eq!(trigger, Some(ExitTrigger::Stop));
        let trade = &sim.trades()[0];
        assert!((trade.exit_price - 95.0).abs() < 1e-9);
        assert!((trade.r_multiple + 1.0).abs() < 1e-9, "a stop is -1R");
    }

    #[test]
    fn an_intrabar_exit_is_never_dated_at_or_before_its_entry() {
        // The entry fills at the bar's open and the same bar then touches the
        // stop. OHLC cannot say when inside the bar that happened, so the exit is
        // dated at the bar's close. Dating it at the open would record a trade
        // that opened and closed at the same instant -- which is what the raw
        // BTCUSDT 5m run produced 36 times before this was fixed.
        let mut sim = clean();
        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );
        sim.check_bar(&candle(100.0, 101.0, 94.0, 96.0));

        let trade = &sim.trades()[0];
        assert_eq!(trade.entry_time, 0);
        assert_eq!(trade.exit_time, Timeframe::M5.nanos());
        assert!(
            trade.exit_time > trade.entry_time,
            "a trade cannot close before or as it opens"
        );
        assert_eq!(trade.holding_nanos(), Timeframe::M5.nanos());
    }

    #[test]
    fn a_target_touch_closes_the_trade_at_the_target() {
        let mut sim = clean();
        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        let trigger = sim.check_bar(&candle(100.0, 111.0, 99.0, 110.0));
        assert_eq!(trigger, Some(ExitTrigger::Target));
        let trade = &sim.trades()[0];
        assert!((trade.exit_price - 110.0).abs() < 1e-9);
        // 10 points of profit on 5 points of risk.
        assert!((trade.r_multiple - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_bar_touching_both_resolves_to_the_stop() {
        let mut sim = clean();
        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        // One candle spans both levels; OHLC cannot say which came first.
        let trigger = sim.check_bar(&candle(100.0, 111.0, 94.0, 105.0));
        assert_eq!(
            trigger,
            Some(ExitTrigger::Stop),
            "ambiguity must not flatter"
        );
        assert!((sim.trades()[0].r_multiple + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_bar_touching_neither_leaves_the_position_open() {
        let mut sim = clean();
        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        assert_eq!(sim.check_bar(&candle(100.0, 104.0, 97.0, 103.0)), None);
        assert!(sim.position().is_some());
        assert_eq!(sim.position().unwrap().bars_held, 1);
        assert!(sim.trades().is_empty());
    }

    #[test]
    fn a_short_stops_out_on_a_high_touch() {
        let mut sim = clean();
        sim.on_entry(
            &short_signal(105.0, Some(90.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        let trigger = sim.check_bar(&candle(100.0, 106.0, 99.0, 104.0));
        assert_eq!(trigger, Some(ExitTrigger::Stop));
        assert!((sim.trades()[0].r_multiple + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_short_hits_its_target_on_a_low_touch() {
        let mut sim = clean();
        sim.on_entry(
            &short_signal(105.0, Some(90.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );

        let trigger = sim.check_bar(&candle(100.0, 101.0, 89.0, 91.0));
        assert_eq!(trigger, Some(ExitTrigger::Target));
        assert!((sim.trades()[0].r_multiple - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_zero_risk_signal_is_refused_rather_than_dividing_by_zero() {
        let mut sim = clean();
        // Stop equal to the reference price: no risk distance to size against.
        let signal = long_signal(100.0, None);
        assert!(!sim.on_entry(&signal, 0, 100.0, "ranging".into(), 0));
        assert!(sim.position().is_none());
        assert!(!sim.refusals().is_empty());
    }

    #[test]
    fn a_second_entry_while_holding_is_refused() {
        let mut sim = clean();
        assert!(sim.on_entry(&long_signal(95.0, None), 0, 100.0, "ranging".into(), 0));
        assert!(!sim.on_entry(&long_signal(95.0, None), 1, 100.0, "ranging".into(), 1));
        assert_eq!(sim.refusals().len(), 1);
    }

    #[test]
    fn the_equity_curve_accumulates_r_across_trades() {
        let mut sim = clean();

        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            0,
            100.0,
            "ranging".into(),
            0,
        );
        sim.check_bar(&candle(100.0, 111.0, 99.0, 110.0)); // +2R

        sim.on_entry(
            &long_signal(95.0, Some(110.0)),
            1,
            100.0,
            "ranging".into(),
            1,
        );
        sim.check_bar(&candle(100.0, 101.0, 94.0, 96.0)); // -1R

        assert_eq!(sim.trades().len(), 2);
        assert!((sim.cumulative_r() - 1.0).abs() < 1e-9);
        assert_eq!(sim.equity_curve().len(), 2);
        assert!((sim.equity_curve()[0] - 2.0).abs() < 1e-9);
        assert!((sim.equity_curve()[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_condition_exit_fills_at_market_with_slippage_against_the_trade() {
        let mut sim = Simulator::new(SimulatorConfig {
            starting_equity: 10_000.0,
            slippage_bps: 100.0,
        });
        sim.on_entry(&long_signal(95.0, None), 0, 100.0, "ranging".into(), 0);
        assert!((sim.position().unwrap().entry_price - 101.0).abs() < 1e-9);

        // Closing a long is a sell, so it fills 1% below the market.
        let trade = sim
            .close_at_market(ExitTrigger::Invalidation, 1, 110.0, vec!["lost".into()])
            .unwrap();
        assert!((trade.exit_price - 108.9).abs() < 1e-9);
        assert_eq!(trade.exit_trigger, ExitTrigger::Invalidation);
        assert_eq!(trade.exit_reasons, vec!["lost".to_string()]);
    }

    #[test]
    fn the_position_view_exposes_unrealized_r() {
        let mut sim = clean();
        sim.on_entry(&long_signal(95.0, None), 0, 100.0, "ranging".into(), 0);

        // 2 points up on 5 points of risk.
        let view = sim.position_view(102.0).unwrap();
        assert!((view.unrealized_r - 0.4).abs() < 1e-9);
        assert_eq!(view.direction, Direction::Long);
        assert!(sim.position_view(102.0).is_some());
    }

    #[test]
    fn an_unknown_exit_time_is_recorded_verbatim() {
        let mut sim = clean();
        sim.on_entry(&long_signal(95.0, None), 1_234, 100.0, "ranging".into(), 0);
        let trade = sim
            .close(ExitTrigger::EndOfData, 9_999, 100.0, Vec::new())
            .unwrap();
        assert_eq!(trade.entry_time, 1_234);
        assert_eq!(trade.exit_time, 9_999);
        assert!((trade.r_multiple).abs() < 1e-9);
    }
}

/// One completed round trip, with everything needed to explain it later.
///
/// The spec requires enough detail that "why did it lose money in June?" can be
/// answered by inspecting real trades rather than aggregate statistics. That
/// means the reasons, the regime, and the resolved levels -- not just the P&L.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Which way the trade was.
    pub direction: Direction,
    /// Fill time (unix nanos).
    pub entry_time: i64,
    /// Fill price, after slippage.
    pub entry_price: f64,
    /// Exit time (unix nanos).
    pub exit_time: i64,
    /// Exit price.
    pub exit_price: f64,
    /// The decision candle's close when the trade was decided. Differs from
    /// `entry_price` by the gap to the next open plus slippage.
    pub reference_price: f64,
    /// The resolved stop.
    pub stop_price: f64,
    /// The resolved target, if any.
    pub take_profit_price: Option<f64>,
    /// Units held.
    pub size: f64,
    /// Risk per unit at entry.
    pub risk_per_unit: f64,
    /// Result in R multiples. `-1.0` is a stop-out, `+2.5` is a 2.5R win.
    pub r_multiple: f64,
    /// Closed candles the position was held for.
    pub bars_held: usize,
    /// What closed it.
    pub exit_trigger: ExitTrigger,
    /// Labels of the conditions that opened the trade.
    pub entry_reasons: Vec<String>,
    /// Labels of the conditions that closed it, when conditions did.
    pub exit_reasons: Vec<String>,
    /// Structural trend on the decision timeframe at entry.
    pub regime: String,
}

impl TradeRecord {
    /// Whether this trade made money.
    #[must_use]
    pub fn is_win(&self) -> bool {
        self.r_multiple > 0.0
    }

    /// Whether this trade lost money.
    #[must_use]
    pub fn is_loss(&self) -> bool {
        self.r_multiple < 0.0
    }

    /// How long the trade was held, in nanoseconds.
    #[must_use]
    pub const fn holding_nanos(&self) -> i64 {
        self.exit_time - self.entry_time
    }
}

/// What the numbers rest on.
///
/// The spec asks for the fill simplifications to be documented *in the report
/// output* so results are not over-trusted. This is that documentation, carried
/// as data rather than prose in a doc comment nobody reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillAssumptions {
    /// How entries fill.
    pub entry_fill: String,
    /// How condition-driven exits fill.
    pub exit_fill: String,
    /// How stop and target exits fill.
    pub stop_target_fill: String,
    /// How a bar that touches both the stop and the target is resolved.
    pub ambiguous_bar: String,
    /// Slippage applied to market orders, in basis points.
    pub slippage_bps: f64,
    /// How positions are sized.
    pub position_sizing: String,
    /// What `net_return_pct` and `max_drawdown_pct` are actually measured in.
    pub return_units: String,
    /// Whether results compound.
    pub compounding: String,
}

impl Default for FillAssumptions {
    fn default() -> Self {
        Self {
            entry_fill: "next decision candle's open, with slippage against the trade".into(),
            exit_fill: "next decision candle's open, with slippage against the trade".into(),
            stop_target_fill: "the touched level itself, with no slippage beyond the level; \
                                timestamped at the bar's close, since OHLC cannot say where \
                                inside the bar the level was reached"
                .into(),
            ambiguous_bar: "a candle touching both stop and target is assumed to hit the stop"
                .into(),
            slippage_bps: 2.0,
            position_sizing: "fixed-fractional against a constant starting equity; size does not \
                              feed the P&L"
                .into(),
            return_units: "R multiples -- 1R is the risk accepted at entry, not a percentage"
                .into(),
            compounding: "none; Phase 3 does not reinvest".into(),
        }
    }
}
