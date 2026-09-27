//! Order-size classes -- bucketing trades by notional value.
//!
//! A delta tells you *which side* aggressed. Classifying by order size tells
//! you **who**: the "Order Flow IQ" suite calls this "CVD by order size," and
//! the reading is that small and large players behave differently at turning
//! points. If small orders aggressively buy while large orders stop selling
//! (or start selling), large passive flow is absorbing retail aggression -- a
//! causal hypothesis an agent thesis can state, and one
//! `backtest_similar_setups` can base-rate historically.
//!
//! ## The notional unit
//!
//! Classes are defined in **quote currency** notional (`price * quantity`):
//! USD on USD pairs, USDT on USDT pairs -- the same convention the video uses.
//! There is deliberately no contract-size interpretation: for futures, quote
//! notional and contract notional differ by a fixed multiplier, and the
//! thresholds are configurable per symbol anyway.
//!
//! ## Sessions
//!
//! The per-class CVD tracker resets on the same **UTC calendar day** boundary
//! as [`crate::cvd`], so per-class and plain CVD are directly comparable.

use serde::{Deserialize, Serialize};

use crate::cvd::session_of;
use crate::types::Trade;
use crate::volume_profile::{calculate_volume_profile, VolumeProfile};

/// Default notional below which an order counts as `Small`, in quote currency.
pub const DEFAULT_SMALL_THRESHOLD: f64 = 10_000.0;
/// Default notional below which an order counts as `Medium`, in quote currency.
pub const DEFAULT_MEDIUM_THRESHOLD: f64 = 50_000.0;

/// Which wallet-size bucket a trade's notional falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeClass {
    /// Small orders -- typically retail-sized.
    Small,
    /// Medium orders.
    Medium,
    /// Large orders -- typically institutional-sized.
    Large,
}

impl SizeClass {
    /// Every class, smallest first.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Small, Self::Medium, Self::Large]
    }

    /// Canonical name, as it appears in responses and the Strategy DSL.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// Index into per-class arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
        }
    }
}

/// Notional thresholds defining the size classes, in quote currency.
///
/// The default boundaries (10k / 50k) mirror the video suite's defaults; they
/// are per-symbol config in practice, because "large" on BTCUSDT and on a
/// thinly traded alt pair are different worlds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizeClassConfig {
    /// Notional below this is `Small`.
    pub small_below: f64,
    /// Notional below this (and at or above `small_below`) is `Medium`.
    pub medium_below: f64,
}

impl Default for SizeClassConfig {
    fn default() -> Self {
        Self {
            small_below: DEFAULT_SMALL_THRESHOLD,
            medium_below: DEFAULT_MEDIUM_THRESHOLD,
        }
    }
}

impl SizeClassConfig {
    /// The class a notional value belongs to.
    ///
    /// A non-finite notional cannot be ranked, and lands in `Large` on the
    /// same reasoning `f64::total_cmp` uses for infinities: a NaN comparison
    /// chain would silently reorder, so an explicit guard keeps the answer
    /// deterministic.
    #[must_use]
    pub fn classify(&self, notional: f64) -> SizeClass {
        if !notional.is_finite() || notional >= self.medium_below {
            SizeClass::Large
        } else if notional >= self.small_below {
            SizeClass::Medium
        } else {
            SizeClass::Small
        }
    }
}

/// Buy/sell/delta breakdown for one size class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassDelta {
    /// Buy-aggressed volume, in base quantity.
    pub buy: f64,
    /// Sell-aggressed volume, in base quantity.
    pub sell: f64,
    /// Number of trades in this class.
    pub trades: usize,
}

impl ClassDelta {
    /// `buy - sell`.
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.buy - self.sell
    }
}

/// Per-class aggregation over a slice of trades.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SizeDeltaBreakdown {
    /// One entry per class, indexed by [`SizeClass::index`].
    pub classes: [ClassDelta; 3],
}

impl SizeDeltaBreakdown {
    /// The breakdown for one class.
    #[must_use]
    pub fn class(&self, class: SizeClass) -> &ClassDelta {
        &self.classes[class.index()]
    }

    /// Total delta across every class. Equals the plain candle delta when the
    /// input was exactly that candle's trades.
    #[must_use]
    pub fn total_delta(&self) -> f64 {
        self.classes.iter().map(ClassDelta::delta).sum()
    }

    /// Total volume across every class.
    #[must_use]
    pub fn total_volume(&self) -> f64 {
        self.classes.iter().map(|c| c.buy + c.sell).sum()
    }
}

/// Aggregate trades into per-class buy/sell volumes.
///
/// Every trade is counted exactly once, so the class volumes sum to the
/// aggregate volume of the input -- a property test pins this.
#[must_use]
pub fn delta_by_size(trades: &[Trade], config: &SizeClassConfig) -> SizeDeltaBreakdown {
    let mut breakdown = SizeDeltaBreakdown::default();
    for trade in trades {
        let notional = trade.price * trade.quantity;
        let slot = &mut breakdown.classes[config.classify(notional).index()];
        if trade.is_buyer_maker {
            slot.sell += trade.quantity;
        } else {
            slot.buy += trade.quantity;
        }
        slot.trades += 1;
    }
    breakdown
}

/// Per-candle per-class breakdown, oldest first.
///
/// Candles and trades must both be sorted ascending by time; the split is a
/// binary search per candle, matching [`crate::footprint::build_footprints`].
/// A candle whose bucket holds no trades yields the zero breakdown rather than
/// being dropped, so the output is index-aligned with the input.
#[must_use]
pub fn delta_by_size_per_candle(
    candles: &[crate::types::Candle],
    trades: &[Trade],
    config: &SizeClassConfig,
) -> Vec<SizeDeltaBreakdown> {
    candles
        .iter()
        .map(|candle| {
            let end = candle.open_time + candle.timeframe.nanos();
            let from = trades.partition_point(|t| t.timestamp < candle.open_time);
            let to = trades.partition_point(|t| t.timestamp < end);
            delta_by_size(&trades[from..to], config)
        })
        .collect()
}

/// Cumulative delta for every size class at one point in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SizeClassCvdPoint {
    /// Open time of the candle this point was fed from.
    pub open_time: i64,
    /// Cumulative delta per class, indexed by [`SizeClass::index`].
    pub classes: [f64; 3],
}

/// Per-class cumulative delta over `candles` and their trades.
///
/// With `reset_at_session` every class restarts at each UTC day boundary, the
/// same rule [`crate::cvd::calculate_cvd`] applies. The per-class sums track
/// the plain CVD exactly: summing the classes of any point reproduces the
/// cumulative delta of the same window (a property test pins this).
#[must_use]
pub fn calculate_cvd_by_size(
    candles: &[crate::types::Candle],
    trades: &[Trade],
    config: &SizeClassConfig,
    reset_at_session: bool,
) -> Vec<SizeClassCvdPoint> {
    let per_candle = delta_by_size_per_candle(candles, trades, config);
    let mut running = [0.0_f64; 3];
    let mut current_session: Option<i64> = None;
    let mut points = Vec::with_capacity(candles.len());

    for (candle, breakdown) in candles.iter().zip(&per_candle) {
        if reset_at_session {
            let session = session_of(candle.open_time);
            if current_session != Some(session) {
                running = [0.0; 3];
                current_session = Some(session);
            }
        }
        for (i, class_delta) in breakdown.classes.iter().enumerate() {
            running[i] += class_delta.delta();
        }
        points.push(SizeClassCvdPoint {
            open_time: candle.open_time,
            classes: running,
        });
    }
    points
}

/// A volume profile computed over **one size class only**.
///
/// This is the "volume filter" idea: the same profile math, restricted to
/// trades whose notional class matches. Filtering to `Large` shows where the
/// big players actually accumulated -- a view almost no charting platform
/// offers.
///
/// The filter is a predicate over trades, so classes compose: a caller that
/// wants "everything except small" passes `|c| c != SizeClass::Small`.
#[must_use]
pub fn calculate_volume_profile_for_class<F>(
    trades: &[Trade],
    bucket_size: f64,
    config: &SizeClassConfig,
    class_filter: F,
) -> VolumeProfile
where
    F: Fn(SizeClass) -> bool,
{
    let filtered: Vec<Trade> = trades
        .iter()
        .filter(|t| class_filter(config.classify(t.price * t.quantity)))
        .cloned()
        .collect();
    calculate_volume_profile(&filtered, bucket_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, Timeframe};

    fn trade(price: f64, qty: f64, buyer_maker: bool, ts: i64) -> Trade {
        Trade {
            symbol: "TEST".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: buyer_maker,
            timestamp: ts,
        }
    }

    fn candle(open_time: i64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    #[test]
    fn classification_boundaries_hold() {
        let config = SizeClassConfig::default();
        assert_eq!(config.classify(0.0), SizeClass::Small);
        assert_eq!(config.classify(9_999.0), SizeClass::Small);
        assert_eq!(config.classify(10_000.0), SizeClass::Medium);
        assert_eq!(config.classify(49_999.0), SizeClass::Medium);
        assert_eq!(config.classify(50_000.0), SizeClass::Large);
        assert!(config.classify(f64::NAN) == SizeClass::Large);
    }

    #[test]
    fn classes_sum_to_the_total() {
        let trades = vec![
            trade(100.0, 1.0, false, 1_000),   // notional 100 -> small buy
            trade(100.0, 200.0, true, 2_000),  // notional 20k -> medium sell
            trade(100.0, 600.0, false, 3_000), // notional 60k -> large buy
        ];
        let config = SizeClassConfig::default();
        let breakdown = delta_by_size(&trades, &config);

        assert!((breakdown.total_volume() - 801.0).abs() < 1e-9);
        assert!((breakdown.total_delta() - (1.0 + 600.0 - 200.0)).abs() < 1e-9);
        assert_eq!(breakdown.class(SizeClass::Small).trades, 1);
        assert_eq!(breakdown.class(SizeClass::Medium).trades, 1);
        assert_eq!(breakdown.class(SizeClass::Large).trades, 1);
    }

    #[test]
    fn per_class_cvd_tracks_plain_cvd() {
        // Candle 0: bought 1, sold 3 (delta -2). Candle 1: bought 10 (+10).
        let c0 = candle(0, 1.0, 3.0);
        let c1 = candle(60_000_000_000, 10.0, 0.0);
        let candles = vec![c0, c1];
        let trades = vec![
            trade(100.0, 1.0, false, 1_000),
            trade(100.0, 3.0, true, 2_000),
            trade(100.0, 10.0, false, 61_000_000_000),
        ];
        let config = SizeClassConfig::default();

        let per_class = calculate_cvd_by_size(&candles, &trades, &config, false);
        let plain = crate::cvd::calculate_cvd(&candles, false);

        let last = per_class.last().expect("two candles");
        let summed: f64 = last.classes.iter().sum();
        let expected = plain.last().expect("two candles");
        assert!((summed - expected).abs() < 1e-9);
    }

    #[test]
    fn session_reset_clears_every_class() {
        let c0 = candle(0, 600.0, 0.0);
        let c1 = candle(crate::cvd::NS_PER_DAY, 300.0, 0.0); // next UTC day
        let candles = vec![c0, c1];
        let trades = vec![
            // Notional 60_000 -> Large.
            trade(100.0, 600.0, false, 1_000),
            // Notional 30_000 -> Medium.
            trade(100.0, 300.0, false, crate::cvd::NS_PER_DAY + 1_000),
        ];
        let config = SizeClassConfig::default();
        let points = calculate_cvd_by_size(&candles, &trades, &config, true);

        // Day 1 accumulates +600 in the Large class. Day 2 resets, then +300 Medium.
        assert!((points[0].classes[SizeClass::Large.index()] - 600.0).abs() < 1e-9);
        assert!((points[1].classes[SizeClass::Medium.index()] - 300.0).abs() < 1e-9);
        assert!(points[1].classes[SizeClass::Large.index()].abs() < 1e-9, "session reset");
    }

    #[test]
    fn class_filtered_profile_only_counts_that_class() {
        let trades = vec![
            trade(100.0, 1.0, false, 1_000),
            trade(100.0, 600.0, false, 2_000),
        ];
        let config = SizeClassConfig::default();
        let profile = calculate_volume_profile_for_class(&trades, 1.0, &config, |c| c == SizeClass::Large);
        let total: f64 = profile.histogram.iter().map(|n| n.volume).sum();
        assert!((total - 600.0).abs() < 1e-9);
    }
}
