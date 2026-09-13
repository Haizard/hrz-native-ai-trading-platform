//! `MarketContext` -- everything a strategy is allowed to see at one instant.
//!
//! ## The visibility rule
//!
//! A strategy may only read data **up to and including the candle that just
//! closed**. `MarketContext` enforces that structurally rather than by
//! convention: it holds a snapshot, there is no reference back to the full
//! series, and the replay driver only ever puts closed candles into it. There
//! is no API on this type that can reach a future bar, so a strategy cannot
//! look ahead even if it tries -- which is why the no-look-ahead guarantee in
//! `docs/07-BACKTESTING-ENGINE.md` is a property of the design rather than of
//! careful coding.
//!
//! ## Shape
//!
//! One [`TimeframeView`] per declared timeframe, plus the open position. Each
//! view carries the newest closed candle, its [`MarketState`], the *previous*
//! state (so `crosses_above` has something to compare against), and a bounded
//! window of recent candles (so `new_low(n)` and ATR stops have something to
//! measure over).
//!
//! The previous view is a `Box<TimeframeView>` with `previous: None`, so the
//! recursion is exactly one level deep -- enough for a cross, not enough to
//! accidentally expose history the strategy should not have.

use std::collections::BTreeMap;

use analytics_core::liquidity::LiquidityLevel;
use analytics_core::{Candle, CvdDivergence, MarketState, Timeframe, Trend};
use serde::{Deserialize, Serialize};
use strategy_dsl::{Direction, Field};

/// One timeframe, as the strategy sees it right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeframeView {
    /// The name this timeframe was declared under, e.g. `"entry"`.
    pub name: String,
    /// Its resolution.
    pub timeframe: Timeframe,
    /// The newest **closed** candle.
    pub candle: Candle,
    /// The state built from the window ending at that candle.
    pub state: MarketState,
    /// The state one candle earlier, for `crosses_above`. `None` on the first
    /// bar, and on the one-level-deep previous view.
    pub previous: Option<Box<TimeframeView>>,
    /// Recent closed candles ending at `candle`, oldest first.
    ///
    /// Bounded by the runtime's `max_history`; `new_low(n)` and ATR-based stops
    /// read from here and treat an insufficient window as a warm-up, never as
    /// "the low so far".
    pub history: Vec<Candle>,
}

impl TimeframeView {
    /// The most recent `n` candles, or `None` when fewer than `n` are retained.
    ///
    /// Returning `None` rather than a short slice is the whole point: a
    /// `new_low(20)` on bar 3 must be `false`, not "the lowest of the three
    /// bars we happen to have", which would fire on every early bar.
    #[must_use]
    pub fn last_n(&self, n: usize) -> Option<&[Candle]> {
        if n == 0 || self.history.len() < n {
            return None;
        }
        Some(&self.history[self.history.len() - n..])
    }

    /// The previous view, if this is not already a previous view.
    #[must_use]
    pub fn prev(&self) -> Option<&TimeframeView> {
        self.previous.as_deref()
    }
}

/// The open position, as the strategy sees it.
///
/// Present in the context only while a position is open, which is what makes
/// `in_position` and the position-scoped fields mean what they say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionView {
    /// Which way the position is.
    pub direction: Direction,
    /// Price the position was filled at.
    pub entry_price: f64,
    /// When it was filled (unix nanos).
    pub entry_time: i64,
    /// The stop the position is carrying.
    pub stop_price: f64,
    /// Its take-profit level, if any.
    pub take_profit_price: Option<f64>,
    /// Units held.
    pub size: f64,
    /// Closed candles since the fill.
    pub bars_in_trade: usize,
    /// Open profit/loss in R multiples, measured against the *current* close.
    pub unrealized_r: f64,
}

impl PositionView {
    /// Risk per unit, always positive.
    #[must_use]
    pub fn risk_per_unit(&self) -> f64 {
        (self.entry_price - self.stop_price).abs()
    }
}

/// Everything a strategy may read at one instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketContext {
    /// Symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Close time of the newest decision candle, unix nanoseconds.
    ///
    /// The strategy's "now". Nothing newer than this exists as far as the
    /// strategy is concerned.
    pub now: i64,
    /// Name of the timeframe the strategy makes decisions on -- the finest
    /// declared one.
    pub decision_timeframe: String,
    /// One view per declared timeframe, keyed by declared name.
    pub timeframes: BTreeMap<String, TimeframeView>,
    /// The open position, if there is one.
    pub position: Option<PositionView>,
    /// Account equity, used for position sizing.
    pub equity: f64,
}

impl MarketContext {
    /// The view for a declared timeframe name.
    #[must_use]
    pub fn view(&self, name: &str) -> Option<&TimeframeView> {
        self.timeframes.get(name)
    }

    /// The decision timeframe's view.
    ///
    /// `None` only if the context was built without its own decision timeframe,
    /// which the engine treats as a hard error rather than a skip.
    #[must_use]
    pub fn decision(&self) -> Option<&TimeframeView> {
        self.view(&self.decision_timeframe)
    }

    /// Whether a position is open.
    #[must_use]
    pub const fn in_position(&self) -> bool {
        self.position.is_some()
    }

    /// Read a field from a declared timeframe.
    ///
    /// Position-scoped fields ignore `timeframe` -- there is one position, not
    /// one per timeframe -- and are [`FieldValue::Absent`] while flat.
    #[must_use]
    pub fn read<'a>(&'a self, timeframe: &str, field: Field) -> FieldValue<'a> {
        if field.is_position_scoped() {
            return self.read_position(field);
        }

        let Some(view) = self.view(timeframe) else {
            return FieldValue::Absent;
        };
        let state = &view.state;
        let candle = &view.candle;

        match field {
            Field::Open => FieldValue::Num(candle.open),
            Field::High => FieldValue::Num(candle.high),
            Field::Low => FieldValue::Num(candle.low),
            Field::Close => FieldValue::Num(candle.close),
            Field::Price => FieldValue::Num(state.price),
            Field::Delta => FieldValue::Num(state.delta),
            Field::Cvd => FieldValue::Num(state.cvd),
            // VWAP needs volume. Absent, not zero: a `close_below(vwap)` on a
            // dead market must be false, not trivially true against 0.
            Field::Vwap => state.vwap.map_or(FieldValue::Absent, FieldValue::Num),
            Field::Poc => FieldValue::Num(state.poc),
            Field::Vah => FieldValue::Num(state.vah),
            Field::Val => FieldValue::Num(state.val),
            Field::Volume => FieldValue::Num(state.volume),
            Field::BuyVolume => FieldValue::Num(state.buy_volume),
            Field::SellVolume => FieldValue::Num(state.sell_volume),

            Field::Trend | Field::MarketStructureTrend => FieldValue::Str(trend_name(state.trend)),
            Field::Divergence => FieldValue::Str(divergence_name(state.divergence)),

            Field::MarketStructureSwingHigh => state
                .swing_highs
                .last()
                .copied()
                .map_or(FieldValue::Absent, FieldValue::Num),
            Field::MarketStructureSwingLow => state
                .swing_lows
                .last()
                .copied()
                .map_or(FieldValue::Absent, FieldValue::Num),

            Field::AbsorptionDetected => FieldValue::Bool(!state.absorption.is_empty()),
            Field::AbsorptionBullish => {
                FieldValue::Bool(state.latest_absorption().is_some_and(|a| a.is_bullish()))
            }
            Field::AbsorptionBearish => {
                FieldValue::Bool(state.latest_absorption().is_some_and(|a| a.is_bearish()))
            }
            Field::AbsorptionStrength => state
                .latest_absorption()
                .map_or(FieldValue::Absent, |a| FieldValue::Num(a.strength)),

            Field::ImbalanceDetected => FieldValue::Bool(!state.imbalances.is_empty()),
            Field::ImbalanceBuy => FieldValue::Bool(state.imbalances.iter().any(|e| e.is_buy())),
            Field::ImbalanceSell => FieldValue::Bool(state.imbalances.iter().any(|e| e.is_sell())),
            Field::ImbalanceStacked => {
                // "Stacked" means a run of same-side events, so it takes at
                // least two levels to be a stack at all.
                const STACK_LEVELS: usize = 2;
                FieldValue::Bool(state.imbalances.iter().any(|e| e.is_stacked(STACK_LEVELS)))
            }
            Field::ImbalanceNetVolume => FieldValue::Num(state.net_imbalance_volume()),

            Field::LiquiditySwept => FieldValue::Str(swept_side(view)),
            Field::LiquiditySweptLevel => match swept_side(view) {
                "buy_side" => {
                    swept_level_price(view, true).map_or(FieldValue::Absent, FieldValue::Num)
                }
                "sell_side" => {
                    swept_level_price(view, false).map_or(FieldValue::Absent, FieldValue::Num)
                }
                // Nothing was swept, so there is no level to name. Absent rather
                // than zero: a comparison against it must be false, not true
                // against a price of zero.
                _ => FieldValue::Absent,
            },
            Field::LiquidityNearestAbove => state
                .nearest_liquidity_above()
                .map_or(FieldValue::Absent, |l| FieldValue::Num(l.price)),
            Field::LiquidityNearestBelow => state
                .nearest_liquidity_below()
                .map_or(FieldValue::Absent, |l| FieldValue::Num(l.price)),

            // Handled by the position-scoped branch above.
            Field::StopPrice
            | Field::EntryPrice
            | Field::PositionSize
            | Field::UnrealizedR
            | Field::BarsInTrade
            | Field::InPosition => self.read_position(field),
        }
    }

    /// Position-scoped fields. `Absent` while flat, which makes every
    /// comparison against them false -- so a condition that depends on an open
    /// position simply does not fire when there is none.
    #[allow(clippy::cast_precision_loss)]
    fn read_position(&self, field: Field) -> FieldValue<'static> {
        let Some(position) = &self.position else {
            return FieldValue::Absent;
        };

        match field {
            Field::StopPrice => FieldValue::Num(position.stop_price),
            Field::EntryPrice => FieldValue::Num(position.entry_price),
            Field::PositionSize => FieldValue::Num(position.size),
            Field::UnrealizedR => FieldValue::Num(position.unrealized_r),
            Field::BarsInTrade => FieldValue::Num(position.bars_in_trade as f64),
            Field::InPosition => FieldValue::Bool(true),
            _ => FieldValue::Absent,
        }
    }
}

/// A field's value at runtime.
///
/// [`FieldValue::Absent`] is a first-class outcome, not an error. It means "the
/// market has not produced this yet" -- no VWAP before any volume traded, no
/// swing before structure confirms, no stop price while flat. Every operation
/// involving an absent operand is `false`, so an absent value makes a condition
/// *not fire* rather than firing on a fabricated zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldValue<'a> {
    /// A boolean.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string, borrowed from either the market state or the expression.
    Str(&'a str),
    /// The market has not produced this value yet.
    Absent,
}

impl<'a> FieldValue<'a> {
    /// Whether this is [`FieldValue::Absent`].
    #[must_use]
    pub const fn is_absent(self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Treat as a boolean. Absent is false.
    #[must_use]
    pub const fn truthy(self) -> bool {
        matches!(self, Self::Bool(true))
    }

    /// Treat as a number, if it is one.
    #[must_use]
    pub const fn as_num(self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(n),
            _ => None,
        }
    }

    /// The numeric value, or `None` when absent or not a number.
    ///
    /// Used by comparisons: an absent operand yields `None`, which the caller
    /// turns into `false`.
    #[must_use]
    pub const fn number_or_absent(self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(n),
            _ => None,
        }
    }

    /// The string value, if any.
    #[must_use]
    pub const fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// The lowercase name of a structural trend.
///
/// Written by hand rather than taken from serde: `Trend` derives `Serialize`
/// with the default variant naming, which yields `"Bullish"`, while the DSL
/// documents `"bullish"`. Mapping explicitly keeps the document vocabulary
/// independent of how the analytics crate happens to serialize its enums.
#[must_use]
pub fn trend_name(trend: Trend) -> &'static str {
    match trend {
        Trend::Bullish => "bullish",
        Trend::Bearish => "bearish",
        Trend::Ranging => "ranging",
    }
}

/// The lowercase name of a CVD divergence.
#[must_use]
pub fn divergence_name(divergence: CvdDivergence) -> &'static str {
    match divergence {
        CvdDivergence::Bullish => "bullish",
        CvdDivergence::Bearish => "bearish",
        CvdDivergence::None => "none",
    }
}

/// Which side of resting liquidity the most recent sweep took out.
///
/// Follows the standard reading of the terms:
///
/// * **sell-side liquidity** rests *below* lows (the sell stops of longs).
///   Sweeping it means price traded down through a low.
/// * **buy-side liquidity** rests *above* highs (the buy stops of shorts).
///   Sweeping it means price traded up through a high.
///
/// So the spec's `liquidity.swept == "sell_side"` entry condition describes a
/// sweep *down* through a low -- the first half of the documented
/// liquidity-sweep setup.
///
/// ## Which sweep is "the most recent"
///
/// The levels the **newest candle itself traded through** win. That is what
/// makes the field describe the bar being evaluated rather than some older one:
/// an entry condition asking "was liquidity just swept?" means *this* bar, not
/// four bars ago.
///
/// Only when the newest candle took out nothing does this fall back to the most
/// recently *formed* swept level, so the field still answers on a quiet bar
/// instead of going blank.
///
/// A bar that sweeps both sides is decided by whichever level it cleared by
/// more, relative to that level's price. That is a deterministic tie-break on
/// real data rather than an arbitrary one -- and a wide bar that takes out both
/// a high and a low genuinely has no single answer, so the bigger grab naming it
/// is the honest choice.
#[must_use]
pub fn swept_side(view: &TimeframeView) -> &'static str {
    let levels: Vec<&LiquidityLevel> = view
        .state
        .liquidity
        .iter()
        .filter(|level| level.swept)
        .collect();

    match freshest_sweep(view, &levels) {
        Some(level) if level.kind.is_above() => "buy_side",
        Some(_) => "sell_side",
        None => "none",
    }
}

/// The price of the swept level a stop should reference, on one side.
///
/// `above` selects buy-side (`true`) or sell-side (`false`) levels. This is the
/// level the newest candle actually cleared -- not merely the most recently
/// *formed* swept level, which is a different and much worse answer: selecting by
/// formation index lets the stop land on a level price has already fallen
/// through. On real BTCUSDT 5m data that produced stops *above* the entry price
/// for long trades (refused, correctly) and, worse, stops a hair below it
/// (accepted, and then meaningless, because a near-zero stop distance turns any
/// move into an arbitrary multiple of R).
///
/// When the candle cleared several levels at once it is the **extreme** one that
/// counts -- the lowest low for a long, the highest high for a short. That is the
/// conservative choice: stopping below the lowest level the bar took out is
/// always at least as far from the entry as stopping below a shallower one, and
/// it is the level whose loss genuinely invalidates the sweep. Taking the level
/// the candle overshot *most* would do the opposite and pick the highest low,
/// which is how a long's stop ends up above its entry.
///
/// A candle that cleared nothing falls back to the most recently formed swept
/// level on the same side, so the answer degrades to the old behaviour only when
/// there is nothing better to say.
#[must_use]
pub fn swept_level_price(view: &TimeframeView, above: bool) -> Option<f64> {
    let cleared = view
        .state
        .liquidity
        .iter()
        .filter(|level| level.swept && level.kind.is_above() == above)
        .filter(|level| overshoot(&view.candle, level).is_some())
        .map(|level| level.price);

    let extreme = if above {
        cleared.reduce(f64::max)
    } else {
        cleared.reduce(f64::min)
    };
    if extreme.is_some() {
        return extreme;
    }

    view.state
        .liquidity
        .iter()
        .filter(|level| level.swept && level.kind.is_above() == above)
        .max_by_key(|level| level.last_index)
        .map(|level| level.price)
}

/// The swept level that best describes the newest candle, by relative overshoot.
///
/// Levels the candle itself traded through win; only when it took out nothing
/// does this fall back to the most recently *formed* swept level, so a quiet bar
/// still answers rather than going blank.
fn freshest_sweep<'a>(
    view: &'a TimeframeView,
    candidates: &[&'a LiquidityLevel],
) -> Option<&'a LiquidityLevel> {
    let fresh: Vec<(&LiquidityLevel, f64)> = candidates
        .iter()
        .filter_map(|level| overshoot(&view.candle, level).map(|depth| (*level, depth)))
        .collect();

    if fresh.is_empty() {
        candidates
            .iter()
            .copied()
            .max_by_key(|level| level.last_index)
    } else {
        fresh
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(level, _)| *level)
    }
}

/// How far beyond a level a candle traded, as a fraction of the level price.
/// `None` when the candle never reached it.
fn overshoot(candle: &Candle, level: &LiquidityLevel) -> Option<f64> {
    if level.price.abs() < f64::EPSILON {
        return None;
    }
    if level.kind.is_above() {
        let beyond = candle.high - level.price;
        (beyond > 0.0).then(|| beyond / level.price)
    } else {
        let beyond = level.price - candle.low;
        (beyond > 0.0).then(|| beyond / level.price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::state::{build_market_state, MarketStateConfig};
    use analytics_core::types::{Candle, Timeframe};

    fn candle(open_time: i64, high: f64, low: f64, close: f64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high,
            low,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn config() -> MarketStateConfig {
        MarketStateConfig {
            bucket_size: 1.0,
            // One-bar lookbacks on both detectors, so a four- or five-candle
            // fixture can actually confirm a swing and sweep a level. With the
            // production lookbacks these fixtures would produce no liquidity
            // levels at all and the assertions would pass vacuously.
            structure: analytics_core::market_structure::StructureConfig { lookback: 1 },
            liquidity: analytics_core::liquidity::LiquidityConfig {
                lookback: 1,
                ..analytics_core::liquidity::LiquidityConfig::default()
            },
            ..MarketStateConfig::default()
        }
    }

    fn view_from(candles: Vec<Candle>) -> TimeframeView {
        let state = build_market_state(&candles, &[], &config()).unwrap();
        TimeframeView {
            name: "entry".into(),
            timeframe: Timeframe::M1,
            candle: candles.last().unwrap().clone(),
            state,
            previous: None,
            history: candles,
        }
    }

    fn ctx(candles: Vec<Candle>, position: Option<PositionView>) -> MarketContext {
        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), view_from(candles));
        MarketContext {
            symbol: "BTCUSDT".into(),
            now: 240_000_000_000,
            decision_timeframe: "entry".into(),
            timeframes,
            position,
            equity: 10_000.0,
        }
    }

    fn series() -> Vec<Candle> {
        vec![
            candle(0, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(60, 12.0, 9.0, 11.0, 3.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 3.0),
            candle(180, 13.0, 10.0, 13.0, 4.0, 1.0),
        ]
    }

    fn position() -> PositionView {
        PositionView {
            direction: Direction::Long,
            entry_price: 100.0,
            entry_time: 0,
            stop_price: 95.0,
            take_profit_price: Some(112.5),
            size: 0.2,
            bars_in_trade: 3,
            unrealized_r: 0.4,
        }
    }

    #[test]
    fn ohlc_comes_from_the_candle_and_the_rest_from_the_state() {
        let c = ctx(series(), None);
        assert_eq!(c.read("entry", Field::Open).as_num(), Some(13.0));
        assert_eq!(c.read("entry", Field::High).as_num(), Some(13.0));
        assert_eq!(c.read("entry", Field::Low).as_num(), Some(10.0));
        assert_eq!(c.read("entry", Field::Close).as_num(), Some(13.0));
        assert_eq!(c.read("entry", Field::Price).as_num(), Some(13.0));
        assert_eq!(c.read("entry", Field::Delta).as_num(), Some(3.0));
        assert_eq!(c.read("entry", Field::Cvd).as_num(), Some(3.0));
        assert_eq!(c.read("entry", Field::Volume).as_num(), Some(5.0));
        assert_eq!(c.read("entry", Field::BuyVolume).as_num(), Some(4.0));
        assert_eq!(c.read("entry", Field::SellVolume).as_num(), Some(1.0));
    }

    #[test]
    fn an_undeclared_timeframe_reads_absent_rather_than_panicking() {
        let c = ctx(series(), None);
        assert!(c.read("nope", Field::Close).is_absent());
        assert!(c.view("nope").is_none());
    }

    #[test]
    fn trend_and_divergence_use_the_documents_lowercase_vocabulary() {
        // The state's `Trend::Bullish` serializes as "Bullish"; the DSL says
        // "bullish". The context is the translation point, so the mapping is
        // asserted directly rather than through a fixture's structure detection.
        assert_eq!(trend_name(Trend::Bullish), "bullish");
        assert_eq!(trend_name(Trend::Bearish), "bearish");
        assert_eq!(trend_name(Trend::Ranging), "ranging");
        assert_eq!(divergence_name(CvdDivergence::Bullish), "bullish");
        assert_eq!(divergence_name(CvdDivergence::Bearish), "bearish");
        assert_eq!(divergence_name(CvdDivergence::None), "none");

        let c = ctx(series(), None);
        let trend = c.read("entry", Field::Trend).as_str().expect("a trend");
        assert!(
            ["bullish", "bearish", "ranging"].contains(&trend),
            "unexpected trend {trend}"
        );
        assert_eq!(
            c.read("entry", Field::MarketStructureTrend).as_str(),
            Some(trend)
        );
        assert!(
            !trend.chars().any(char::is_uppercase),
            "the document vocabulary is lowercase, got {trend}"
        );
    }

    #[test]
    fn swing_fields_are_absent_until_structure_confirms() {
        let flat = vec![
            candle(0, 10.0, 10.0, 10.0, 1.0, 1.0),
            candle(60, 10.0, 10.0, 10.0, 1.0, 1.0),
        ];
        let c = ctx(flat, None);
        assert!(c.read("entry", Field::MarketStructureSwingHigh).is_absent());
        assert!(c.read("entry", Field::MarketStructureSwingLow).is_absent());

        let rising = ctx(series(), None);
        assert_eq!(
            rising
                .read("entry", Field::MarketStructureSwingHigh)
                .as_num(),
            Some(12.0)
        );
    }

    #[test]
    fn position_fields_are_absent_while_flat() {
        let flat = ctx(series(), None);
        for field in [
            Field::StopPrice,
            Field::EntryPrice,
            Field::PositionSize,
            Field::UnrealizedR,
            Field::BarsInTrade,
        ] {
            assert!(flat.read("entry", field).is_absent(), "{field}");
        }
        assert!(!flat.read("entry", Field::InPosition).truthy());
        assert!(!flat.in_position());
    }

    #[test]
    fn position_fields_resolve_once_a_position_is_open() {
        let open = ctx(series(), Some(position()));
        assert_eq!(open.read("entry", Field::StopPrice).as_num(), Some(95.0));
        assert_eq!(open.read("entry", Field::EntryPrice).as_num(), Some(100.0));
        assert_eq!(open.read("entry", Field::PositionSize).as_num(), Some(0.2));
        assert_eq!(open.read("entry", Field::UnrealizedR).as_num(), Some(0.4));
        assert_eq!(open.read("entry", Field::BarsInTrade).as_num(), Some(3.0));
        assert!(open.read("entry", Field::InPosition).truthy());
        assert!(open.in_position());
    }

    #[test]
    fn position_fields_ignore_which_timeframe_is_asked_for() {
        // There is one position, not one per timeframe.
        let open = ctx(series(), Some(position()));
        assert_eq!(open.read("whatever", Field::StopPrice).as_num(), Some(95.0));
    }

    #[test]
    fn vwap_is_absent_without_volume_rather_than_zero() {
        let dead = vec![
            candle(0, 10.0, 10.0, 10.0, 0.0, 0.0),
            candle(60, 11.0, 11.0, 11.0, 0.0, 0.0),
        ];
        let c = ctx(dead, None);
        let value = c.read("entry", Field::Vwap);
        assert!(value.is_absent(), "vwap must not be reported as 0");
        // And an absent number never compares as if it were a real price.
        assert_eq!(value.number_or_absent(), None);
    }

    #[test]
    fn last_n_refuses_a_window_it_does_not_have() {
        let view = view_from(series());
        assert_eq!(view.last_n(4).map(<[Candle]>::len), Some(4));
        assert_eq!(view.last_n(5), None, "a short window must not be faked");
        assert_eq!(view.last_n(0), None);
    }

    #[test]
    fn swept_side_is_none_without_a_sweep() {
        let c = ctx(series(), None);
        let value = c.read("entry", Field::LiquiditySwept).as_str();
        assert!(value.is_some());
    }

    #[test]
    fn swept_side_reads_sell_side_for_a_swept_low_and_buy_side_for_a_swept_high() {
        // A swing low at 8, then a bar trading down through it.
        let low_sweep = vec![
            candle(0, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(60, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(180, 10.0, 7.0, 8.0, 1.0, 3.0),
        ];
        assert_eq!(swept_side(&view_from(low_sweep)), "sell_side");

        // The mirror: a swing high at 12, then a bar trading up through it.
        let high_sweep = vec![
            candle(0, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(60, 12.0, 10.0, 11.0, 1.0, 1.0),
            candle(120, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(180, 13.0, 10.0, 12.0, 3.0, 1.0),
        ];
        assert_eq!(swept_side(&view_from(high_sweep)), "buy_side");
    }

    #[test]
    fn swept_side_is_none_when_nothing_was_ever_swept() {
        // A swing low exists but no later bar trades through it.
        let untested = vec![
            candle(0, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(60, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(120, 11.0, 9.5, 10.0, 1.0, 1.0),
        ];
        assert_eq!(swept_side(&view_from(untested)), "none");
    }

    #[test]
    fn swept_side_describes_the_newest_candle_not_an_older_sweep() {
        // Bar 3 sweeps the low at 8. Bar 4 is quiet -- it takes out nothing --
        // so the field falls back to the most recently formed swept level
        // rather than going blank, and still reports sell-side.
        let quiet_fifth = view_from(vec![
            candle(0, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(60, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(180, 10.0, 7.0, 8.0, 1.0, 3.0),
            candle(240, 9.5, 8.5, 9.0, 1.0, 1.0),
        ]);
        assert_eq!(swept_side(&quiet_fifth), "sell_side");

        // Now the same shape, but bar 4 sweeps a *high* that formed earlier.
        // Both sides have been swept by the end, so ordering by when each level
        // formed would answer "sell_side" (the low formed first). The field must
        // instead describe the bar being evaluated: buy_side.
        let sweeps_a_high = view_from(vec![
            candle(0, 10.0, 9.0, 9.5, 1.0, 1.0),
            candle(60, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(120, 10.5, 9.0, 10.0, 1.0, 1.0),
            candle(180, 10.0, 7.0, 8.0, 1.0, 3.0),
            candle(240, 11.0, 8.5, 10.5, 1.0, 1.0),
        ]);
        assert_eq!(swept_side(&sweeps_a_high), "buy_side");
    }

    #[test]
    fn the_context_serializes_so_it_can_be_shown_to_the_agent() {
        let json = serde_json::to_string(&ctx(series(), Some(position()))).unwrap();
        assert!(json.contains("\"decision_timeframe\":\"entry\""), "{json}");
        assert!(json.contains("\"stop_price\":95.0"), "{json}");
    }

    #[test]
    fn a_stop_references_the_level_this_candle_cleared_not_a_stale_one() {
        // Two sell-side levels exist. The low at 8.0 formed at bar 1; a *newer*
        // one at 9.3 formed at bar 3. Bar 5 dumps to 7.0, clearing both, and
        // closes at 8.6 -- below 9.3.
        //
        // Ordering swept levels by when they formed picks the newer 9.3 level,
        // which now sits *above* the close: a long stop above its entry, which
        // is not a stop at all. Taking the level the bar overshot most picks the
        // same wrong answer, because a lower low overshoots a higher level by
        // more. Only the lowest cleared level is a stop.
        let view = view_from(vec![
            candle(0, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(60, 10.5, 8.0, 9.0, 1.0, 3.0),
            candle(120, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(180, 10.5, 9.3, 10.0, 1.0, 1.0),
            candle(240, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(300, 9.0, 7.0, 8.6, 1.0, 5.0),
        ]);

        assert_eq!(swept_side(&view), "sell_side");
        assert_eq!(
            swept_level_price(&view, false),
            Some(8.0),
            "the stop must sit below the lowest low this bar cleared, not a stale level above it"
        );
        assert!(swept_level_price(&view, false).unwrap() < view.candle.close);
    }

    #[test]
    fn a_stop_uses_the_extreme_level_when_a_bar_clears_several() {
        // One wide bar takes out lows at 9.0 and 8.0. The conservative stop is
        // below the *lowest* of them.
        let view = view_from(vec![
            candle(0, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(60, 10.5, 9.0, 10.0, 1.0, 1.0),
            candle(120, 11.0, 10.0, 10.5, 1.0, 1.0),
            candle(180, 10.5, 8.0, 9.0, 1.0, 1.0),
            candle(240, 11.0, 9.5, 10.0, 1.0, 1.0),
            candle(300, 10.0, 6.5, 7.5, 1.0, 5.0),
        ]);
        assert_eq!(swept_level_price(&view, false), Some(8.0));

        // The mirror: a wide up bar clearing highs at 11.0 and 12.0 stops above
        // the *highest*, and there is no sell-side answer to give.
        let mirrored = view_from(vec![
            candle(0, 10.5, 9.5, 10.0, 1.0, 1.0),
            candle(60, 11.0, 9.5, 10.0, 1.0, 1.0),
            candle(120, 10.5, 9.5, 10.0, 1.0, 1.0),
            candle(180, 12.0, 10.0, 11.0, 1.0, 1.0),
            candle(240, 11.0, 9.5, 10.0, 1.0, 1.0),
            candle(300, 13.0, 10.5, 12.5, 1.0, 1.0),
        ]);
        assert_eq!(swept_level_price(&mirrored, true), Some(12.0));
        assert!(swept_level_price(&mirrored, false).is_none());
    }
}
