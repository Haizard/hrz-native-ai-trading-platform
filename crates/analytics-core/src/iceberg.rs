//! Iceberg order detection -- the "Resistance" method.
//!
//! An iceberg order displays only a slice of its real size, refilling as it
//! fills. Exchanges do not report icebergs, so every detector is an
//! estimation; the modern method (Bookmap documents it as **Resistance**,
//! noting its accuracy is close to native MBO detection when the event stream
//! is well ordered) watches for a price level whose displayed size keeps
//! *reappearing* while volume executed against it far exceeds anything that
//! was ever visible there.
//!
//! ## The rule
//!
//! For each price level touched in the window, track the maximum size ever
//! visible on the side being tested. If executed volume against that side at
//! that price exceeds `max_visible * ratio_threshold`, the level behaved like
//! an iceberg: either a genuine iceberg order or an HFT-mimicking sequence of
//! re-placed limit orders. The two are behaviorally equivalent -- the
//! resistance they offer is the same, which is exactly what a trader reads.
//!
//! ## Honesty
//!
//! Every event carries its ratio and is explicitly **probabilistic**: a
//! candidate, not a fact. Detection needs depth history; with no snapshots
//! the answer is empty -- "no evidence," never a fabricated signal.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::{OrderBookSnapshot, Side, Trade};

/// Default multiple of max visible size executed volume must reach.
///
/// The same 3x convention the imbalance detector uses, so the two tools read
/// with one intuition: "3x is where we claim dominance."
pub const DEFAULT_RATIO_THRESHOLD: f64 = 3.0;

/// Tuning for [`detect_icebergs`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IcebergConfig {
    /// Executed volume must exceed `max_visible * ratio_threshold` to count.
    pub ratio_threshold: f64,
    /// A level must have absorbed at least this much executed volume to be
    /// worth reporting, in base quantity. Keeps dust levels out of the output.
    pub min_executed: f64,
}

impl Default for IcebergConfig {
    fn default() -> Self {
        Self {
            ratio_threshold: DEFAULT_RATIO_THRESHOLD,
            min_executed: 1.0,
        }
    }
}

/// One detected iceberg candidate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IcebergEvent {
    /// The price level that showed iceberg behavior.
    pub price: f64,
    /// The side whose resting orders absorbed the flow.
    ///
    /// `Side::Buy` = bids refilled at this price (a hidden buyer).
    /// `Side::Sell` = asks refilled (a hidden seller).
    pub side: Side,
    /// Total executed volume against that side at this price.
    pub executed: f64,
    /// Largest size ever displayed on that side at this price in the window.
    pub max_visible: f64,
    /// `executed / max_visible`. The confidence proxy: higher means the
    /// refill story is harder to explain any other way.
    pub ratio: f64,
}

impl IcebergEvent {
    /// Whether this is a hidden buyer (bids absorbed).
    #[must_use]
    pub const fn is_bid_iceberg(&self) -> bool {
        matches!(self.side, Side::Buy)
    }
}

/// Price levels are bucketed to this many decimal places before comparison, so
/// two prints at 99.9999999 and 100.0 land on one level. A footprint's bucket
/// is the same idea; here the bucket width is fixed at `1e-6` because the
/// detector runs per-symbol over short windows where one instrument's tick
/// size does not change mid-window.
const PRICE_TOLERANCE: f64 = 1_000_000.0;

/// A level is `(bucketed price, side)`, packed into one `u64` key so the
/// per-level accumulators live in a single flat map. The price is scaled by
/// [`PRICE_TOLERANCE`], truncated, and split across 40 bits (plenty for any
/// price a venue lists, up to ~1.1e12 before scaling); the side takes 1 bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LevelKey {
    price_bucket: i64,
    side: Side,
}

impl LevelKey {
    fn of(price: f64, side: Side) -> Self {
        Self {
            price_bucket: (price * PRICE_TOLERANCE).round() as i64,
            side,
        }
    }
}

/// Per-level accumulators over the detection window.
#[derive(Debug, Default)]
struct LevelTape {
    executed: f64,
    max_visible: f64,
}

/// Track one snapshot's resting sizes for every level the map already cares
/// about (or, with `all_levels`, for every level the snapshot shows).
fn record_visibility(
    tapes: &mut HashMap<LevelKey, LevelTape>,
    snapshot: &OrderBookSnapshot,
    all_levels: bool,
) {
    for (levels, side) in [(&snapshot.bids, Side::Buy), (&snapshot.asks, Side::Sell)] {
        for level in levels {
            let key = LevelKey::of(level.price, side);
            if !all_levels && !tapes.contains_key(&key) {
                continue;
            }
            let tape = tapes.entry(key).or_default();
            tape.max_visible = tape.max_visible.max(level.quantity);
        }
    }
}

/// Detect iceberg candidates from ordered depth snapshots and the trade tape.
///
/// Both inputs must be in chronological order. The interesting window is short
/// (seconds to minutes): beyond that, a level that merely *accumulated*
/// slowly reads as refilling, which is absorption language, not iceberg
/// language.
///
/// Levels are grouped by bucketed price + side; each level's executed volume
/// comes from the trades that printed at (or within a half-tick of) it, and
/// its `max_visible` comes from every snapshot at or before the last such
/// trade. Returns candidates sorted by descending ratio. Empty when either
/// input is empty -- and that emptiness means "no evidence," not "no
/// icebergs."
pub fn detect_icebergs(
    snapshots: &[OrderBookSnapshot],
    trades: &[Trade],
    config: IcebergConfig,
) -> Vec<IcebergEvent> {
    let mut tapes: HashMap<LevelKey, LevelTape> = HashMap::new();
    let mut snapshot_iter = snapshots.iter().peekable();

    for trade in trades {
        // Depth history up to this trade: everything at or before its time.
        while let Some(snapshot) = snapshot_iter.peek() {
            if snapshot.timestamp <= trade.timestamp {
                record_visibility(&mut tapes, snapshot, false);
                snapshot_iter.next();
            } else {
                break;
            }
        }

        // The aggressor trades into the resting side. A seller aggressing
        // (`is_buyer_maker == true`) hits resting bids.
        let side = if trade.is_buyer_maker {
            Side::Buy
        } else {
            Side::Sell
        };
        let key = LevelKey::of(trade.price, side);
        let tape = tapes.entry(key).or_default();
        tape.executed += trade.quantity;
        // Record the trade instant's own visibility too, in case it lands
        // between snapshots: the level's current displayed size is evidence.
        let visible = current_visible(snapshots, trade.timestamp, trade.price, side);
        tape.max_visible = tape.max_visible.max(visible.unwrap_or(0.0));
    }

    // Snapshots after the last trade still contribute visibility evidence.
    for snapshot in snapshot_iter {
        record_visibility(&mut tapes, snapshot, false);
    }

    let mut events: Vec<IcebergEvent> = tapes
        .into_iter()
        .filter_map(|(key, tape)| {
            if tape.executed < config.min_executed || tape.max_visible <= 0.0 {
                return None;
            }
            let ratio = tape.executed / tape.max_visible;
            if ratio < config.ratio_threshold {
                return None;
            }
            Some(IcebergEvent {
                price: key.price_bucket as f64 / PRICE_TOLERANCE,
                side: key.side,
                executed: tape.executed,
                max_visible: tape.max_visible,
                ratio,
            })
        })
        .collect();

    events.sort_by(|a, b| b.ratio.total_cmp(&a.ratio));
    events
}

/// The displayed size on `side` at `price` in the latest snapshot at or before
/// `ts`. Linear over the whole snapshot list per call; callers pass short
/// windows, and the tape loop above advances a cursor for the common case.
fn current_visible(
    snapshots: &[OrderBookSnapshot],
    ts: i64,
    price: f64,
    side: Side,
) -> Option<f64> {
    let snapshot = snapshots
        .iter()
        .rev()
        .find(|s| s.timestamp <= ts)?;
    let levels = match side {
        Side::Buy => &snapshot.bids,
        Side::Sell => &snapshot.asks,
    };
    levels
        .iter()
        .find(|l| (l.price - price).abs() < 1.0 / PRICE_TOLERANCE)
        .map(|l| l.quantity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OrderBookLevel;

    fn bid_snapshot(ts: i64, price: f64, qty: f64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "TEST".into(),
            timestamp: ts,
            bids: vec![OrderBookLevel { price, quantity: qty }],
            asks: vec![],
        }
    }

    fn sell_trade(ts: i64, price: f64, qty: f64) -> Trade {
        Trade {
            symbol: "TEST".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: true,
            timestamp: ts,
        }
    }

    #[test]
    fn refuses_without_evidence() {
        assert!(detect_icebergs(&[], &[], IcebergConfig::default()).is_empty());
    }

    #[test]
    fn a_refilling_bid_is_detected() {
        // The bid shows 5 at most, but 30 executes into it: 6x.
        let snapshots = vec![
            bid_snapshot(1_000, 99.0, 5.0),
            bid_snapshot(3_000, 99.0, 2.0),
        ];
        let trades = vec![
            sell_trade(2_000, 99.0, 10.0),
            sell_trade(4_000, 99.0, 20.0),
        ];

        let events = detect_icebergs(&snapshots, &trades, IcebergConfig::default());
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert!(event.is_bid_iceberg());
        assert!((event.executed - 30.0).abs() < 1e-9);
        assert!((event.max_visible - 5.0).abs() < 1e-9);
        assert!((event.ratio - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_one_shot_large_order_is_not_an_iceberg() {
        // One snapshot showing 50, one trade of 10: only 0.2x.
        let snapshots = vec![bid_snapshot(1_000, 99.0, 50.0)];
        let trades = vec![sell_trade(2_000, 99.0, 10.0)];

        let events = detect_icebergs(&snapshots, &trades, IcebergConfig::default());
        assert!(events.is_empty());
    }

    #[test]
    fn min_executed_filters_dust() {
        // 4x ratio, but only 0.4 executed: below the dust floor.
        let snapshots = vec![bid_snapshot(1_000, 99.0, 0.1)];
        let trades = vec![sell_trade(2_000, 99.0, 0.4)];

        let config = IcebergConfig::default();
        assert!(detect_icebergs(&snapshots, &trades, config).is_empty());

        let config = IcebergConfig {
            min_executed: 0.1,
            ..config
        };
        assert_eq!(detect_icebergs(&snapshots, &trades, config).len(), 1);
    }
}
