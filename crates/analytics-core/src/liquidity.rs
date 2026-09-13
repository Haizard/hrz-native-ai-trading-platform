//! Liquidity -- where the resting orders are.
//!
//! Stops do not sit at random prices. They cluster just beyond levels the
//! market has already respected: above equal highs (shorts' stops) and below
//! equal lows (longs' stops). Those clusters are what a "liquidity sweep" goes
//! hunting for -- the sharp wick that takes the level out and then reverses is
//! the market filling those orders.
//!
//! ## What is detected
//!
//! * **Swing highs/lows** -- single confirmed pivots
//!   ([`LiquidityKind::SwingHigh`] / [`LiquidityKind::SwingLow`]).
//! * **Equal highs/lows** -- two or more pivots at effectively the same price
//!   ([`LiquidityKind::EqualHighs`] / [`LiquidityKind::EqualLows`]). These are
//!   the ones that actually matter: a level defended twice is a level with
//!   orders behind it.
//!
//! Each level carries `swept`, which turns true once price trades beyond it
//! after it formed. A swept equal-high is a completed liquidity grab.

use serde::{Deserialize, Serialize};

use crate::market_structure::{find_swings, SwingKind};
use crate::types::Candle;

/// What kind of resting liquidity a level represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiquidityKind {
    /// Two or more swing highs at effectively the same price.
    EqualHighs,
    /// Two or more swing lows at effectively the same price.
    EqualLows,
    /// A single swing high.
    SwingHigh,
    /// A single swing low.
    SwingLow,
}

impl LiquidityKind {
    /// Whether this level sits above the market (a supply of sell-side
    /// liquidity, i.e. buy stops and short stops).
    #[must_use]
    pub const fn is_above(self) -> bool {
        matches!(self, Self::EqualHighs | Self::SwingHigh)
    }

    /// Whether this level sits below the market.
    #[must_use]
    pub const fn is_below(self) -> bool {
        matches!(self, Self::EqualLows | Self::SwingLow)
    }
}

/// A pool of resting liquidity at one price.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LiquidityLevel {
    /// The level price (the first swing's extreme).
    pub price: f64,
    /// What kind of level this is.
    pub kind: LiquidityKind,
    /// How many swings formed this level; `>= 2` means equal highs/lows.
    pub touches: usize,
    /// Whether price has since traded beyond the level.
    pub swept: bool,
    /// Timestamp of the first swing that formed the level.
    pub formed_at: i64,
    /// Index of the last swing that formed the level.
    pub last_index: usize,
}

/// Tuning for [`detect_liquidity_levels_with`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LiquidityConfig {
    /// Candles required on each side of a swing.
    pub lookback: usize,
    /// Two swings count as "equal" when their prices are within this fraction
    /// of each other (e.g. `0.0005` = 5 bps).
    pub tolerance: f64,
}

impl Default for LiquidityConfig {
    fn default() -> Self {
        Self {
            lookback: 3,
            tolerance: 0.0005,
        }
    }
}

/// Detect liquidity levels with the default tolerance.
#[must_use]
pub fn detect_liquidity_levels(candles: &[Candle], lookback: usize) -> Vec<LiquidityLevel> {
    detect_liquidity_levels_with(
        candles,
        LiquidityConfig {
            lookback,
            ..LiquidityConfig::default()
        },
    )
}

/// Detect liquidity levels with explicit tuning.
///
/// Levels come back in formation order (by the index of their last touch).
///
/// # Example
///
/// ```
/// use analytics_core::liquidity::{detect_liquidity_levels, LiquidityKind};
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
/// // Two swing highs at 12 -> equal highs. A later candle runs to 14.
/// let candles = vec![
///     c(10.0, 8.0, 9.0),
///     c(12.0, 9.0, 11.0),
///     c(10.0, 9.0, 9.5),
///     c(12.0, 9.0, 11.5),
///     c(11.0, 9.0, 10.0),
///     c(14.0, 11.0, 13.0),
/// ];
/// let levels = detect_liquidity_levels(&candles, 1);
///
/// assert_eq!(levels.len(), 1);
/// assert_eq!(levels[0].kind, LiquidityKind::EqualHighs);
/// assert_eq!(levels[0].touches, 2);
/// assert!(levels[0].swept);
/// ```
#[must_use]
pub fn detect_liquidity_levels_with(
    candles: &[Candle],
    config: LiquidityConfig,
) -> Vec<LiquidityLevel> {
    if candles.is_empty() || config.lookback == 0 {
        return Vec::new();
    }

    let points = find_swings(candles, config.lookback);
    if points.is_empty() {
        return Vec::new();
    }

    let tolerance = if config.tolerance.is_finite() && config.tolerance >= 0.0 {
        config.tolerance
    } else {
        LiquidityConfig::default().tolerance
    };

    let mut highs = Vec::new();
    let mut lows = Vec::new();
    for point in &points {
        match point.kind {
            SwingKind::High => highs.push((point.index, point.timestamp, point.price)),
            SwingKind::Low => lows.push((point.index, point.timestamp, point.price)),
        }
    }

    let mut levels = Vec::new();
    levels.extend(cluster(&highs, tolerance, true, candles));
    levels.extend(cluster(&lows, tolerance, false, candles));

    // Formation order: by the last touch that built the level, then price, so
    // the ordering is total and deterministic.
    levels.sort_by(|a, b| {
        a.last_index
            .cmp(&b.last_index)
            .then_with(|| a.price.total_cmp(&b.price))
    });
    levels
}

/// The nearest liquidity level strictly above `price`.
#[must_use]
pub fn nearest_liquidity_above(levels: &[LiquidityLevel], price: f64) -> Option<&LiquidityLevel> {
    levels
        .iter()
        .filter(|l| l.price > price)
        .min_by(|a, b| a.price.total_cmp(&b.price))
}

/// The nearest liquidity level strictly below `price`.
#[must_use]
pub fn nearest_liquidity_below(levels: &[LiquidityLevel], price: f64) -> Option<&LiquidityLevel> {
    levels
        .iter()
        .filter(|l| l.price < price)
        .max_by(|a, b| a.price.total_cmp(&b.price))
}

/// Cluster swings at effectively the same price into one level.
fn cluster(
    swings: &[(usize, i64, f64)],
    tolerance: f64,
    is_high: bool,
    candles: &[Candle],
) -> Vec<LiquidityLevel> {
    let mut clusters: Vec<Cluster> = Vec::new();

    for &(index, timestamp, price) in swings {
        let matched = clusters
            .iter_mut()
            .rev()
            .find(|c| within(c.price, price, tolerance));

        if let Some(cluster) = matched {
            cluster.touches += 1;
            cluster.last_index = index;
        } else {
            clusters.push(Cluster {
                price,
                touches: 1,
                last_index: index,
                formed_at: timestamp,
            });
        }
    }

    clusters
        .into_iter()
        .map(|cluster| {
            let kind = match (is_high, cluster.touches >= 2) {
                (true, true) => LiquidityKind::EqualHighs,
                (true, false) => LiquidityKind::SwingHigh,
                (false, true) => LiquidityKind::EqualLows,
                (false, false) => LiquidityKind::SwingLow,
            };
            LiquidityLevel {
                price: cluster.price,
                kind,
                touches: cluster.touches,
                swept: is_swept(
                    candles,
                    cluster.last_index,
                    cluster.price,
                    tolerance,
                    is_high,
                ),
                formed_at: cluster.formed_at,
                last_index: cluster.last_index,
            }
        })
        .collect()
}

/// Whether price traded beyond the level after the level's last touch.
fn is_swept(
    candles: &[Candle],
    last_index: usize,
    price: f64,
    tolerance: f64,
    is_high: bool,
) -> bool {
    let Some(after) = candles.get(last_index + 1..) else {
        return false;
    };

    if is_high {
        let trigger = price * (1.0 + tolerance);
        after.iter().any(|c| c.high > trigger)
    } else {
        let trigger = price * (1.0 - tolerance);
        after.iter().any(|c| c.low < trigger)
    }
}

/// Relative closeness: within `tolerance` of either price.
fn within(a: f64, b: f64, tolerance: f64) -> bool {
    let scale = a.abs().max(b.abs());
    (a - b).abs() <= tolerance * scale
}

/// Working state while clustering swings.
struct Cluster {
    price: f64,
    touches: usize,
    last_index: usize,
    formed_at: i64,
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

    /// Two equal highs at 12, then an optional run beyond them.
    fn equal_highs(with_sweep: bool) -> Vec<Candle> {
        let mut candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(10.0, 9.0, 9.5),
            c(12.0, 9.0, 11.5),
            c(11.0, 9.0, 10.0),
        ];
        if with_sweep {
            candles.push(c(14.0, 11.0, 13.0));
        }
        candles
    }

    /// Two equal lows at 8, then an optional flush below them.
    fn equal_lows(with_sweep: bool) -> Vec<Candle> {
        let mut candles = vec![
            c(12.0, 10.0, 11.0),
            c(11.0, 8.0, 9.0),
            c(10.5, 9.0, 9.5),
            c(10.0, 8.0, 9.0),
            c(9.5, 9.0, 9.5),
        ];
        if with_sweep {
            candles.push(c(10.0, 6.0, 7.0));
        }
        candles
    }

    #[test]
    fn equal_highs_are_clustered_into_one_level() {
        let levels = detect_liquidity_levels(&equal_highs(false), 1);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].kind, LiquidityKind::EqualHighs);
        assert_eq!(levels[0].touches, 2);
        assert!((levels[0].price - 12.0).abs() < 1e-9);
        assert!(levels[0].kind.is_above());
        assert!(!levels[0].kind.is_below());
    }

    #[test]
    fn a_level_is_swept_once_price_trades_beyond_it() {
        assert!(!detect_liquidity_levels(&equal_highs(false), 1)[0].swept);
        assert!(detect_liquidity_levels(&equal_highs(true), 1)[0].swept);
    }

    #[test]
    fn a_single_swing_is_reported_as_a_plain_swing() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 9.0, 10.0),
            c(11.0, 9.0, 10.0),
        ];
        let levels = detect_liquidity_levels(&candles, 1);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].kind, LiquidityKind::SwingHigh);
        assert_eq!(levels[0].touches, 1);
    }

    #[test]
    fn equal_lows_are_detected_below() {
        let levels = detect_liquidity_levels(&equal_lows(false), 1);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].kind, LiquidityKind::EqualLows);
        assert!(levels[0].kind.is_below());
        assert!((levels[0].price - 8.0).abs() < 1e-9);
    }

    #[test]
    fn equal_lows_are_swept_by_a_lower_low() {
        assert!(!detect_liquidity_levels(&equal_lows(false), 1)[0].swept);
        assert!(detect_liquidity_levels(&equal_lows(true), 1)[0].swept);
    }

    #[test]
    fn prices_outside_the_tolerance_are_separate_levels() {
        // 12.0 and 12.5 differ by 4% -- far beyond 5 bps.
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(10.0, 9.0, 9.5),
            c(12.5, 9.0, 11.5),
            c(11.0, 9.0, 10.0),
        ];
        let levels = detect_liquidity_levels(&candles, 1);
        assert_eq!(levels.len(), 2);
        assert!(levels.iter().all(|l| l.touches == 1));
    }

    #[test]
    fn tolerance_is_configurable() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(10.0, 9.0, 9.5),
            c(12.5, 9.0, 11.5),
            c(11.0, 9.0, 10.0),
        ];
        let config = LiquidityConfig {
            lookback: 1,
            tolerance: 0.05,
        };
        let levels = detect_liquidity_levels_with(&candles, config);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].touches, 2);
    }

    #[test]
    fn levels_are_returned_in_formation_order() {
        let candles = vec![
            c(10.0, 8.0, 9.0),
            c(12.0, 9.0, 11.0),
            c(11.0, 7.0, 8.0),
            c(10.0, 9.0, 9.5),
            c(11.0, 9.0, 10.0),
        ];
        let levels = detect_liquidity_levels(&candles, 1);
        assert!(levels
            .windows(2)
            .all(|w| w[0].last_index <= w[1].last_index));
    }

    #[test]
    fn nearest_helpers_pick_the_closest_level_each_way() {
        let levels = vec![
            LiquidityLevel {
                price: 90.0,
                kind: LiquidityKind::EqualLows,
                touches: 2,
                swept: false,
                formed_at: 0,
                last_index: 3,
            },
            LiquidityLevel {
                price: 110.0,
                kind: LiquidityKind::EqualHighs,
                touches: 2,
                swept: false,
                formed_at: 0,
                last_index: 5,
            },
        ];
        assert!((nearest_liquidity_above(&levels, 100.0).unwrap().price - 110.0).abs() < 1e-9);
        assert!((nearest_liquidity_below(&levels, 100.0).unwrap().price - 90.0).abs() < 1e-9);
        assert!(nearest_liquidity_above(&levels, 110.0).is_none());
        assert!(nearest_liquidity_below(&levels, 90.0).is_none());
    }

    #[test]
    fn too_few_candles_yield_nothing() {
        assert!(detect_liquidity_levels(&[], 1).is_empty());
        assert!(detect_liquidity_levels(&[c(10.0, 9.0, 9.5)], 1).is_empty());
        assert!(detect_liquidity_levels(&equal_highs(false), 0).is_empty());
    }

    #[test]
    fn a_level_never_sweeps_itself() {
        // The last touch is the final candle, so there is nothing after it.
        let candles = vec![c(10.0, 8.0, 9.0), c(12.0, 9.0, 11.0), c(10.0, 9.0, 9.5)];
        let levels = detect_liquidity_levels(&candles, 1);
        assert_eq!(levels.len(), 1);
        assert!(!levels[0].swept);
    }
}
