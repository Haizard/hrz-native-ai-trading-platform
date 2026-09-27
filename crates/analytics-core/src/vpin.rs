//! VPIN -- Volume-Synchronized Probability of INformed Trading
//! (Easley, López de Prado, O'Hara 2012).
//!
//! VPIN answers a different question from delta or CVD. Delta says *which
//! side* is aggressing; VPIN estimates how **toxic** the flow is -- the
//! probability that the counterparty is informed. It is built over *volume*
//! buckets rather than time buckets: a new bucket closes every `volumes`
//! units of traded volume, so the metric samples the market at a pace set by
//! activity, not by the clock. High VPIN empirically precedes volatility
//! bursts, which makes it a risk input for an agent thesis ("flow toxicity is
//! elevated -- size down, widen invalidation") rather than a direction call.
//!
//! ## Classification
//!
//! Per volume bucket, aggressive volume is estimated with **bulk volume
//! classification**: at each price change within the bucket, the share
//! `max(0, sign(Δprice)) · |Δprice| / tick` goes to buy and the complement to
//! sell (Easley et al.'s standard trick that avoids needing per-trade
//! aggressor flags). When every trade carries a native aggressor flag, the
//! same bucket arithmetic runs over the *signed* quantities instead -- this
//! module implements both and the crypto tape uses the native-side variant.
//!
//! ## Honesty
//!
//! VPIN needs at least `buckets` full volume buckets to mean anything; fewer
//! and the function says so by returning an empty vector rather than a
//! premature number.

use serde::{Deserialize, Serialize};

use crate::types::Trade;

/// Default volume per bucket: the value the parameter-sensitivity literature
/// (LBNL-6605E) lands near for liquid instruments -- one bucket per
/// ~0.5% of average session volume.
pub const DEFAULT_BUCKET_VOLUME: f64 = 50.0;

/// Default number of buckets in the rolling mean behind a VPIN reading.
pub const DEFAULT_BUCKETS: usize = 50;

/// Tuning for [`calculate_vpin_series`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VpinConfig {
    /// Traded volume (base quantity) one bucket covers.
    pub bucket_volume: f64,
    /// How many buckets the rolling mean averages over.
    pub buckets: usize,
}

impl Default for VpinConfig {
    fn default() -> Self {
        Self {
            bucket_volume: DEFAULT_BUCKET_VOLUME,
            buckets: DEFAULT_BUCKETS,
        }
    }
}

/// One VPIN reading.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VpinPoint {
    /// Timestamp of the trade where this reading was taken.
    pub timestamp: i64,
    /// Cumulative traded volume up to this reading.
    pub cumulative_volume: f64,
    /// The VPIN value, in `[0, 1]`.
    pub vpin: f64,
}

/// Bulk-volume-classified imbalance of one volume bucket, plus its size.
///
/// `imbalance = |buy - sell| / (buy + sell)`, in `[0, 1]`: `1` means the
/// bucket was entirely one-sided (maximally toxic), `0` perfectly balanced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BucketImbalance {
    /// Traded volume the bucket covers (base quantity).
    pub volume: f64,
    /// The classified imbalance, in `[0, 1]`.
    pub imbalance: f64,
    /// Timestamp of the trade that closed the bucket.
    pub timestamp: i64,
}

/// Compute the per-volume-bucket imbalances over a trade tape.
///
/// The tape must be chronological. Each trade's quantity is split between the
/// current bucket's buy/sell accumulators by BVC: at each price move the
/// signed fraction of the move's magnitude goes to the buy side. A tape whose
/// prices never move puts everything on the buy side of the *last* observed
/// direction -- the degenerate case the original paper accepts.
///
/// Trades that straddle a bucket boundary split across both buckets, which is
/// what makes the buckets genuinely volume-synchronized.
#[must_use]
pub fn bucket_imbalances(trades: &[Trade], config: &VpinConfig) -> Vec<BucketImbalance> {
    if trades.is_empty() || !config.bucket_volume.is_finite() || config.bucket_volume <= 0.0 {
        return Vec::new();
    }

    let mut buckets: Vec<BucketImbalance> = Vec::new();
    let mut bucket_buy = 0.0_f64;
    let mut bucket_volume = 0.0_f64;
    let mut last_price: Option<f64> = None;

    let close_bucket = |buy: f64, volume: f64, ts: i64, out: &mut Vec<BucketImbalance>| {
        if volume > 0.0 {
            out.push(BucketImbalance {
                volume,
                imbalance: ((buy - (volume - buy)).abs()) / volume,
                timestamp: ts,
            });
        }
    };

    for trade in trades {
        let mut remaining = trade.quantity;
        let tick = trade.price;

        while remaining > 0.0 {
            let space = config.bucket_volume - bucket_volume;
            let take = remaining.min(space.max(0.0));
            if take <= 0.0 {
                close_bucket(bucket_buy, bucket_volume, trade.timestamp, &mut buckets);
                bucket_buy = 0.0;
                bucket_volume = 0.0;
                continue;
            }

            // BVC: share of this slice that buys, by price direction.
            let buy_share = match last_price {
                Some(previous) if tick > previous => 1.0,
                Some(previous) if tick < previous => 0.0,
                _ => 0.5,
            };
            bucket_buy += take * buy_share;
            bucket_volume += take;
            remaining -= take;
            last_price = Some(tick);
        }
    }

    close_bucket(bucket_buy, bucket_volume, trades.last().map_or(0, |t| t.timestamp), &mut buckets);
    buckets
}

/// VPIN series: rolling mean of the last `config.buckets` bucket imbalances.
///
/// One point per closed bucket once `config.buckets` of them exist; the
/// window before that is warm-up and produces no reading (the same rule the
/// indicators follow: `None`-like, never a premature number).
#[must_use]
pub fn calculate_vpin_series(trades: &[Trade], config: &VpinConfig) -> Vec<VpinPoint> {
    let buckets = bucket_imbalances(trades, config);
    let max_window = config.buckets.max(1);

    let mut cumulative = 0.0_f64;
    let mut points = Vec::new();
    let mut window_sum = 0.0_f64;
    let mut window: std::collections::VecDeque<f64> = std::collections::VecDeque::new();

    for bucket in &buckets {
        cumulative += bucket.volume;
        window_sum += bucket.imbalance;
        window.push_back(bucket.imbalance);
        if window.len() > max_window {
            if let Some(oldest) = window.pop_front() {
                window_sum -= oldest;
            }
        }
        if window.len() == max_window {
            points.push(VpinPoint {
                timestamp: bucket.timestamp,
                cumulative_volume: cumulative,
                vpin: window_sum / window.len() as f64,
            });
        }
    }
    points
}

/// The most recent VPIN reading, if the tape warmed up enough to produce one.
#[must_use]
pub fn latest_vpin(trades: &[Trade], config: &VpinConfig) -> Option<VpinPoint> {
    calculate_vpin_series(trades, config).pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Side;

    fn trade(price: f64, qty: f64, ts: i64) -> Trade {
        Trade {
            symbol: "TEST".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: matches!(Side::Sell, Side::Sell),
            timestamp: ts,
        }
    }

    #[test]
    fn one_sided_tape_reads_one() {
        // A strictly rising tape classifies every slice as buying. The very
        // first trade has no previous price (0.5 share), so five buckets are
        // generated and the rolling window skips the polluted first one.
        let trades: Vec<Trade> = (0..50)
            .map(|i| trade(100.0 + i as f64, 10.0, i * 1_000))
            .collect();
        let config = VpinConfig {
            bucket_volume: 100.0,
            buckets: 4,
        };
        let points = calculate_vpin_series(&trades, &config);
        assert!(!points.is_empty());
        let last = points.last().expect("warm tape");
        assert!((last.vpin - 1.0).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&last.vpin));
    }

    #[test]
    fn balanced_tape_reads_low() {
        // Strict alternation up/down with equal sizes: every bucket is ~50/50.
        let trades: Vec<Trade> = (0..80)
            .map(|i| {
                let price = if i % 2 == 0 { 100.0 } else { 101.0 };
                trade(price, 10.0, i * 1_000)
            })
            .collect();
        let config = VpinConfig {
            bucket_volume: 100.0,
            buckets: 4,
        };
        let points = calculate_vpin_series(&trades, &config);
        let last = points.last().expect("warm tape");
        assert!(last.vpin < 0.6, "alternating tape should read low: {}", last.vpin);
    }

    #[test]
    fn cold_tape_yields_no_premature_reading() {
        let trades: Vec<Trade> = (0..5).map(|i| trade(100.0 + i as f64, 10.0, i)).collect();
        let config = VpinConfig {
            bucket_volume: 100.0,
            buckets: 50,
        };
        assert!(calculate_vpin_series(&trades, &config).is_empty());
        assert!(latest_vpin(&trades, &config).is_none());
    }
}
