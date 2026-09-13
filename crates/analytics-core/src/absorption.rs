//! Absorption -- where aggression ran into a wall and lost.
//!
//! Absorption is the failure of aggression. Sellers dump size into the bid and
//! price *does not* make a new low: someone with a limit order absorbed
//! everything they threw. The tell is a price level carrying far more volume
//! than the levels around it, at the extreme of the candle, with the candle
//! refusing to extend.
//!
//! ## The definition, precisely
//!
//! Bullish absorption at candle `i` requires **all** of:
//!
//! 1. The lowest traded level of candle `i` carries at least
//!    `volume_ratio` times the average volume of candle `i`'s traded levels.
//! 2. Candle `i`'s low does not break candle `i-1`'s low by more than
//!    `price_tolerance` -- the aggression failed to extend.
//! 3. Candle `i`'s delta improved on candle `i-1`'s by at least
//!    `min_delta_improvement` -- the pressure is turning.
//!
//! Bearish absorption is the mirror image at the high.
//!
//! The delta condition is the one that needs care. At a *bullish* absorption
//! the raw delta is usually still **negative** -- sellers are the aggressors,
//! that is the whole point. What matters is that it is *improving*: the sellers
//! are working harder for less. Requiring a positive delta would miss the
//! signal entirely, which is why the config compares against the previous
//! candle rather than against zero.

use serde::{Deserialize, Serialize};

use crate::footprint::FootprintCandle;
use crate::types::Side;

/// Tuning for [`detect_absorption`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AbsorptionConfig {
    /// A level must carry at least this multiple of the candle's average
    /// traded-level volume to count as "heavy".
    pub volume_ratio: f64,
    /// How far past the previous extreme price may go and still count as a
    /// failure to extend, as a fraction of price (e.g. `0.0005` = 5 bps).
    pub price_tolerance: f64,
    /// Minimum improvement in the candle's delta versus the previous candle.
    /// `0.0` means "delta must not deteriorate"; the condition is always
    /// active.
    pub min_delta_improvement: f64,
}

impl Default for AbsorptionConfig {
    fn default() -> Self {
        Self {
            volume_ratio: 2.0,
            price_tolerance: 0.0005,
            min_delta_improvement: 0.0,
        }
    }
}

/// One detected absorption.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AbsorptionEvent {
    /// Index into the slice passed to [`detect_absorption`].
    pub candle_index: usize,
    /// Timestamp of the absorbing candle.
    pub timestamp: i64,
    /// The level that absorbed the flow.
    pub price_level: f64,
    /// `Buy` = sellers were absorbed at a low (bullish).
    /// `Sell` = buyers were absorbed at a high (bearish).
    pub side: Side,
    /// Volume at the absorbing level.
    pub volume: f64,
    /// `volume / average traded-level volume` in this candle.
    pub strength: f64,
    /// Delta at the absorbing level itself.
    pub cell_delta: f64,
    /// The candle's overall delta.
    pub candle_delta: f64,
    /// `candle_delta - previous candle's delta`.
    pub delta_improvement: f64,
}

impl AbsorptionEvent {
    /// Whether this is bullish absorption (sellers absorbed at a low).
    #[must_use]
    pub const fn is_bullish(&self) -> bool {
        matches!(self.side, Side::Buy)
    }

    /// Whether this is bearish absorption (buyers absorbed at a high).
    #[must_use]
    pub const fn is_bearish(&self) -> bool {
        matches!(self.side, Side::Sell)
    }
}

/// Detect absorption across a sequence of footprints.
///
/// Needs at least two candles, because the definition compares each candle
/// against its predecessor.
///
/// # Example
///
/// ```
/// use analytics_core::absorption::{detect_absorption, AbsorptionConfig};
/// use analytics_core::footprint::FootprintCandle;
/// use analytics_core::types::{Candle, FootprintCell, Timeframe};
///
/// fn fp(low: f64, high: f64, buy: f64, sell: f64, levels: &[(f64, f64)]) -> FootprintCandle {
///     let cells = levels
///         .iter()
///         .enumerate()
///         .map(|(i, (bid, ask))| FootprintCell {
///             price_level: low + i as f64 + 0.5,
///             bid_volume: *bid,
///             ask_volume: *ask,
///             delta: ask - bid,
///         })
///         .collect();
///     FootprintCandle {
///         candle: Candle {
///             symbol: "BTCUSDT".into(),
///             timeframe: Timeframe::M1,
///             open_time: 0,
///             open: low,
///             high,
///             low,
///             close: high,
///             volume: buy + sell,
///             buy_volume: buy,
///             sell_volume: sell,
///         },
///         cells,
///         imbalances: Vec::new(),
///     }
/// }
///
/// // Previous candle: sellers in control, delta -45.
/// let prev = fp(99.0, 101.0, 0.0, 45.0, &[(10.0, 0.0), (20.0, 0.0), (15.0, 0.0)]);
/// // This candle: heavy sell volume sits on the low, the low holds, delta improves.
/// let cur = fp(100.0, 102.0, 0.0, 24.0, &[(20.0, 0.0), (3.0, 0.0), (1.0, 0.0)]);
///
/// let events = detect_absorption(&[prev, cur], AbsorptionConfig::default());
/// assert_eq!(events.len(), 1);
/// assert!(events[0].is_bullish());
/// assert!((events[0].price_level - 100.5).abs() < 1e-9);
/// ```
#[must_use]
pub fn detect_absorption(
    candles: &[FootprintCandle],
    config: AbsorptionConfig,
) -> Vec<AbsorptionEvent> {
    let mut events = Vec::new();
    if candles.len() < 2 {
        return events;
    }

    for index in 1..candles.len() {
        let current = &candles[index];
        let previous = &candles[index - 1];

        let mean = mean_traded_volume(&current.cells);
        let Some(mean) = mean else {
            continue;
        };

        let (Some(low_cell), Some(high_cell)) = (
            current.cells.iter().find(|c| c.total_volume() > 0.0),
            current.cells.iter().rev().find(|c| c.total_volume() > 0.0),
        ) else {
            continue;
        };

        let threshold = config.volume_ratio * mean;
        let candle_delta = current.candle.delta();
        let delta_improvement = candle_delta - previous.candle.delta();

        // Bullish: heavy sell volume at the low, the low holds, delta improves.
        if low_cell.total_volume() >= threshold
            && previous.candle.low > 0.0
            && current.candle.low >= previous.candle.low * (1.0 - config.price_tolerance)
            && delta_improvement >= config.min_delta_improvement
        {
            events.push(AbsorptionEvent {
                candle_index: index,
                timestamp: current.candle.open_time,
                price_level: low_cell.price_level,
                side: Side::Buy,
                volume: low_cell.total_volume(),
                strength: low_cell.total_volume() / mean,
                cell_delta: low_cell.delta,
                candle_delta,
                delta_improvement,
            });
        }

        // Bearish: heavy buy volume at the high, the high holds, delta decays.
        if high_cell.total_volume() >= threshold
            && previous.candle.high > 0.0
            && current.candle.high <= previous.candle.high * (1.0 + config.price_tolerance)
            && -delta_improvement >= config.min_delta_improvement
        {
            events.push(AbsorptionEvent {
                candle_index: index,
                timestamp: current.candle.open_time,
                price_level: high_cell.price_level,
                side: Side::Sell,
                volume: high_cell.total_volume(),
                strength: high_cell.total_volume() / mean,
                cell_delta: high_cell.delta,
                candle_delta,
                delta_improvement,
            });
        }
    }

    events
}

/// Mean volume across the levels that actually traded.
///
/// Empty levels are excluded on purpose: they are structural placeholders (see
/// [`footprint`](crate::footprint)), and averaging them in would make "heavy"
/// depend on how wide the caller's bucket size happens to be.
fn mean_traded_volume(cells: &[crate::types::FootprintCell]) -> Option<f64> {
    let mut total = 0.0;
    let mut count = 0usize;
    for cell in cells {
        let volume = cell.total_volume();
        if volume > 0.0 {
            total += volume;
            count += 1;
        }
    }

    if count == 0 {
        return None;
    }

    #[allow(clippy::cast_precision_loss)]
    Some(total / count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, FootprintCell, Timeframe};

    /// Footprint with `levels` as ascending `(bid, ask)` pairs starting at `low`.
    fn fp(low: f64, high: f64, buy: f64, sell: f64, levels: &[(f64, f64)]) -> FootprintCandle {
        let cells = levels
            .iter()
            .enumerate()
            .map(|(i, (bid, ask))| {
                #[allow(clippy::cast_precision_loss)]
                let price_level = low + i as f64 + 0.5;
                FootprintCell {
                    price_level,
                    bid_volume: *bid,
                    ask_volume: *ask,
                    delta: ask - bid,
                }
            })
            .collect();
        FootprintCandle {
            candle: Candle {
                symbol: "BTCUSDT".into(),
                timeframe: Timeframe::M1,
                open_time: 0,
                open: low,
                high,
                low,
                close: high,
                volume: buy + sell,
                buy_volume: buy,
                sell_volume: sell,
            },
            cells,
            imbalances: Vec::new(),
        }
    }

    /// Sellers heavy, delta -45.
    fn seller_candle() -> FootprintCandle {
        fp(
            99.0,
            101.0,
            0.0,
            45.0,
            &[(10.0, 0.0), (20.0, 0.0), (15.0, 0.0)],
        )
    }

    /// Heavy sell volume parked on the low; low holds; delta improves to -24.
    fn bullish_absorption_candle() -> FootprintCandle {
        fp(
            100.0,
            102.0,
            0.0,
            24.0,
            &[(20.0, 0.0), (3.0, 0.0), (1.0, 0.0)],
        )
    }

    #[test]
    fn detects_bullish_absorption_at_the_low() {
        let events = detect_absorption(
            &[seller_candle(), bullish_absorption_candle()],
            AbsorptionConfig::default(),
        );
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert!(event.is_bullish());
        assert!(!event.is_bearish());
        assert_eq!(event.candle_index, 1);
        assert!((event.price_level - 100.5).abs() < 1e-9);
        assert!((event.volume - 20.0).abs() < 1e-9);
        assert!((event.strength - 2.5).abs() < 1e-9);
        assert!((event.delta_improvement - 21.0).abs() < 1e-9);
    }

    #[test]
    fn detects_bearish_absorption_at_the_high() {
        // Mirror: buyers push into the high, the high holds, delta decays.
        let prev = fp(
            99.0,
            101.0,
            45.0,
            0.0,
            &[(15.0, 0.0), (20.0, 0.0), (10.0, 0.0)],
        );
        // High level carries 20 while the others carry 3 and 1.
        let cur = fp(
            98.0,
            101.0,
            24.0,
            0.0,
            &[(1.0, 0.0), (3.0, 0.0), (20.0, 0.0)],
        );
        let events = detect_absorption(&[prev, cur], AbsorptionConfig::default());
        assert_eq!(events.len(), 1);
        assert!(events[0].is_bearish());
        assert!((events[0].price_level - 100.5).abs() < 1e-9);
    }

    #[test]
    fn no_absorption_when_the_extreme_is_broken() {
        // Same heavy volume, but the low takes out the previous low -> no event.
        let cur = fp(
            95.0,
            102.0,
            0.0,
            24.0,
            &[(20.0, 0.0), (3.0, 0.0), (1.0, 0.0)],
        );
        assert!(detect_absorption(&[seller_candle(), cur], AbsorptionConfig::default()).is_empty());
    }

    #[test]
    fn no_absorption_when_delta_deteriorates() {
        // The low holds, but sellers are getting stronger, not weaker.
        let cur = fp(
            100.0,
            102.0,
            0.0,
            60.0,
            &[(50.0, 0.0), (8.0, 0.0), (2.0, 0.0)],
        );
        assert!(detect_absorption(&[seller_candle(), cur], AbsorptionConfig::default()).is_empty());
    }

    #[test]
    fn no_absorption_when_the_level_is_not_heavy() {
        // Uniform-ish volume: the low is not special.
        let cur = fp(
            100.0,
            102.0,
            0.0,
            30.0,
            &[(10.0, 0.0), (10.0, 0.0), (10.0, 0.0)],
        );
        assert!(detect_absorption(&[seller_candle(), cur], AbsorptionConfig::default()).is_empty());
    }

    #[test]
    fn volume_ratio_is_configurable() {
        let cur = fp(
            100.0,
            102.0,
            0.0,
            30.0,
            &[(10.0, 0.0), (10.0, 0.0), (10.0, 0.0)],
        );
        let lenient = AbsorptionConfig {
            volume_ratio: 1.0,
            ..AbsorptionConfig::default()
        };
        // 10 >= 1.0 * 10, so the level now qualifies as heavy.
        assert_eq!(detect_absorption(&[seller_candle(), cur], lenient).len(), 1);
    }

    #[test]
    fn price_tolerance_is_configurable() {
        // Low dips 0.2% below the previous low.
        let cur = fp(
            98.8,
            102.0,
            0.0,
            24.0,
            &[(20.0, 0.0), (3.0, 0.0), (1.0, 0.0)],
        );
        assert!(
            detect_absorption(&[seller_candle(), cur.clone()], AbsorptionConfig::default())
                .is_empty()
        );

        let tolerant = AbsorptionConfig {
            price_tolerance: 0.005,
            ..AbsorptionConfig::default()
        };
        assert_eq!(
            detect_absorption(&[seller_candle(), cur], tolerant).len(),
            1
        );
    }

    #[test]
    fn delta_improvement_threshold_is_enforced() {
        let cur = bullish_absorption_candle();
        let strict = AbsorptionConfig {
            min_delta_improvement: 30.0,
            ..AbsorptionConfig::default()
        };
        // Improvement is only +21.
        assert!(detect_absorption(&[seller_candle(), cur], strict).is_empty());
    }

    #[test]
    fn fewer_than_two_candles_yields_nothing() {
        assert!(detect_absorption(&[], AbsorptionConfig::default()).is_empty());
        assert!(
            detect_absorption(&[bullish_absorption_candle()], AbsorptionConfig::default())
                .is_empty()
        );
    }

    #[test]
    fn a_candle_with_no_trades_is_skipped() {
        let empty = fp(100.0, 100.0, 0.0, 0.0, &[]);
        assert!(
            detect_absorption(&[seller_candle(), empty], AbsorptionConfig::default()).is_empty()
        );
    }

    #[test]
    fn strength_is_volume_over_the_traded_level_mean() {
        let events = detect_absorption(
            &[seller_candle(), bullish_absorption_candle()],
            AbsorptionConfig::default(),
        );
        // Levels 20/3/1 -> mean 8; the absorbing level is 20 -> 2.5x.
        assert!((events[0].strength - 2.5).abs() < 1e-9);
    }
}
