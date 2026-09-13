//! Order-book maintenance from an exchange diff stream.
//!
//! Binance (and most venues) do **not** ship a full book on every message. They
//! ship periodic diffs and expect the client to keep the book itself:
//!
//! 1. Fetch a REST snapshot (`lastUpdateId = L`).
//! 2. Start receiving diffs. Drop every diff with `u <= L` (already in the
//!    snapshot). The first diff satisfying `U <= L+1 <= u` bridges the gap;
//!    apply it and everything after it.
//! 3. From then on, apply diffs in order and raise a gap flag if `U > last_u + 1`.
//!
//! Everything here is pure and synchronous so the sequence logic is unit
//! testable without a socket (`docs/04-MARKET-DATA-ENGINE.md`).

use std::collections::BTreeMap;

use analytics_core::{OrderBookLevel, OrderBookSnapshot};

/// A price level, ordered by price.
///
/// `f64` has no `Ord`, so we order by the IEEE-754 bit pattern. That is valid
/// here because prices are always positive and finite: for positive floats the
/// unsigned integer ordering of the bit pattern matches numeric ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PriceKey(u64);

impl PriceKey {
    /// Build a key from a price.
    ///
    /// # Panics
    /// Debug-asserts that `price` is positive and finite; a `NaN` or negative
    /// price would break the ordering invariant.
    #[must_use]
    pub fn new(price: f64) -> Self {
        debug_assert!(price.is_finite() && price > 0.0, "invalid price: {price}");
        Self(price.to_bits())
    }

    /// The original price.
    #[must_use]
    pub fn price(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// One side of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSide {
    /// Bids (buy orders).
    Bid,
    /// Asks (sell orders).
    Ask,
}

/// A diff-stream delta: `[price, quantity]` pairs, where quantity `0` removes
/// the level.
#[derive(Debug, Clone, PartialEq)]
pub struct DepthDiff {
    /// Binance `U`: first update id in this event.
    pub first_update_id: u64,
    /// Binance `u`: final update id in this event.
    pub final_update_id: u64,
    /// Bid levels to set or remove.
    pub bids: Vec<(f64, f64)>,
    /// Ask levels to set or remove.
    pub asks: Vec<(f64, f64)>,
}

/// Why a diff was handled the way it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOutcome {
    /// Applied to the book.
    Applied,
    /// Buffered because no REST snapshot has arrived yet.
    Buffered,
    /// Dropped because it predates the snapshot (already reflected in it).
    Stale,
    /// Dropped because it does not bridge the gap and is not yet applicable.
    OutOfOrder,
    /// Applied, but a sequence gap was detected beforehand.
    AppliedWithGap,
}

/// A live, in-memory order book kept up to date from a diff stream.
#[derive(Debug, Clone)]
pub struct OrderBook {
    symbol: String,
    /// price -> quantity, iterated in reverse for best-first.
    bids: BTreeMap<PriceKey, f64>,
    /// price -> quantity, iterated forward for best-first.
    asks: BTreeMap<PriceKey, f64>,
    last_final_update_id: u64,
    updated_at: i64,
}

impl OrderBook {
    /// An empty book for `symbol`.
    #[must_use]
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_final_update_id: 0,
            updated_at: 0,
        }
    }

    /// Replace the whole book with a REST snapshot.
    pub fn apply_snapshot(
        &mut self,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        last_update_id: u64,
        ts: i64,
    ) {
        self.bids.clear();
        self.asks.clear();
        for (price, qty) in bids {
            Self::set(&mut self.bids, *price, *qty);
        }
        for (price, qty) in asks {
            Self::set(&mut self.asks, *price, *qty);
        }
        self.last_final_update_id = last_update_id;
        self.updated_at = ts;
    }

    /// Apply one diff event.
    pub fn apply_diff(
        &mut self,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        final_update_id: u64,
        ts: i64,
    ) {
        for (price, qty) in bids {
            Self::set(&mut self.bids, *price, *qty);
        }
        for (price, qty) in asks {
            Self::set(&mut self.asks, *price, *qty);
        }
        self.last_final_update_id = final_update_id;
        self.updated_at = ts;
    }

    fn set(levels: &mut BTreeMap<PriceKey, f64>, price: f64, qty: f64) {
        let key = PriceKey::new(price);
        if qty <= 0.0 {
            levels.remove(&key);
        } else {
            levels.insert(key, qty);
        }
    }

    /// Symbol this book belongs to.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Last applied `u` (final update id).
    #[must_use]
    pub const fn last_update_id(&self) -> u64 {
        self.last_final_update_id
    }

    /// Timestamp (unix nanos) of the last applied update.
    #[must_use]
    pub const fn updated_at(&self) -> i64 {
        self.updated_at
    }

    /// Best (highest) bid.
    #[must_use]
    pub fn best_bid(&self) -> Option<OrderBookLevel> {
        self.bids.iter().next_back().map(|(k, q)| OrderBookLevel {
            price: k.price(),
            quantity: *q,
        })
    }

    /// Best (lowest) ask.
    #[must_use]
    pub fn best_ask(&self) -> Option<OrderBookLevel> {
        self.asks.iter().next().map(|(k, q)| OrderBookLevel {
            price: k.price(),
            quantity: *q,
        })
    }

    /// Best ask minus best bid, if both sides exist.
    #[must_use]
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some(a.price - b.price),
            _ => None,
        }
    }

    /// Mid price, if both sides exist.
    #[must_use]
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((a.price + b.price) / 2.0),
            _ => None,
        }
    }

    /// Materialize a snapshot of the top `depth` levels per side.
    ///
    /// Bids come out best-first (descending price), asks best-first (ascending
    /// price) -- the convention the DOM panel and `get_orderbook()` expect.
    #[must_use]
    pub fn snapshot(&self, depth: usize) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: self.symbol.clone(),
            timestamp: self.updated_at,
            bids: self
                .bids
                .iter()
                .rev()
                .take(depth)
                .map(|(k, q)| OrderBookLevel {
                    price: k.price(),
                    quantity: *q,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .take(depth)
                .map(|(k, q)| OrderBookLevel {
                    price: k.price(),
                    quantity: *q,
                })
                .collect(),
        }
    }
}

/// Drives the snapshot -> buffer -> bridge -> synced lifecycle.
///
/// Feed it a REST snapshot via [`set_snapshot`](Self::set_snapshot) and then
/// every diff via [`on_diff`](Self::on_diff); it owns the buffering and
/// staleness rules so the collector doesn't have to.
#[derive(Debug)]
pub struct OrderBookSynchronizer {
    symbol: String,
    book: Option<OrderBook>,
    buffer: Vec<DepthDiff>,
    snapshot_id: Option<u64>,
    synced: bool,
    gaps: u64,
}

impl OrderBookSynchronizer {
    /// A synchronizer that has not yet received a snapshot.
    #[must_use]
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            book: None,
            buffer: Vec::new(),
            snapshot_id: None,
            synced: false,
            gaps: 0,
        }
    }

    /// Whether diffs are being applied (as opposed to buffered).
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.synced
    }

    /// Number of sequence gaps detected since construction.
    #[must_use]
    pub const fn gaps(&self) -> u64 {
        self.gaps
    }

    /// The maintained book, if a snapshot has arrived.
    #[must_use]
    pub const fn book(&self) -> Option<&OrderBook> {
        self.book.as_ref()
    }

    /// Install the REST snapshot and drain any diffs that arrived before it.
    pub fn set_snapshot(
        &mut self,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        last_update_id: u64,
        ts: i64,
    ) {
        let mut book = OrderBook::new(self.symbol.clone());
        book.apply_snapshot(bids, asks, last_update_id, ts);
        self.book = Some(book);
        self.snapshot_id = Some(last_update_id);

        let buffered = std::mem::take(&mut self.buffer);
        let mut bridged = false;

        for diff in buffered {
            if diff.final_update_id <= last_update_id {
                continue; // already reflected in the snapshot
            }
            if !bridged {
                if diff.first_update_id <= last_update_id + 1
                    && diff.final_update_id > last_update_id
                {
                    bridged = true;
                } else {
                    continue;
                }
            }
            if let Some(book) = self.book.as_mut() {
                book.apply_diff(&diff.bids, &diff.asks, diff.final_update_id, 0);
            }
        }

        self.synced = bridged;
    }

    /// Handle one diff event.
    pub fn on_diff(&mut self, diff: DepthDiff, ts: i64) -> DiffOutcome {
        let Some(snapshot_id) = self.snapshot_id else {
            self.buffer.push(diff);
            return DiffOutcome::Buffered;
        };

        if diff.final_update_id <= snapshot_id {
            return DiffOutcome::Stale;
        }

        if !self.synced {
            let bridges =
                diff.first_update_id <= snapshot_id + 1 && diff.final_update_id > snapshot_id;
            if !bridges {
                return DiffOutcome::OutOfOrder;
            }
            if let Some(book) = self.book.as_mut() {
                book.apply_diff(&diff.bids, &diff.asks, diff.final_update_id, ts);
            }
            self.synced = true;
            return DiffOutcome::Applied;
        }

        let expected_next = self
            .book
            .as_ref()
            .map_or(snapshot_id, OrderBook::last_update_id)
            + 1;

        let mut gapped = false;
        if diff.first_update_id > expected_next {
            self.gaps += 1;
            gapped = true;
        }

        if let Some(book) = self.book.as_mut() {
            book.apply_diff(&diff.bids, &diff.asks, diff.final_update_id, ts);
        }

        if gapped {
            DiffOutcome::AppliedWithGap
        } else {
            DiffOutcome::Applied
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book_with_snapshot() -> OrderBook {
        let mut b = OrderBook::new("BTCUSDT");
        b.apply_snapshot(
            &[(100.0, 2.0), (99.0, 3.0)],
            &[(101.0, 1.5), (102.0, 4.0)],
            10,
            0,
        );
        b
    }

    #[test]
    fn best_bid_is_highest_and_best_ask_is_lowest() {
        let b = book_with_snapshot();
        assert_eq!(b.best_bid().unwrap().price, 100.0);
        assert_eq!(b.best_ask().unwrap().price, 101.0);
        assert!((b.spread().unwrap() - 1.0).abs() < f64::EPSILON);
        assert!((b.mid().unwrap() - 100.5).abs() < f64::EPSILON);
    }

    #[test]
    fn snapshot_orders_bids_descending_and_asks_ascending() {
        let snap = book_with_snapshot().snapshot(10);
        let bid_prices: Vec<f64> = snap.bids.iter().map(|l| l.price).collect();
        let ask_prices: Vec<f64> = snap.asks.iter().map(|l| l.price).collect();
        assert_eq!(bid_prices, vec![100.0, 99.0]);
        assert_eq!(ask_prices, vec![101.0, 102.0]);
    }

    #[test]
    fn zero_quantity_removes_a_level() {
        let mut b = book_with_snapshot();
        b.apply_diff(&[(100.0, 0.0)], &[], 11, 0);
        assert_eq!(b.best_bid().unwrap().price, 99.0);
    }

    #[test]
    fn diff_updates_existing_level_and_adds_new() {
        let mut b = book_with_snapshot();
        b.apply_diff(&[(100.0, 5.0), (98.0, 1.0)], &[(101.0, 0.25)], 11, 0);
        assert_eq!(b.best_bid().unwrap().quantity, 5.0);
        assert_eq!(b.best_ask().unwrap().quantity, 0.25);
        assert_eq!(b.snapshot(10).bids.len(), 3);
    }

    #[test]
    fn synchronizer_buffers_until_snapshot_arrives() {
        let mut s = OrderBookSynchronizer::new("BTCUSDT");
        let outcome = s.on_diff(
            DepthDiff {
                first_update_id: 1,
                final_update_id: 5,
                bids: vec![],
                asks: vec![],
            },
            0,
        );
        assert_eq!(outcome, DiffOutcome::Buffered);
        assert!(!s.is_synced());
        assert!(s.book().is_none());
    }

    #[test]
    fn synchronizer_drops_stale_and_bridges_on_first_valid_diff() {
        let mut s = OrderBookSynchronizer::new("BTCUSDT");

        // Arrives before the snapshot -- buffered.
        s.on_diff(
            DepthDiff {
                first_update_id: 8,
                final_update_id: 9,
                bids: vec![],
                asks: vec![],
            },
            0,
        );

        // Snapshot at L=10 already contains updates up to 10.
        s.set_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 10, 0);

        // Stale: entirely within the snapshot.
        assert_eq!(
            s.on_diff(
                DepthDiff {
                    first_update_id: 9,
                    final_update_id: 10,
                    bids: vec![],
                    asks: vec![]
                },
                0
            ),
            DiffOutcome::Stale
        );

        // Bridges: covers 11 = L+1.
        assert_eq!(
            s.on_diff(
                DepthDiff {
                    first_update_id: 11,
                    final_update_id: 12,
                    bids: vec![(100.0, 9.0)],
                    asks: vec![]
                },
                0
            ),
            DiffOutcome::Applied
        );
        assert!(s.is_synced());
        assert_eq!(s.book().unwrap().best_bid().unwrap().quantity, 9.0);
    }

    #[test]
    fn synchronizer_detects_sequence_gap_but_still_applies() {
        let mut s = OrderBookSynchronizer::new("BTCUSDT");
        s.set_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 10, 0);
        assert_eq!(
            s.on_diff(
                DepthDiff {
                    first_update_id: 11,
                    final_update_id: 12,
                    bids: vec![],
                    asks: vec![]
                },
                0
            ),
            DiffOutcome::Applied
        );

        // 20 > 12 + 1 -> we missed updates 13..19.
        assert_eq!(
            s.on_diff(
                DepthDiff {
                    first_update_id: 20,
                    final_update_id: 21,
                    bids: vec![],
                    asks: vec![]
                },
                0
            ),
            DiffOutcome::AppliedWithGap
        );
        assert_eq!(s.gaps(), 1);
    }

    #[test]
    fn consecutive_diffs_are_not_reported_as_gaps() {
        let mut s = OrderBookSynchronizer::new("BTCUSDT");
        s.set_snapshot(&[], &[], 10, 0);
        for (u, f) in [(11u64, 12u64), (13, 14), (15, 16)] {
            let outcome = s.on_diff(
                DepthDiff {
                    first_update_id: u,
                    final_update_id: f,
                    bids: vec![],
                    asks: vec![],
                },
                0,
            );
            assert_eq!(outcome, DiffOutcome::Applied);
        }
        assert_eq!(s.gaps(), 0);
    }
}
