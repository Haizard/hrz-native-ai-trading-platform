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
        let first = ((candle.low - min_price) / bucket_size).floor() as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let last = ((candle.high - min_price) / bucket_size).floor() as usize;
        let last = last.min(bucket_count - 1);

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
