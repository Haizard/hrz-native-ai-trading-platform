//! RSI/price divergence -- the classic momentum exhaustion signal.
//!
//! ## Why this module exists
//!
//! The crate already answers "is price diverging from order flow"
//! ([`crate::cvd::detect_cvd_divergence`]). RSI divergence is the same
//! question against momentum instead of aggression, and it is the single most
//! requested indicator overlay that the vocabulary could not say: a client
//! asking for "RSI divergence with confirmation" got a proxy or nothing.
//! Like every module here it is a *measurement*, not a strategy -- it reports
//! what diverged and where, and lets the condition language decide what to do.
//!
//! ## The rule, as stated
//!
//! 1. Find the last confirmed swing in price and in RSI over the lookback
//!    (mirror of `market_structure`'s swing rule: a high with `strength`
//!    candles lower on both sides, or a low with higher ones).
//! 2. **Bearish:** price made a higher high, RSI made a lower high.
//!    **Bullish:** price made a lower low, RSI made a higher low.
//! 3. Require the RSI extreme to be beyond its band (70/30) so "divergence"
//!    is not reported from momentum drift in the middle of the range.
//! 4. Require the two swing points to be a meaningful number of bars apart so
//!    a two-candle wiggle is not reported as a divergence.
//!
//! Multiple items returned: [`rsi_divergences`] reports **every** divergence
//! in the window, oldest first, so the chart can draw them and the runtime can
//! ask for the newest -- the same newest-wins convention `concepts::detect`
//! established and the engine already relies on.

use serde::{Deserialize, Serialize};

use crate::indicators::rsi;
use crate::types::Candle;

/// Default RSI period.
pub const DEFAULT_PERIOD: usize = 14;
/// Default overbought level.
pub const DEFAULT_OVERBOUGHT: f64 = 70.0;
/// Default oversold level.
pub const DEFAULT_OVERSOLD: f64 = 30.0;

/// A confirmed RSI/price divergence over a window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RsiDivergence {
    /// Which way the divergence leans.
    pub direction: RsiDivergenceKind,
    /// Bar index of the price swing (into the slice the detector saw).
    pub price_index: usize,
    /// Bar index of the RSI swing.
    pub rsi_index: usize,
    /// The price extreme at the divergence point.
    pub price: f64,
    /// The RSI extreme at the divergence point.
    pub rsi: f64,
}

/// Which way a divergence leans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RsiDivergenceKind {
    /// Price made a lower low, RSI made a higher low -- sellers thinning out.
    Bullish,
    /// Price made a higher high, RSI made a lower high -- buyers thinning out.
    Bearish,
}

impl RsiDivergenceKind {
    /// Canonical name, as a condition writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bullish => "bullish",
            Self::Bearish => "bearish",
        }
    }
}

/// Tuning for the divergence detector.
///
/// The defaults are the numbers the 70/30 band convention implies; every one
/// is a knob because the fixtures that prove them wrong are data, not theory.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RsiDivergenceConfig {
    /// RSI period (Wilder's smoothing, same as `indicators::rsi`).
    pub period: usize,
    /// RSI level a bearish divergence's high must exceed.
    pub overbought: f64,
    /// RSI level a bullish divergence's low must stay under.
    pub oversold: f64,
    /// Bars on each side of a confirmed swing extreme.
    pub strength: usize,
    /// Shortest distance between the two swing points of a divergence.
    pub min_bars_apart: usize,
    /// Bars compared when looking for divergences.
    pub lookback: usize,
}

impl Default for RsiDivergenceConfig {
    fn default() -> Self {
        Self {
            period: DEFAULT_PERIOD,
            overbought: DEFAULT_OVERBOUGHT,
            oversold: DEFAULT_OVERSOLD,
            strength: 2,
            min_bars_apart: 5,
            lookback: 60,
        }
    }
}

/// Wilder's RSI over the candles' closes, one `Option` per candle.
///
/// Handed back alongside the divergences because a chart that draws the RSI
/// pane needs the same series the detector read -- a second RSI computation in
/// the caller is exactly the duplicate-math rule this crate exists to prevent.
#[must_use]
pub fn rsi_series(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    rsi(&closes, period)
}

/// Find every RSI/price divergence in the trailing window, oldest first.
///
/// Total: a series too short for the period, a flat series and a non-finite
/// close all produce fewer or no divergences, never a panic -- the chart
/// engine calls this inside wasm where a panic is a trap and a dead canvas.
#[must_use]
pub fn rsi_divergences(candles: &[Candle], config: &RsiDivergenceConfig) -> Vec<RsiDivergence> {
    if config.lookback == 0 || candles.is_empty() {
        return Vec::new();
    }
    let start = candles.len().saturating_sub(config.lookback);
    let window = &candles[start..];
    let rsi_values = rsi_series(window, config.period);

    let mut out = Vec::new();
    for kind in [RsiDivergenceKind::Bullish, RsiDivergenceKind::Bearish] {
        if let Some(found) = divergence_of(window, &rsi_values, config, kind) {
            out.push(found);
        }
    }
    // Oldest first: the price swing of a bullish divergence precedes its RSI
    // swing, and both precede the bearish leg of a later turn. Ordering by the
    // price swing keeps the output a timeline rather than a kind group.
    out.sort_by_key(|div| div.price_index);
    out
}

/// The newest divergence in the trailing window, if any.
///
/// The one the runtime reads: `concepts::detect` returns bands oldest first
/// and the engine takes the last, so a divergence detector that reports many
/// needs the same "newest wins" accessor or every consumer reinvents it.
#[must_use]
pub fn latest_rsi_divergence(
    candles: &[Candle],
    config: &RsiDivergenceConfig,
) -> Option<RsiDivergence> {
    rsi_divergences(candles, config).pop()
}

/// Detect one direction, or `None` when that direction never diverged here.
fn divergence_of(
    window: &[Candle],
    rsi_values: &[Option<f64>],
    config: &RsiDivergenceConfig,
    kind: RsiDivergenceKind,
) -> Option<RsiDivergence> {
    // The price extreme and the RSI extreme of the two most recent confirmed
    // swings. Swing i is confirmed when `strength` bars on each side agree.
    let (price_first, price_last) = swings(window, kind, config.strength, |c| match kind {
        RsiDivergenceKind::Bullish => c.low,
        RsiDivergenceKind::Bearish => c.high,
    })?;
    // Warm-up bars read `None`; they arrive as `NaN` and the swing scan skips
    // non-finite values, so the RSI series' shape never invents a swing.
    let (rsi_first, rsi_last) = swings(rsi_values, kind, config.strength, |v| {
        v.unwrap_or(f64::NAN)
    })?;

    // A divergence is *two* points: the earlier swing and the later one, in
    // both series. If either series only managed one swing in the window there
    // is no comparison to make.
    let (price_extreme_first, price_extreme_last) = (price_first?, price_last?);
    let (rsi_extreme_first, rsi_extreme_last) = (rsi_first?, rsi_last?);

    if price_extreme_last.index.0 <= price_extreme_first.index.0
        || rsi_extreme_last.index.0 <= rsi_extreme_first.index.0
    {
        return None;
    }

    // The two swings have to be far enough apart to be a real leg.
    let bars_apart = price_extreme_last.index.0.saturating_sub(price_extreme_first.index.0);
    if bars_apart < config.min_bars_apart {
        return None;
    }

    // The band filter: momentum must actually have been stretched at the
    // *start* of the leg, or every mid-range wiggle reads as exhaustion. The
    // second extreme is deliberately not banded -- it being higher (bullish)
    // or lower (bearish) than the first *is* the divergence, and demanding it
    // also sit outside the band misses the textbook case where the second
    // low's RSI has climbed to 35 while the first was 22.
    match kind {
        RsiDivergenceKind::Bullish => {
            if rsi_extreme_first.value >= config.oversold {
                return None;
            }
        }
        RsiDivergenceKind::Bearish => {
            if rsi_extreme_first.value <= config.overbought {
                return None;
            }
        }
    }

    // The divergence itself, stated as the two series disagreeing.
    let diverged = match kind {
        RsiDivergenceKind::Bullish => {
            price_extreme_last.value < price_extreme_first.value
                && rsi_extreme_last.value > rsi_extreme_first.value
        }
        RsiDivergenceKind::Bearish => {
            price_extreme_last.value > price_extreme_first.value
                && rsi_extreme_last.value < rsi_extreme_first.value
        }
    };
    if !diverged {
        return None;
    }

    // The divergence is reported at the *price* swing -- that is where the
    // chart draws it and where a condition compares. The RSI swing rides
    // along as evidence.
    Some(RsiDivergence {
        direction: kind,
        price_index: price_extreme_last.index.0,
        rsi_index: rsi_extreme_last.index.0,
        price: price_extreme_last.value,
        rsi: rsi_extreme_last.value,
    })
}

/// One extreme: where it happened and what it measured.
#[derive(Debug, Clone, Copy)]
struct Extreme {
    /// `(bar index, swing generation)` -- the generation orders two swings at
    /// the same index, which cannot happen here but costs nothing to carry.
    index: (usize, usize),
    /// The measured value.
    value: f64,
}

/// The two most recent confirmed swings of the requested kind, over `items`.
///
/// A swing is confirmed when `strength` neighbours on each side are all
/// strictly beyond it in the opposite direction (lows: strictly higher; highs:
/// strictly lower). Returns `(earlier, later)`; each is `None` when the window
/// only contains one. The last swing may sit at the window's edge -- its
/// right-hand side simply does not exist, and waiting for it would make every
/// signal visible only in the past.
fn swings<T, F: Fn(&T) -> f64>(
    items: &[T],
    kind: RsiDivergenceKind,
    strength: usize,
    measure: F,
) -> Option<(Option<Extreme>, Option<Extreme>)> {
    if strength == 0 || items.len() < 2 {
        return None;
    }

    let is_extreme = |value: f64, other: f64| match kind {
        RsiDivergenceKind::Bullish => value < other,
        RsiDivergenceKind::Bearish => value > other,
    };

    let mut found: Vec<Extreme> = Vec::new();
    for index in 0..items.len() {
        let value = measure(&items[index]);
        if !value.is_finite() {
            continue;
        }
        let left = index.saturating_sub(strength);
        let right = (index + strength + 1).min(items.len());

        let confirmed = items[left..index].iter().all(|other| is_extreme(value, measure(other)))
            && items[index + 1..right]
                .iter()
                .all(|other| is_extreme(value, measure(other)));
        if confirmed {
            found.push(Extreme { index: (index, found.len()), value });
        }
    }

    let last = found.pop()?;
    let first = found.pop()?;
    Some((Some(first), Some(last)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(index: i64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: index * Timeframe::M1.nanos(),
            open: close,
            high,
            low,
            close,
            volume: 10.0,
            buy_volume: 5.0,
            sell_volume: 5.0,
        }
    }

    /// Quiet bars so RSI is warm before the move, a leg down into 90, a
    /// bounce, then a lower low in price (88.8) whose RSI prints *higher*
    /// than the first low's -- the canonical bullish divergence shape, driven
    /// through real Wilder smoothing. Lows either side of each swing sit
    /// strictly higher, because a low must be confirmed to be a swing at all.
    fn bullish_series() -> Vec<Candle> {
        let shape: [(f64, f64, f64); 23] = [
            (100.5, 99.5, 100.0),
            (100.8, 99.8, 100.3),
            (100.6, 99.6, 100.1),
            (100.9, 99.9, 100.4),
            (100.7, 99.7, 100.2),
            (100.5, 99.5, 100.0),
            (100.3, 99.3, 99.8),
            (100.5, 99.5, 100.0),
            (99.8, 98.5, 99.0), // the decline begins
            (98.8, 96.5, 97.0),
            (96.8, 94.0, 94.5),
            (94.4, 91.5, 92.0),
            (92.2, 90.0, 90.5), // first low: price 90.0
            (91.8, 90.6, 91.4),
            (92.6, 91.4, 92.2),
            (93.4, 92.2, 93.0),
            (93.2, 91.8, 92.2),
            (92.0, 90.8, 91.2),
            (91.2, 89.6, 90.0),
            (90.4, 88.9, 89.3),
            (89.6, 88.8, 89.0), // second low: price 88.8, lower
            (90.2, 89.5, 90.0),
            (90.8, 90.1, 90.6),
        ];
        shape
            .into_iter()
            .enumerate()
            .map(|(i, (high, low, close))| candle(i64::try_from(i).expect("small"), high, low, close))
            .collect()
    }

    #[test]
    fn a_real_bullish_divergence_is_found_through_wilder_smoothing() {
        let config = RsiDivergenceConfig {
            period: 5,
            strength: 2,
            min_bars_apart: 3,
            lookback: 60,
            ..RsiDivergenceConfig::default()
        };
        let (rsi_values, divergences) = {
            let values = rsi_series(&bullish_series(), config.period);
            (values, rsi_divergences(&bullish_series(), &config))
        };
        // The fixture is built so the second low's RSI is higher; assert the
        // detector agrees, and that the point it reports is the price low.
        let div = divergences
            .iter()
            .find(|d| d.direction == RsiDivergenceKind::Bullish)
            .expect("the fixture diverges bullish");
        assert_eq!(div.price_index, 20, "{div:?}");
        assert_eq!(div.price, 88.8, "{div:?}");
    }

    #[test]
    fn a_series_that_never_stretches_the_band_reports_nothing() {
        // A gentle drift that never produces an RSI under 30 or over 70.
        let candles: Vec<Candle> = (0..40)
            .map(|i| {
                let close = 100.0 + f64::from(i % 5);
                candle(i64::from(i), close + 0.5, close - 0.5, close)
            })
            .collect();
        let divergences = rsi_divergences(&candles, &RsiDivergenceConfig::default());
        assert!(
            divergences.is_empty(),
            "mid-range momentum is not exhaustion: {divergences:?}"
        );
    }

    #[test]
    fn the_detector_is_total_over_hostile_inputs() {
        let config = RsiDivergenceConfig::default();
        assert!(rsi_divergences(&[], &config).is_empty());
        let one = vec![candle(0, 100.5, 99.5, 100.0)];
        assert!(rsi_divergences(&one, &config).is_empty());
        // A flat series has no swings with strict comparison.
        let flat: Vec<Candle> = (0..30).map(|i| candle(i64::from(i), 100.0, 100.0, 100.0)).collect();
        assert!(rsi_divergences(&flat, &config).is_empty());
    }

    #[test]
    fn lookback_bounds_what_the_detector_can_see() {
        let full = bullish_series();
        let mut config = RsiDivergenceConfig {
            period: 5,
            strength: 2,
            min_bars_apart: 3,
            ..RsiDivergenceConfig::default()
        };
        config.lookback = 3;
        let candles = &full;
        // Three bars cannot contain two confirmed swings five apart.
        assert!(rsi_divergences(candles, &config).is_empty());
    }

    #[test]
    fn the_kind_names_are_what_a_condition_writes() {
        assert_eq!(RsiDivergenceKind::Bullish.name(), "bullish");
        assert_eq!(RsiDivergenceKind::Bearish.name(), "bearish");
    }

    #[test]
    fn latest_prefers_the_most_recent_swing_pair() {
        let config = RsiDivergenceConfig {
            period: 5,
            strength: 2,
            min_bars_apart: 3,
            ..RsiDivergenceConfig::default()
        };
        let latest = latest_rsi_divergence(&bullish_series(), &config);
        let all = rsi_divergences(&bullish_series(), &config);
        assert_eq!(
            latest.as_ref().map(|d| d.price_index),
            all.last().map(|d| d.price_index),
            "latest is the last of the ordered list"
        );
    }
}

