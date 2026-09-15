//! Market structure -- swings, Break of Structure and Change of Character.
//!
//! This is the skeleton traders draw on a chart: the sequence of higher highs
//! and higher lows (or the inverse) that defines a trend, and the two events
//! that end one.
//!
//! * **BOS (Break of Structure)** -- price closes through the last swing *in
//!   the direction of the trend*. Continuation.
//! * **CHoCH (Change of Character)** -- price closes through the last swing
//!   *against* the trend. The first real evidence of a reversal.
//!
//! ## Swing definition
//!
//! A candle at index `i` is a swing high when its high is **strictly** greater
//! than the highs of the `lookback` candles on each side (mirror for lows).
//! Strictness matters: with `>=`, a flat plateau produces a swing at every
//! candle in it, which is noise. It also makes the result deterministic, so the
//! same data always yields the same structure -- a hard requirement for the
//! backtester.
//!
//! ## Confirmation delay
//!
//! A swing at index `i` cannot be known until `lookback` bars have printed
//! *after* it. This module respects that: levels only become tradeable once
//! they are confirmed. Using an unconfirmed swing would be look-ahead bias, and
//! it would quietly inflate every backtest.

use serde::{Deserialize, Serialize};

use crate::types::{Candle, Side};

/// Which extreme a swing point marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwingKind {
    /// A local high.
    High,
    /// A local low.
    Low,
}

/// A confirmed swing point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwingPoint {
    /// Index into the candle slice.
    pub index: usize,
    /// Timestamp of the swing candle.
    pub timestamp: i64,
    /// The swing price (the candle's high or low).
    pub price: f64,
    /// Whether this is a high or a low.
    pub kind: SwingKind,
}

/// Overall direction implied by the structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trend {
    /// Higher highs and higher lows.
    Bullish,
    /// Lower highs and lower lows.
    Bearish,
    /// No structure has broken yet.
    Ranging,
}

/// Which kind of structural event occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakKind {
    /// Break in the direction of the prevailing trend: continuation.
    Bos,
    /// Break against the prevailing trend: reversal evidence.
    Choch,
}

impl BreakKind {
    /// Every variant, for a client's vocabulary and exhaustive tests.
    pub const ALL: [Self; 2] = [Self::Bos, Self::Choch];

    /// Canonical name, as a chart spells it.
    ///
    /// `BreakKind` derives `Serialize` without a rename, so it travels as
    /// `"Bos"`. That is fine inside a typed analytics message and wrong on a
    /// chart, where everything else is `snake_case` and the shell switches on
    /// the string. Same bridge, for the same reason, as [`Side::name`].
    ///
    /// [`Side::name`]: crate::types::Side::name
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bos => "bos",
            Self::Choch => "choch",
        }
    }
}

/// A close through a confirmed swing level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StructureBreak {
    /// Index of the breaking candle.
    pub index: usize,
    /// Timestamp of the breaking candle.
    pub timestamp: i64,
    /// Close price that did the breaking.
    pub price: f64,
    /// The swing level that was broken.
    pub level: f64,
    /// BOS or CHoCH.
    pub kind: BreakKind,
    /// `Buy` for an upward break, `Sell` for a downward one.
    pub direction: Side,
}

/// Tuning for [`detect_market_structure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureConfig {
    /// Candles required on each side of a swing. Larger = fewer, more
    /// significant swings.
    pub lookback: usize,
}

impl Default for StructureConfig {
    fn default() -> Self {
        Self { lookback: 3 }
    }
}

/// The structural read of a candle series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketStructure {
    /// Every confirmed swing point, in index order.
    pub points: Vec<SwingPoint>,
    /// Every BOS/CHoCH, in index order.
    pub breaks: Vec<StructureBreak>,
    /// Direction implied by the last break.
    pub trend: Trend,
    /// Confirmed swing-high prices, oldest first.
    pub swing_highs: Vec<f64>,
    /// Confirmed swing-low prices, oldest first.
    pub swing_lows: Vec<f64>,
}

impl MarketStructure {
    /// The most recent confirmed swing high, if any.
    #[must_use]
    pub fn latest_swing_high(&self) -> Option<f64> {
        self.swing_highs.last().copied()
    }

    /// The most recent confirmed swing low, if any.
    #[must_use]
    pub fn latest_swing_low(&self) -> Option<f64> {
        self.swing_lows.last().copied()
    }

    /// The most recent break, if any.
    #[must_use]
    pub fn latest_break(&self) -> Option<&StructureBreak> {
        self.breaks.last()
    }

    /// Whether the structure is currently trending up.
    #[must_use]
    pub const fn is_bullish(&self) -> bool {
        matches!(self.trend, Trend::Bullish)
    }

    /// Whether the structure is currently trending down.
    #[must_use]
    pub const fn is_bearish(&self) -> bool {
        matches!(self.trend, Trend::Bearish)
    }
}

/// Find every confirmed swing high and low.
///
/// Returns points in index order; a candle can be both a swing high and a swing
/// low (an outside bar), in which case the high is emitted first.
#[must_use]
pub fn find_swings(candles: &[Candle], lookback: usize) -> Vec<SwingPoint> {
    let mut points = Vec::new();
    if lookback == 0 || candles.len() < lookback * 2 + 1 {
        return points;
    }

    for i in lookback..candles.len() - lookback {
        let candle = &candles[i];
        let left = &candles[i - lookback..i];
        let right = &candles[i + 1..=i + lookback];

        let is_high =
            left.iter().all(|c| c.high < candle.high) && right.iter().all(|c| c.high < candle.high);
        let is_low =
            left.iter().all(|c| c.low > candle.low) && right.iter().all(|c| c.low > candle.low);

        if is_high {
            points.push(SwingPoint {
                index: i,
                timestamp: candle.open_time,
                price: candle.high,
                kind: SwingKind::High,
            });
        }
        if is_low {
            points.push(SwingPoint {
                index: i,
                timestamp: candle.open_time,
                price: candle.low,
                kind: SwingKind::Low,
            });
        }
    }

    points
}

/// Detect swing points, BOS/CHoCH events and the resulting trend.
///
/// A break consumes the level it broke: the same swing cannot be broken twice,
/// so a level that has already been taken out will not fire again.
///
/// # Example
///
/// ```
/// use analytics_core::market_structure::{
///     detect_market_structure, BreakKind, StructureConfig, Trend,
/// };
/// use analytics_core::types::{Candle, Timeframe};
///
/// fn c(high: f64, low: f64, close: f64) -> Candle {
///     Candle {
///         symbol: "BTCUSDT".into(),
///         timeframe: Timeframe::M1,
///         open_time: 0,
///         open: close,
///         high,
///         low,
///         close,
///         volume: 1.0,
///         buy_volume: 0.5,
///         sell_volume: 0.5,
///     }
/// }
///
/// // A swing high prints at 12, then a later candle closes above it.
/// let candles = vec![
///     c(10.0, 8.0, 9.0),
///     c(12.0, 9.0, 11.0), // swing high
///     c(11.0, 9.0, 10.0),
///     c(13.0, 10.0, 13.0), // closes through 12
/// ];
/// let structure = detect_market_structure(&candles, StructureConfig { lookback: 1 });
///
/// assert_eq!(structure.swing_highs, vec![12.0]);
/// assert_eq!(structure.breaks.len(), 1);
/// assert_eq!(structure.breaks[0].kind, BreakKind::Bos);
/// assert_eq!(structure.trend, Trend::Bullish);
/// ```
#[must_use]
pub fn detect_market_structure(candles: &[Candle], config: StructureConfig) -> MarketStructure {
    let points = find_swings(candles, config.lookback);

    let mut swing_highs = Vec::new();
    let mut swing_lows = Vec::new();
    for point in &points {
        match point.kind {
            SwingKind::High => swing_highs.push(point.price),
            SwingKind::Low => swing_lows.push(point.price),
        }
    }

    let mut breaks = Vec::new();
    let mut trend = Trend::Ranging;
    let mut last_high: Option<f64> = None;
    let mut last_low: Option<f64> = None;
    let mut next_point = 0usize;

    for (i, candle) in candles.iter().enumerate() {
        // Promote swings that have now had their full confirmation window.
        while next_point < points.len() && points[next_point].index + config.lookback <= i {
            match points[next_point].kind {
                SwingKind::High => last_high = Some(points[next_point].price),
                SwingKind::Low => last_low = Some(points[next_point].price),
            }
            next_point += 1;
        }

        if let Some(level) = last_high {
            if candle.close > level {
                breaks.push(StructureBreak {
                    index: i,
                    timestamp: candle.open_time,
                    price: candle.close,
                    level,
                    kind: if trend == Trend::Bearish {
                        BreakKind::Choch
                    } else {
                        BreakKind::Bos
                    },
                    direction: Side::Buy,
                });
                trend = Trend::Bullish;
                last_high = None;
            }
        }

        if let Some(level) = last_low {
            if candle.close < level {
                breaks.push(StructureBreak {
                    index: i,
                    timestamp: candle.open_time,
                    price: candle.close,
                    level,
                    kind: if trend == Trend::Bullish {
                        BreakKind::Choch
                    } else {
                        BreakKind::Bos
                    },
                    direction: Side::Sell,
                });
                trend = Trend::Bearish;
                last_low = None;
            }
        }
    }

    MarketStructure {
        points,
        breaks,
        trend,
        swing_highs,
        swing_lows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn c(high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: close,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn config(lookback: usize) -> StructureConfig {
        StructureConfig { lookback }
    }

    #[test]
    fn finds_a_strict_swing_high() {
        let candles = vec![c(10.0, 8.0, 9.0), c(12.0, 9.0, 11.0), c(11.0, 9.0, 10.0)];
        let points = find_swings(&candles, 1);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].index, 1);
        assert_eq!(points[0].kind, SwingKind::High);
        assert!((points[0].price - 12.0).abs() < 1e-9);
    }

    #[test]
    fn finds_a_strict_swing_low() {
        let candles = vec![c(10.0, 9.0, 9.5), c(10.0, 7.0, 8.0), c(11.0, 9.0, 10.5)];
        let points = find_swings(&candles, 1);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].kind, SwingKind::Low);
        assert!((points[0].price - 7.0).abs() < 1e-9);
    }

    #[test]
    fn a_flat_plateau_is_not_a_swing() {
        // Three identical highs: with strict comparison none of them qualifies.
        let candles = vec![
            c(10.0, 9.0, 9.5),
            c(12.0, 9.0, 11.0),
            c(12.0, 9.0, 11.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
        ];
        let points = find_swings(&candles, 1);
        assert!(
            !points.iter().any(|p| p.kind == SwingKind::High),
            "plateau must not register: {points:?}"
        );
    }

    #[test]
    fn an_outside_bar_is_both_a_swing_high_and_a_low() {
        let candles = vec![c(10.0, 9.0, 9.5), c(13.0, 6.0, 10.0), c(10.0, 9.0, 9.5)];
        let points = find_swings(&candles, 1);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].kind, SwingKind::High);
        assert_eq!(points[1].kind, SwingKind::Low);
    }

    #[test]
    fn too_few_candles_yield_no_swings() {
        let candles = vec![c(10.0, 9.0, 9.5), c(12.0, 9.0, 11.0)];
        assert!(find_swings(&candles, 1).is_empty());
        assert!(find_swings(&candles, 0).is_empty());
    }

    #[test]
    fn a_close_above_a_swing_high_is_a_bos_when_ranging() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
            c(13.0, 10.0, 13.0),
        ];
        let structure = detect_market_structure(&candles, config(1));
        assert_eq!(structure.breaks.len(), 1);
        assert_eq!(structure.breaks[0].kind, BreakKind::Bos);
        assert_eq!(structure.breaks[0].direction, Side::Buy);
        assert!((structure.breaks[0].level - 12.0).abs() < 1e-9);
        assert_eq!(structure.trend, Trend::Bullish);
        assert!(structure.is_bullish());
    }

    #[test]
    fn a_wick_through_the_level_does_not_break_structure() {
        // The high pokes above 12 but the candle closes back below it.
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
            c(13.0, 10.0, 11.5),
        ];
        let structure = detect_market_structure(&candles, config(1));
        assert!(structure.breaks.is_empty(), "close-based breaks only");
        assert_eq!(structure.trend, Trend::Ranging);
    }

    #[test]
    fn a_level_is_consumed_after_it_breaks() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
            c(13.0, 10.0, 13.0),
            c(14.0, 12.0, 13.5),
        ];
        let structure = detect_market_structure(&candles, config(1));
        assert_eq!(structure.breaks.len(), 1, "12.0 must not break twice");
    }

    #[test]
    fn a_reversal_after_a_bearish_break_is_a_choch() {
        // Swing high 12, then a close below the swing low 7, then a close back
        // above 12 -> change of character.
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0), // swing high 12
            c(11.0, 8.0, 9.0),
            c(10.0, 7.0, 7.5), // swing low 7
            c(9.0, 7.2, 7.6),  // low holds above 7
            c(8.0, 6.0, 6.5),  // closes below 7 -> bearish BOS
            c(10.0, 7.0, 9.0),
            c(13.0, 9.0, 13.0), // closes above 12 -> CHoCH
        ];
        let structure = detect_market_structure(&candles, config(1));
        assert_eq!(structure.breaks.len(), 2, "breaks: {:?}", structure.breaks);
        assert_eq!(structure.breaks[0].kind, BreakKind::Bos);
        assert_eq!(structure.breaks[0].direction, Side::Sell);
        assert!((structure.breaks[0].level - 7.0).abs() < 1e-9);
        assert_eq!(structure.breaks[1].kind, BreakKind::Choch);
        assert_eq!(structure.breaks[1].direction, Side::Buy);
        assert!((structure.breaks[1].level - 12.0).abs() < 1e-9);
        assert_eq!(structure.trend, Trend::Bullish);
    }

    #[test]
    fn swing_highs_and_lows_are_collected_in_order() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 8.0, 9.0),
            c(10.0, 7.0, 7.5),
            c(11.0, 8.0, 10.0),
        ];
        let structure = detect_market_structure(&candles, config(1));
        assert_eq!(structure.swing_highs, vec![12.0]);
        assert_eq!(structure.swing_lows, vec![7.0]);
        assert!((structure.latest_swing_high().unwrap() - 12.0).abs() < 1e-9);
        assert!((structure.latest_swing_low().unwrap() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn a_larger_lookback_needs_more_confirmation() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
            c(13.0, 10.0, 13.0),
        ];
        // lookback 2 needs two bars each side -> the 4-candle series has none.
        let structure = detect_market_structure(&candles, config(2));
        assert!(structure.points.is_empty());
        assert!(structure.breaks.is_empty());
    }

    #[test]
    fn empty_input_is_handled() {
        let structure = detect_market_structure(&[], config(3));
        assert!(structure.points.is_empty());
        assert!(structure.breaks.is_empty());
        assert_eq!(structure.trend, Trend::Ranging);
        assert!(structure.latest_break().is_none());
    }
}
