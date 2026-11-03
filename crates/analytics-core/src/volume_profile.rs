//! Volume Profile -- how much volume traded at each price.
//!
//! Answers a different question from a candlestick chart. A candle says *when*
//! volume happened; a volume profile says *where*. The price levels where the
//! most volume changed hands (the POC, and the value area around it) are where
//! the market found agreement, and they behave as magnets and as rejection
//! levels afterwards.
//!
//! ## Bucketing
//!
//! Prices are grouped into fixed-width buckets spanning the observed range.
//! Buckets are **contiguous**, including empty ones -- a gap in the histogram
//! is information (a price the market skipped), so it must not be collapsed
//! away. Each node's `price_level` is the bucket **midpoint**.
//!
//! ## Value area
//!
//! The value area is grown outward from the POC, always taking whichever
//! neighbouring bucket has more volume, until it covers `value_area_pct`
//! (default 70%) of total volume. This is the standard Market Profile
//! algorithm.

use serde::{Deserialize, Serialize};

use crate::types::{Candle, Trade};

/// Default share of volume the value area should cover.
pub const DEFAULT_VALUE_AREA_PCT: f64 = 0.70;

/// Upper bound on histogram size.
///
/// A tiny `bucket_size` over a wide price range would otherwise allocate an
/// enormous vector (a `0.01` bucket across BTC's full history is millions of
/// nodes). Exceeding this returns an empty profile rather than exhausting
/// memory -- see the resource-limit requirements in `docs/08-SANDBOX-WASM.md`.
pub const MAX_BUCKETS: usize = 100_000;

/// One price bucket's traded volume.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolumeNode {
    /// Bucket midpoint.
    pub price_level: f64,
    /// Total volume traded in this bucket.
    pub volume: f64,
    /// Buy-aggressed volume.
    pub buy_volume: f64,
    /// Sell-aggressed volume.
    pub sell_volume: f64,
}

/// A volume-by-price histogram with its derived levels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeProfile {
    /// Width of each bucket.
    pub bucket_size: f64,
    /// Point of Control: the price with the most volume.
    pub poc: f64,
    /// Value Area High.
    pub vah: f64,
    /// Value Area Low.
    pub val: f64,
    /// Total volume across the profile.
    pub total_volume: f64,
    /// Nodes in **ascending** price order.
    pub histogram: Vec<VolumeNode>,
    /// High Volume Nodes: local maxima in the histogram.
    pub hvn: Vec<f64>,
    /// Low Volume Nodes: local minima in the histogram.
    pub lvn: Vec<f64>,
}

/// Session profile estimate learned from completed candle sessions.
///
/// The estimate is intentionally deterministic: it uses normalized prior
/// session histograms and a shrinkage estimate of session drift. It is an
/// approximation from OHLCV candles, not exchange-provided volume-at-price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBayesianProfile {
    /// Current UTC session's opening timestamp.
    pub session_start: i64,
    /// Current session opening price.
    pub session_open: f64,
    /// Forecast center after shrinkage of prior-session drift.
    pub forecast_center: f64,
    /// Confidence proxy based on the number and consistency of prior sessions.
    pub confidence: f64,
    /// Forecast volume share at or above the session open.
    pub bullish_share: f64,
    /// Number of completed sessions informing the estimate.
    pub training_sessions: usize,
    /// Forecast bins in ascending price order.
    pub forecast: Vec<VolumeNode>,
    /// Candle-derived observed profile for the current session.
    pub observed: VolumeProfile,
}

/// Learn a session profile from prior sessions and forecast the latest session.
///
/// Completed sessions are mapped onto a common price axis centered on each
/// session's open and scaled by its high-low range. The latest session is
/// excluded from training. Historical normalized shapes are averaged and
/// smoothed; the projected center uses a sample-size shrinkage of prior
/// midpoint drift. This is a practical candle-data analogue of a Bayesian
/// particle profile, without claiming access to intrabar trade distributions.
#[must_use]
pub fn calculate_session_bayesian_profile(
    candles: &[Candle],
    bucket_size: f64,
    session_nanos: i64,
    max_training_sessions: usize,
) -> Option<SessionBayesianProfile> {
    use std::collections::BTreeMap;
    if candles.len() < 2 || !bucket_size.is_finite() || bucket_size <= 0.0 || session_nanos <= 0 {
        return None;
    }
    let bucket_start = |ts: i64| {
        if session_nanos == 7 * 24 * 60 * 60 * 1_000_000_000i64 {
            let monday_offset = 4 * 24 * 60 * 60 * 1_000_000_000i64;
            (ts - monday_offset).div_euclid(session_nanos) * session_nanos + monday_offset
        } else {
            ts.div_euclid(session_nanos) * session_nanos
        }
    };
    let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
    for candle in candles {
        if candle.open_time >= 0
            && candle.low.is_finite()
            && candle.high.is_finite()
            && candle.open.is_finite()
            && candle.close.is_finite()
            && candle.high >= candle.low
            && candle.volume.is_finite()
            && candle.volume >= 0.0
        {
            grouped
                .entry(bucket_start(candle.open_time))
                .or_default()
                .push(candle);
        }
    }
    if grouped.len() < 2 {
        return None;
    }
    let current_start = *grouped.keys().next_back()?;
    let current = grouped.get(&current_start)?;
    let open = current.first()?.open;
    let mut completed: Vec<_> = grouped
        .iter()
        .filter(|(start, _)| **start < current_start)
        .collect();
    if max_training_sessions > 0 && completed.len() > max_training_sessions {
        completed.drain(..completed.len() - max_training_sessions);
    }
    if completed.is_empty() {
        return None;
    }

    const SHAPE_BINS: usize = 41;
    let mut shape = [0.0f64; SHAPE_BINS];
    let mut offsets = Vec::with_capacity(completed.len());
    let mut ranges = Vec::with_capacity(completed.len());
    for (_, session) in &completed {
        let first = session.first()?;
        let (lo, hi) = session
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, c| {
                (a.0.min(c.low), a.1.max(c.high))
            });
        let range = (hi - lo).max(bucket_size);
        let mid = (hi + lo) * 0.5;
        offsets.push((mid - first.open) / range);
        ranges.push(range);
        for candle in session.iter() {
            let span = (candle.high - candle.low).max(bucket_size);
            // Some quote-only feeds publish zero candle volume. In that case
            // each candle is one activity observation, not zero information.
            let activity = if candle.volume > 0.0 {
                candle.volume
            } else {
                1.0
            };
            let weight = activity / SHAPE_BINS as f64;
            for (i, slot) in shape.iter_mut().enumerate() {
                let z = (i as f64 - (SHAPE_BINS as f64 - 1.0) / 2.0) / 10.0;
                let price = first.open + z * range;
                if price >= candle.low && price <= candle.high {
                    *slot += weight * (bucket_size / span).min(1.0);
                }
            }
        }
    }
    let shape_total: f64 = shape.iter().sum();
    if shape_total <= 0.0 {
        return None;
    }
    let mean_offset = offsets.iter().sum::<f64>() / offsets.len() as f64;
    let mean_range = ranges.iter().sum::<f64>() / ranges.len() as f64;
    let drift_trust = (offsets.len() as f64 / (offsets.len() as f64 + 3.0)).clamp(0.0, 0.85);
    let forecast_center = open + mean_offset * mean_range * drift_trust;
    let confidence = (drift_trust
        * (1.0
            - (offsets.iter().map(|v| (v - mean_offset).abs()).sum::<f64>()
                / offsets.len() as f64)
                .min(1.0)))
    .clamp(0.0, 1.0);
    let mut forecast = Vec::with_capacity(SHAPE_BINS);
    let mut bullish_volume = 0.0;
    for (i, mass) in shape.iter().enumerate() {
        // Smooth neighboring samples into a compact kernel estimate.
        let smoothed = [-2isize, -1, 0, 1, 2]
            .iter()
            .map(|offset| {
                let index = i as isize + offset;
                if index >= 0 && index < SHAPE_BINS as isize {
                    shape[index as usize]
                        * if *offset == 0 {
                            3.0
                        } else if offset.abs() == 1 {
                            2.0
                        } else {
                            1.0
                        }
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / 9.0;
        let z = (i as f64 - (SHAPE_BINS as f64 - 1.0) / 2.0) / 10.0;
        let price = forecast_center + z * mean_range;
        let volume = if smoothed.is_finite() {
            smoothed
        } else {
            *mass
        };
        if price >= open {
            bullish_volume += volume;
        }
        forecast.push(VolumeNode {
            price_level: price,
            volume,
            buy_volume: 0.0,
            sell_volume: 0.0,
        });
    }
    let total = forecast.iter().map(|node| node.volume).sum::<f64>();
    if total <= 0.0 {
        return None;
    }
    let current_candles: Vec<Candle> = current.iter().map(|candle| (**candle).clone()).collect();
    let observed = calculate_volume_profile_from_candles(&current_candles, bucket_size);
    Some(SessionBayesianProfile {
        session_start: current_start,
        session_open: open,
        forecast_center,
        confidence,
        bullish_share: (bullish_volume / total).clamp(0.0, 1.0),
        training_sessions: completed.len(),
        forecast,
        observed,
    })
}

impl VolumeProfile {
    /// An empty profile, returned when the input cannot produce one.
    #[must_use]
    pub fn empty(bucket_size: f64) -> Self {
        Self {
            bucket_size,
            poc: 0.0,
            vah: 0.0,
            val: 0.0,
            total_volume: 0.0,
            histogram: Vec::new(),
            hvn: Vec::new(),
            lvn: Vec::new(),
        }
    }

    /// Whether this profile holds no data.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.histogram.is_empty()
    }

    /// Whether `price` sits inside the value area.
    #[must_use]
    pub fn in_value_area(&self, price: f64) -> bool {
        !self.is_empty() && price >= self.val && price <= self.vah
    }

    /// Signed distance from the POC as a fraction of the POC.
    #[must_use]
    pub fn poc_deviation(&self, price: f64) -> Option<f64> {
        if self.is_empty() || self.poc.abs() < f64::EPSILON {
            return None;
        }
        Some((price - self.poc) / self.poc)
    }
}

/// A round bucket size that yields about `target_rows` rows over `span`.
///
/// Rounded to a power of ten times 1, 2 or 5, so a price axis reads 77,300
/// rather than 77,341.6667 -- and so a caller can put a label on a level without
/// inventing a format for an arbitrary number.
///
/// This is a *display* concern, and it lives here anyway because there is only
/// one correct answer: the chart engine and the footprint route both need it,
/// and two copies of a rounding rule drift.
///
/// `span` is a price range. A non-finite or non-positive one yields 1.0, so a
/// degenerate window cannot produce a zero bucket and divide by it.
#[must_use]
pub fn round_bucket(span: f64, target_rows: usize) -> f64 {
    if !span.is_finite() || span <= 0.0 || target_rows == 0 {
        return 1.0;
    }
    let raw = span / target_rows as f64;
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        let candidate = magnitude * step;
        if candidate >= raw {
            return candidate;
        }
    }
    magnitude * 10.0
}

/// Volume-by-price from trades, with the default value-area percentage.
pub fn calculate_volume_profile(trades: &[Trade], bucket_size: f64) -> VolumeProfile {
    calculate_volume_profile_with(trades, bucket_size, DEFAULT_VALUE_AREA_PCT, 2)
}

/// Volume profile with explicit tuning.
///
/// Returns an empty profile when `bucket_size` is not positive, when there are
/// no trades, or when the range would need more than [`MAX_BUCKETS`] buckets.
#[must_use]
pub fn calculate_volume_profile_with(
    trades: &[Trade],
    bucket_size: f64,
    value_area_pct: f64,
    node_lookback: usize,
) -> VolumeProfile {
    if trades.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return VolumeProfile::empty(bucket_size);
    }

    let (min_price, max_price) = trades
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, t| {
            (acc.0.min(t.price), acc.1.max(t.price))
        });

    if !min_price.is_finite() || !max_price.is_finite() {
        return VolumeProfile::empty(bucket_size);
    }

    let span = max_price - min_price;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bucket_count = (span / bucket_size).floor() as usize + 1;
    if bucket_count == 0 || bucket_count > MAX_BUCKETS {
        return VolumeProfile::empty(bucket_size);
    }

    let mut nodes: Vec<VolumeNode> = (0..bucket_count)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let offset = (i as f64 + 0.5) * bucket_size;
            VolumeNode {
                price_level: min_price + offset,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            }
        })
        .collect();

    for trade in trades {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = ((trade.price - min_price) / bucket_size).floor() as usize;
        let Some(node) = nodes.get_mut(index.min(bucket_count - 1)) else {
            continue;
        };

        node.volume += trade.quantity;
        if trade.is_buyer_maker {
            node.sell_volume += trade.quantity;
        } else {
            node.buy_volume += trade.quantity;
        }
    }

    finalize(nodes, bucket_size, value_area_pct, node_lookback)
}

/// Volume profile derived from candles, spreading each candle's volume
/// uniformly across its `[low, high]` range.
///
/// This is an **approximation**. Without tick data you cannot know where inside
/// the candle the volume actually traded; uniform distribution is the standard
/// assumption and it is good enough for a coarse profile. Prefer
/// [`calculate_volume_profile`] with real trades when you have them.
#[must_use]
pub fn calculate_volume_profile_from_candles(
    candles: &[Candle],
    bucket_size: f64,
) -> VolumeProfile {
    if candles.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return VolumeProfile::empty(bucket_size);
    }

    let (min_price, max_price) = candles
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, c| {
            (acc.0.min(c.low), acc.1.max(c.high))
        });

    if !min_price.is_finite() || !max_price.is_finite() {
        return VolumeProfile::empty(bucket_size);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bucket_count = ((max_price - min_price) / bucket_size).floor() as usize + 1;
    if bucket_count == 0 || bucket_count > MAX_BUCKETS {
        return VolumeProfile::empty(bucket_size);
    }

    let mut nodes: Vec<VolumeNode> = (0..bucket_count)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let offset = (i as f64 + 0.5) * bucket_size;
            VolumeNode {
                price_level: min_price + offset,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            }
        })
        .collect();

    for candle in candles {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // A malformed candle (low above high, which happens in sloppy
        // upstream data) is normalised, never panicked on: the profile
        // cannot reason about a candle that breaks OHLC, but it must not
        // underflow on one either.
        let lo = candle.low.min(candle.high);
        let hi = candle.low.max(candle.high);
        let first = ((lo - min_price) / bucket_size).floor() as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let last = ((hi - min_price) / bucket_size).floor() as usize;
        let last = last.min(bucket_count - 1);
        // A zero-range bar at the absolute top of the range floors its low
        // into bucket_count while the clamp holds its high at bucket_count-1:
        // clamp  too, or the span subtracts with overflow.
        let first = first.min(bucket_count - 1);

        let spanned = (last - first + 1).max(1);
        #[allow(clippy::cast_precision_loss)]
        let per_bucket = candle.volume / spanned as f64;
        let buy_per_bucket = candle.buy_volume / spanned as f64;
        let sell_per_bucket = candle.sell_volume / spanned as f64;

        for node in nodes.iter_mut().take(last + 1).skip(first) {
            node.volume += per_bucket;
            node.buy_volume += buy_per_bucket;
            node.sell_volume += sell_per_bucket;
        }
    }

    finalize(nodes, bucket_size, DEFAULT_VALUE_AREA_PCT, 2)
}

/// Price-range activity profile for quote-only candles such as Deriv
/// synthetics. Each candle contributes one equal unit, spread evenly over its
/// high-low range. This is a bar-range coverage estimate, never traded volume.
#[must_use]
pub fn calculate_quote_range_activity_profile(
    candles: &[Candle],
    bucket_size: f64,
) -> VolumeProfile {
    let quote_bars: Vec<Candle> = candles
        .iter()
        .cloned()
        .map(|mut candle| {
            candle.volume = 1.0;
            candle.buy_volume = 0.0;
            candle.sell_volume = 0.0;
            candle
        })
        .collect();
    calculate_volume_profile_from_candles(&quote_bars, bucket_size)
}

/// Price-by-quote-tick profile. Each finite quote observation contributes one
/// count at its observed price; these are quote updates, not traded volume.
#[must_use]
pub fn calculate_quote_tick_profile(prices: &[f64], bucket_size: f64) -> VolumeProfile {
    if prices.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return VolumeProfile::empty(bucket_size);
    }
    let finite: Vec<f64> = prices
        .iter()
        .copied()
        .filter(|price| price.is_finite())
        .collect();
    let (Some(min_price), Some(max_price)) = (
        finite.iter().copied().reduce(f64::min),
        finite.iter().copied().reduce(f64::max),
    ) else {
        return VolumeProfile::empty(bucket_size);
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bucket_count = ((max_price - min_price) / bucket_size).floor() as usize + 1;
    if bucket_count == 0 || bucket_count > MAX_BUCKETS {
        return VolumeProfile::empty(bucket_size);
    }
    let mut nodes: Vec<VolumeNode> = (0..bucket_count)
        .map(|i| VolumeNode {
            price_level: min_price + (i as f64 + 0.5) * bucket_size,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        })
        .collect();
    for price in finite {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = ((price - min_price) / bucket_size).floor() as usize;
        nodes[index.min(bucket_count - 1)].volume += 1.0;
    }
    finalize(nodes, bucket_size, DEFAULT_VALUE_AREA_PCT, 2)
}

fn finalize(
    nodes: Vec<VolumeNode>,
    bucket_size: f64,
    value_area_pct: f64,
    node_lookback: usize,
) -> VolumeProfile {
    let total_volume: f64 = nodes.iter().map(|n| n.volume).sum();
    if total_volume <= 0.0 {
        return VolumeProfile::empty(bucket_size);
    }

    // POC: highest volume; ties resolve to the lower price for determinism.
    let mut poc_index = 0;
    for (i, node) in nodes.iter().enumerate() {
        if node.volume > nodes[poc_index].volume {
            poc_index = i;
        }
    }

    let (val_index, vah_index) =
        value_area_indices(&nodes, poc_index, total_volume, value_area_pct);

    let (hvn, lvn) = detect_nodes(&nodes, node_lookback);

    VolumeProfile {
        bucket_size,
        poc: nodes[poc_index].price_level,
        vah: nodes[vah_index].price_level,
        val: nodes[val_index].price_level,
        total_volume,
        histogram: nodes,
        hvn,
        lvn,
    }
}

/// Grow the value area outward from the POC, returning `(val_index, vah_index)`.
fn value_area_indices(
    nodes: &[VolumeNode],
    poc_index: usize,
    total_volume: f64,
    value_area_pct: f64,
) -> (usize, usize) {
    let target = total_volume * value_area_pct.clamp(0.0, 1.0);
    let mut included = nodes[poc_index].volume;
    let mut upper = poc_index + 1;
    let mut lower = poc_index;
    let mut top = poc_index;

    while included < target && (upper < nodes.len() || lower > 0) {
        let upper_volume = nodes.get(upper).map_or(-1.0, |n| n.volume);
        let lower_volume = if lower > 0 {
            nodes[lower - 1].volume
        } else {
            -1.0
        };

        if upper_volume < 0.0 && lower_volume < 0.0 {
            break;
        }

        // Take the fatter neighbour; ties favour the upside, matching the
        // convention most Market Profile implementations use.
        if upper_volume >= lower_volume {
            included += upper_volume;
            top = upper;
            upper += 1;
        } else {
            included += lower_volume;
            lower -= 1;
        }
    }

    (lower, top)
}

/// Local maxima (HVN) and minima (LVN) in the histogram.
///
/// A node qualifies when it is strictly the largest (or smallest) within
/// `lookback` buckets on either side. Edge buckets are skipped because they
/// lack a full window and would otherwise always look like extremes.
fn detect_nodes(nodes: &[VolumeNode], lookback: usize) -> (Vec<f64>, Vec<f64>) {
    let mut hvn = Vec::new();
    let mut lvn = Vec::new();

    if lookback == 0 || nodes.len() < lookback * 2 + 1 {
        return (hvn, lvn);
    }

    for i in lookback..nodes.len() - lookback {
        let window = &nodes[i - lookback..=i + lookback];
        let volume = nodes[i].volume;

        // Only consider buckets that actually traded.
        if volume <= 0.0 {
            continue;
        }

        let is_max = window.iter().all(|n| volume >= n.volume);
        let is_min = window.iter().all(|n| volume <= n.volume);

        if is_max && !is_min {
            hvn.push(nodes[i].price_level);
        } else if is_min && !is_max {
            lvn.push(nodes[i].price_level);
        }
    }

    (hvn, lvn)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_flat_padded_series_does_not_overflow() {
        use crate::types::Timeframe;
        let mut ks: Vec<Candle> = (0..3)
            .map(|_| Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: 0,
                open: 99.8,
                high: 100.2,
                low: 99.4,
                close: 99.9,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let path = [
            (100.0, 100.5, 99.3, 100.2),
            (100.2, 102.0, 100.0, 101.6),
            (101.6, 101.8, 100.8, 101.0),
            (101.0, 101.2, 99.0, 99.4),
            (99.4, 100.6, 101.0, 100.4),
            (100.4, 101.8, 101.2, 101.5),
            (101.5, 102.0, 101.3, 101.7),
            (101.7, 102.2, 101.4, 101.9),
            (101.9, 104.0, 101.6, 103.6),
            (103.6, 103.8, 102.6, 103.0),
            (103.0, 103.2, 101.0, 101.4),
            (101.4, 101.6, 101.2, 101.5),
        ];
        for (o, h, l, c) in path {
            ks.push(Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: 0,
                open: o,
                high: h,
                low: l,
                close: c,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        for _ in 0..10 {
            ks.push(Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: 0,
                open: 101.4,
                high: 101.6,
                low: 101.2,
                close: 101.5,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        // This is the exact series shape that underflowed: it must compute.
        let span = 104.0 - 99.0;
        let bucket = round_bucket(span, 48);
        let profile = calculate_volume_profile_from_candles(&ks, bucket);
        assert!(!profile.is_empty());
    }

    use super::*;
    use crate::types::Timeframe;

    fn trade(price: f64, quantity: f64, buyer_maker: bool) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 0,
            price,
            quantity,
            is_buyer_maker: buyer_maker,
            timestamp: 0,
        }
    }

    fn candle(high: f64, low: f64, close: f64, volume: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: close,
            high,
            low,
            close,
            volume,
            buy_volume: volume / 2.0,
            sell_volume: volume / 2.0,
        }
    }

    #[test]
    fn session_bayesian_profile_learns_prior_and_reports_live_session() {
        let day = Timeframe::D1.nanos();
        let mut prior = candle(105.0, 95.0, 103.0, 100.0);
        prior.open = 100.0;
        prior.open_time = day;
        let mut live = candle(108.0, 99.0, 105.0, 60.0);
        live.open = 102.0;
        live.open_time = day * 2;
        let result = calculate_session_bayesian_profile(&[prior, live], 0.5, day, 8).unwrap();
        assert_eq!(result.training_sessions, 1);
        assert_eq!(result.session_start, day * 2);
        assert_eq!(result.session_open, 102.0);
        assert!(!result.forecast.is_empty());
        assert!(!result.observed.is_empty());
        assert!((0.0..=1.0).contains(&result.confidence));
        assert!((0.0..=1.0).contains(&result.bullish_share));
    }

    #[test]
    fn session_bayesian_profile_waits_for_a_completed_session() {
        let mut first = candle(105.0, 95.0, 100.0, 10.0);
        first.open_time = Timeframe::D1.nanos();
        assert!(
            calculate_session_bayesian_profile(&[first], 1.0, Timeframe::D1.nanos(), 8).is_none()
        );
    }

    #[test]
    fn poc_is_the_heaviest_price() {
        let trades = vec![
            trade(100.0, 1.0, false),
            trade(101.0, 10.0, false),
            trade(102.0, 1.0, false),
        ];
        let profile = calculate_volume_profile(&trades, 1.0);
        // Bucket midpoints are 100.5, 101.5, 102.5.
        assert!((profile.poc - 101.5).abs() < 1e-9);
    }

    #[test]
    fn histogram_is_ascending_and_contiguous() {
        let trades = vec![trade(100.0, 1.0, false), trade(103.0, 1.0, false)];
        let profile = calculate_volume_profile(&trades, 1.0);
        assert_eq!(profile.histogram.len(), 4, "empty buckets must be kept");
        for pair in profile.histogram.windows(2) {
            assert!(pair[1].price_level > pair[0].price_level);
            assert!((pair[1].price_level - pair[0].price_level - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn histogram_volume_sums_to_total() {
        let trades = vec![
            trade(100.0, 2.0, false),
            trade(100.5, 3.0, true),
            trade(101.0, 5.0, false),
        ];
        let profile = calculate_volume_profile(&trades, 0.5);
        let sum: f64 = profile.histogram.iter().map(|n| n.volume).sum();
        assert!((sum - profile.total_volume).abs() < 1e-9);
        assert!((profile.total_volume - 10.0).abs() < 1e-9);
    }

    #[test]
    fn buy_and_sell_volume_split_by_aggressor() {
        let trades = vec![trade(100.0, 4.0, false), trade(100.0, 6.0, true)];
        let profile = calculate_volume_profile(&trades, 1.0);
        let node = &profile.histogram[0];
        assert!((node.buy_volume - 4.0).abs() < 1e-9);
        assert!((node.sell_volume - 6.0).abs() < 1e-9);
    }

    #[test]
    fn value_area_brackets_the_poc() {
        let mut trades = Vec::new();
        for _ in 0..10 {
            trades.push(trade(100.0, 1.0, false));
        }
        for _ in 0..5 {
            trades.push(trade(101.0, 1.0, false));
        }
        for _ in 0..3 {
            trades.push(trade(102.0, 1.0, false));
        }
        let profile = calculate_volume_profile(&trades, 1.0);
        assert!(profile.val <= profile.poc);
        assert!(profile.vah >= profile.poc);
    }

    #[test]
    fn value_area_covers_at_least_the_target_share() {
        let mut trades = Vec::new();
        for _ in 0..50 {
            trades.push(trade(100.0, 1.0, false));
        }
        for _ in 0..30 {
            trades.push(trade(101.0, 1.0, false));
        }
        for _ in 0..20 {
            trades.push(trade(102.0, 1.0, false));
        }
        let profile = calculate_volume_profile(&trades, 1.0);

        let in_va: f64 = profile
            .histogram
            .iter()
            .filter(|n| n.price_level >= profile.val && n.price_level <= profile.vah)
            .map(|n| n.volume)
            .sum();
        assert!(in_va / profile.total_volume >= DEFAULT_VALUE_AREA_PCT - 1e-9);
    }

    #[test]
    fn invalid_bucket_size_yields_an_empty_profile_not_a_panic() {
        let trades = vec![trade(100.0, 1.0, false)];
        assert!(calculate_volume_profile(&trades, 0.0).is_empty());
        assert!(calculate_volume_profile(&trades, -1.0).is_empty());
        assert!(calculate_volume_profile(&trades, f64::NAN).is_empty());
    }

    #[test]
    fn no_trades_yields_an_empty_profile() {
        assert!(calculate_volume_profile(&[], 1.0).is_empty());
    }

    #[test]
    fn absurd_bucket_count_is_refused() {
        // 1e-6 buckets across a 100k range -> ~1e11 buckets.
        let trades = vec![trade(1.0, 1.0, false), trade(100_000.0, 1.0, false)];
        assert!(calculate_volume_profile(&trades, 0.000_001).is_empty());
    }

    #[test]
    fn in_value_area_and_deviation() {
        let mut trades = Vec::new();
        for _ in 0..10 {
            trades.push(trade(100.0, 1.0, false));
        }
        let profile = calculate_volume_profile(&trades, 1.0);
        assert!(profile.in_value_area(profile.poc));
        assert!(profile.poc_deviation(profile.poc).unwrap().abs() < 1e-9);
    }

    #[test]
    fn single_trade_profile_is_degenerate_but_valid() {
        let profile = calculate_volume_profile(&[trade(100.0, 5.0, false)], 1.0);
        assert_eq!(profile.histogram.len(), 1);
        assert!((profile.poc - profile.vah).abs() < 1e-9);
        assert!((profile.poc - profile.val).abs() < 1e-9);
    }

    #[test]
    fn candle_profile_spreads_volume_across_the_range() {
        // One candle spanning 100..102 with volume 9 -> 3 per bucket.
        let profile =
            calculate_volume_profile_from_candles(&[candle(102.0, 100.0, 101.0, 9.0)], 1.0);
        assert_eq!(profile.histogram.len(), 3);
        for node in &profile.histogram {
            assert!((node.volume - 3.0).abs() < 1e-9, "got {}", node.volume);
        }
        assert!((profile.total_volume - 9.0).abs() < 1e-9);
    }

    #[test]
    fn quote_range_activity_profile_counts_bars_without_claiming_volume() {
        let bars = [
            candle(102.0, 100.0, 101.0, 80.0),
            candle(104.0, 101.0, 103.0, 250.0),
        ];
        let profile = calculate_quote_range_activity_profile(&bars, 1.0);
        assert!(!profile.is_empty());
        assert!((profile.total_volume - 2.0).abs() < 1e-9);
    }

    #[test]
    fn quote_tick_profile_counts_observed_prices_without_claiming_volume() {
        let profile = calculate_quote_tick_profile(&[100.0, 100.1, 100.1, 101.0, f64::NAN], 0.5);
        assert!((profile.total_volume - 4.0).abs() < 1e-9);
        let busiest = profile
            .histogram
            .iter()
            .max_by(|a, b| a.volume.total_cmp(&b.volume))
            .expect("price buckets");
        assert!((busiest.price_level - 100.25).abs() < 1e-9);
    }

    #[test]
    fn hvn_detected_at_the_heavy_price_and_lvn_at_the_light_one() {
        // Heavy in the middle, thin at the edges.
        let mut trades = Vec::new();
        for price in [98.0, 99.0, 101.0, 102.0] {
            trades.push(trade(price, 1.0, false));
        }
        for _ in 0..20 {
            trades.push(trade(100.0, 1.0, false));
        }
        let profile = calculate_volume_profile_with(&trades, 1.0, 0.7, 1);
        assert!(!profile.hvn.is_empty(), "expected a high volume node");
        assert!(profile.hvn.iter().any(|p| (*p - 100.5).abs() < 1e-9));
    }
}
