//! Recent trades and the newest book, held in RAM.
//!
//! ## Why a tape and not a table
//!
//! A footprint chart is built from **individual trades**, and trades are the
//! most expensive thing this platform could store: ~110 MB/day/symbol against a
//! database with 6 GB for every symbol of every market. So the tape is a ring
//! buffer in memory, bounded by count, and it is the only place trades live.
//!
//! ## What that costs, stated up front
//!
//! A `Trade` is ~64 bytes. The default 100,000-trade tape is therefore ~6.4 MB
//! per symbol -- cheap for five symbols, ~130 MB at twenty, and the knob to
//! turn is the count, not the design.
//!
//! The consequence is the one that has to be said out loud rather than hidden:
//! **100,000 trades is about 33 minutes of BTCUSDT** at ~50 trades/second. A
//! footprint is therefore a *recent* chart. A window older than the tape is not
//! served, and the route says so with the tape's own span rather than drawing
//! something invented. There is no way around this without storing trades, and
//! storing trades is the thing the 6 GB forbids.
//!
//! ## The book is the cheap half
//!
//! An order-book snapshot is ~1.3 KB. Keeping the newest one per symbol costs
//! nothing and is strictly better than reading a stored one: `/orderbook`
//! answers with the book as it is *now*, rather than the last snapshot a pump
//! happened to write.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use analytics_core::{OrderBookSnapshot, Trade};

/// Trades kept per symbol by default.
///
/// ~6.4 MB per symbol at ~64 bytes a trade. Sized so a footprint has something
/// to draw on a liquid symbol without the tape being the biggest thing in the
/// process.
pub const DEFAULT_TAPE_TRADES: usize = 100_000;

/// One symbol's recent trades, oldest first.
///
/// All methods take `&self`; sharing a tape across tasks is `Arc` and needs no
/// exterior lock.
#[derive(Debug)]
pub struct TradeTape {
    capacity: usize,
    trades: RwLock<VecDeque<Trade>>,
}

impl TradeTape {
    /// An empty tape holding at most `capacity` trades.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            trades: RwLock::new(VecDeque::new()),
        }
    }

    /// An empty tape with [`DEFAULT_TAPE_TRADES`].
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_TAPE_TRADES)
    }

    /// How many trades this tape holds on to.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record one trade.
    ///
    /// A trade older than the newest one on the tape is dropped: the tape is a
    /// chronological window, and splicing an older trade in would put it out of
    /// order for every reader that walks it by time.
    pub fn push(&self, trade: Trade) {
        let mut trades = match self.trades.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(last) = trades.back() {
            if trade.timestamp < last.timestamp {
                return;
            }
        }

        trades.push_back(trade);
        while trades.len() > self.capacity {
            trades.pop_front();
        }
    }

    /// Every trade in `[from_ns, to_ns)`, oldest first.
    #[must_use]
    pub fn range(&self, from_ns: i64, to_ns: i64) -> Vec<Trade> {
        self.trades
            .read()
            .map(|trades| {
                trades
                    .iter()
                    .filter(|t| t.timestamp >= from_ns && t.timestamp < to_ns)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Timestamp of the oldest trade on the tape.
    #[must_use]
    pub fn earliest_ns(&self) -> Option<i64> {
        self.trades
            .read()
            .ok()
            .and_then(|trades| trades.front().map(|t| t.timestamp))
    }

    /// Timestamp of the newest trade on the tape.
    #[must_use]
    pub fn newest_ns(&self) -> Option<i64> {
        self.trades
            .read()
            .ok()
            .and_then(|trades| trades.back().map(|t| t.timestamp))
    }

    /// How many trades are on the tape.
    #[must_use]
    pub fn len(&self) -> usize {
        self.trades.read().map(|trades| trades.len()).unwrap_or(0)
    }

    /// Whether the tape holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The newest order-book snapshot per symbol.
///
/// Not a history: one snapshot per symbol, replaced as they arrive. Everything
/// that reads a book wants the current one.
#[derive(Debug, Default)]
pub struct BookCache {
    books: RwLock<HashMap<String, OrderBookSnapshot>>,
}

impl BookCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the book for its symbol.
    pub fn set(&self, snapshot: OrderBookSnapshot) {
        if let Ok(mut books) = self.books.write() {
            books.insert(snapshot.symbol.to_uppercase(), snapshot);
        }
    }

    /// The newest book for `symbol`, if any has arrived.
    #[must_use]
    pub fn get(&self, symbol: &str) -> Option<OrderBookSnapshot> {
        self.books
            .read()
            .ok()
            .and_then(|books| books.get(&symbol.to_uppercase()).cloned())
    }

    /// Every symbol with a book, sorted.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .books
            .read()
            .map(|books| books.keys().cloned().collect())
            .unwrap_or_default();
        out.sort_unstable();
        out
    }
}

/// When a book was first waited on, and when the newest one arrived.
///
/// This exists because of `docs/19` row 24, and it is worth saying why a
/// *timestamp* rather than a `synced` flag. A book that has never bridged onto
/// its snapshot publishes nothing at all, so from outside the platform it is
/// indistinguishable from a book nobody has asked for: the DOM pane is empty
/// and `/orderbook` answers 404 either way. Age separates the two. A flag would
/// have to be maintained by the collector, deep inside its pump, and would
/// report the state of the synchroniser rather than the thing a user notices.
#[derive(Debug, Clone, Copy)]
struct BookFreshness {
    /// When the platform first expected a book for this symbol.
    expected_ns: i64,
    /// When the newest book arrived. `None` until one does.
    newest_ns: Option<i64>,
}

/// The live, in-memory half of market data: a tape and a book per symbol.
///
/// Deliberately one handle rather than two, because both are fed by the same
/// task from the same bus and every consumer wants both.
#[derive(Debug, Default)]
pub struct LiveRegistry {
    tapes: RwLock<HashMap<String, Arc<TradeTape>>>,
    /// The newest book per symbol.
    pub books: BookCache,
    /// How long each book has been waited on, and how long since one arrived.
    freshness: RwLock<HashMap<String, BookFreshness>>,
    /// Trades dropped because they arrived out of order, for the log.
    dropped: RwLock<u64>,
}

impl LiveRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The tape for `symbol`, created on first access.
    #[must_use]
    pub fn tape(&self, symbol: &str) -> Arc<TradeTape> {
        let key = symbol.to_uppercase();
        if let Some(found) = self.tapes.read().ok().and_then(|m| m.get(&key).cloned()) {
            return found;
        }

        let mut map = match self.tapes.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(key)
            .or_insert_with(|| Arc::new(TradeTape::with_default_capacity()))
            .clone()
    }

    /// Record a trade into its symbol's tape.
    pub fn record_trade(&self, trade: &Trade) {
        let tape = self.tape(&trade.symbol);
        let before = tape.len();
        tape.push(trade.clone());
        if tape.len() == before {
            if let Ok(mut dropped) = self.dropped.write() {
                *dropped += 1;
            }
        }
    }

    /// Record a book snapshot, and mark the symbol's book fresh.
    pub fn record_book(&self, snapshot: &OrderBookSnapshot) {
        self.books.set(snapshot.clone());

        // Monotonic: a book that arrives out of order must not make the symbol
        // look older than it is. The whole point of this number is the moment a
        // book *stops* arriving, and one late snapshot must not fake that.
        if let Ok(mut map) = self.freshness.write() {
            let entry = map
                .entry(snapshot.symbol.to_uppercase())
                .or_insert(BookFreshness {
                    expected_ns: snapshot.timestamp,
                    newest_ns: None,
                });
            entry.newest_ns = Some(entry.newest_ns.map_or(snapshot.timestamp, |prev| {
                prev.max(snapshot.timestamp)
            }));
        }
    }

    /// Say that a book is expected for `symbol` from `now_ns` onwards.
    ///
    /// Idempotent: only the first call starts the clock, so something asking
    /// the same question on every scrape cannot keep resetting it.
    pub fn expect_book(&self, symbol: &str, now_ns: i64) {
        if let Ok(mut map) = self.freshness.write() {
            map.entry(symbol.to_uppercase()).or_insert(BookFreshness {
                expected_ns: now_ns,
                newest_ns: None,
            });
        }
    }

    /// How long `symbol` has been without a book, in nanoseconds.
    ///
    /// Measured from the newest book, or from when a book was first expected if
    /// none has ever arrived -- that second case is the one that has to be
    /// visible, because it is the only difference between "the book is late"
    /// and "the book has never worked".
    ///
    /// Registers the symbol as expected if it is not already, so a caller
    /// cannot measure an age and forget to start the clock.
    #[must_use]
    pub fn book_age_ns(&self, symbol: &str, now_ns: i64) -> Option<i64> {
        self.expect_book(symbol, now_ns);
        let map = self.freshness.read().ok()?;
        let entry = map.get(&symbol.to_uppercase())?;
        Some(now_ns - entry.newest_ns.unwrap_or(entry.expected_ns))
    }

    /// The age of every book the platform is waiting on, sorted by symbol.
    ///
    /// Only symbols somebody has actually waited on appear. A symbol nobody has
    /// asked for is not stale, and reporting it would make a healthy platform
    /// look broken on every restart.
    #[must_use]
    pub fn book_ages(&self, now_ns: i64) -> Vec<(String, i64)> {
        let Ok(map) = self.freshness.read() else {
            return Vec::new();
        };
        let mut out: Vec<(String, i64)> = map
            .iter()
            .map(|(symbol, fresh)| {
                (
                    symbol.clone(),
                    now_ns - fresh.newest_ns.unwrap_or(fresh.expected_ns),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The newest book for `symbol`.
    #[must_use]
    pub fn book(&self, symbol: &str) -> Option<OrderBookSnapshot> {
        self.books.get(symbol)
    }

    /// Trades dropped for arriving out of order.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.read().map(|d| *d).unwrap_or(0)
    }

    /// Every symbol with a tape or a book, sorted.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let mut out = self.books.symbols();
        if let Ok(tapes) = self.tapes.read() {
            out.extend(tapes.keys().cloned());
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;

    fn trade_at(ts: i64, price: f64) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: ts as u64,
            price,
            quantity: 1.0,
            is_buyer_maker: false,
            timestamp: ts,
        }
    }

    fn book_at(symbol: &str, ts: i64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: symbol.into(),
            timestamp: ts,
            bids: vec![],
            asks: vec![],
        }
    }

    #[test]
    fn a_tape_keeps_only_its_capacity() {
        let tape = TradeTape::new(3);
        for i in 1..=5 {
            tape.push(trade_at(SECOND * i, i as f64));
        }
        assert_eq!(tape.len(), 3);
        assert_eq!(tape.earliest_ns(), Some(SECOND * 3));
        assert_eq!(tape.newest_ns(), Some(SECOND * 5));
    }

    #[test]
    fn a_trade_older_than_the_newest_is_dropped_and_counted() {
        let registry = LiveRegistry::new();
        registry.record_trade(&trade_at(SECOND * 5, 5.0));
        registry.record_trade(&trade_at(SECOND, 1.0));

        assert_eq!(registry.tape("BTCUSDT").len(), 1);
        assert_eq!(registry.dropped(), 1, "an out-of-order trade must be visible");
    }

    #[test]
    fn a_window_returns_only_the_trades_inside_it() {
        let tape = TradeTape::new(16);
        for i in 1..=5 {
            tape.push(trade_at(SECOND * i, i as f64));
        }
        let window = tape.range(SECOND * 2, SECOND * 4);
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].timestamp, SECOND * 2);
        assert_eq!(window[1].timestamp, SECOND * 3);
    }

    #[test]
    fn an_empty_tape_has_no_span() {
        let tape = TradeTape::with_default_capacity();
        assert!(tape.is_empty());
        assert_eq!(tape.earliest_ns(), None);
        assert_eq!(tape.newest_ns(), None);
        assert_eq!(tape.capacity(), DEFAULT_TAPE_TRADES);
    }

    #[test]
    fn the_book_holds_the_newest_snapshot_per_symbol() {
        let cache = BookCache::new();
        assert_eq!(cache.get("BTCUSDT"), None);

        cache.set(book_at("btcusdt", 1));
        cache.set(book_at("BTCUSDT", 2));
        cache.set(book_at("ETHUSDT", 3));

        assert_eq!(cache.get("BTCUSDT").map(|b| b.timestamp), Some(2));
        assert_eq!(cache.symbols(), vec!["BTCUSDT", "ETHUSDT"]);
    }

    #[test]
    fn the_registry_returns_one_tape_per_symbol() {
        let registry = LiveRegistry::new();
        let a = registry.tape("btcusdt");
        let again = registry.tape("BTCUSDT");
        assert!(Arc::ptr_eq(&a, &again));

        registry.record_trade(&trade_at(SECOND, 1.0));
        assert_eq!(a.len(), 1);
        assert!(registry.tape("ETHUSDT").is_empty());
    }

    #[test]
    fn the_registry_lists_symbols_from_either_half() {
        let registry = LiveRegistry::new();
        registry.record_trade(&trade_at(SECOND, 1.0));
        registry.record_book(&book_at("ETHUSDT", SECOND));

        assert_eq!(registry.symbols(), vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(registry.book("ETHUSDT").map(|b| b.timestamp), Some(SECOND));
        assert_eq!(registry.book("BTCUSDT"), None);
    }

    /// The case `docs/19` row 24 was about: a book that never arrives. If the
    /// age were measured only from the newest book, a symbol that has never
    /// synced would have no number at all, which is exactly the silence that
    /// hid the defect for a whole run.
    #[test]
    fn a_book_that_has_never_arrived_is_aged_from_when_it_was_first_expected() {
        let registry = LiveRegistry::new();
        registry.expect_book("btcusdt", 100 * SECOND);

        assert_eq!(
            registry.book_age_ns("BTCUSDT", 130 * SECOND),
            Some(30 * SECOND)
        );
        assert!(registry.book("BTCUSDT").is_none());
    }

    /// Only the first `expect_book` starts the clock -- otherwise a scrape
    /// asking every 30 seconds would reset the age to zero forever and the
    /// number could never exceed the threshold.
    #[test]
    fn expecting_a_book_twice_does_not_move_the_clock() {
        let registry = LiveRegistry::new();
        registry.expect_book("BTCUSDT", 100 * SECOND);
        registry.expect_book("BTCUSDT", 120 * SECOND);

        assert_eq!(
            registry.book_age_ns("BTCUSDT", 130 * SECOND),
            Some(30 * SECOND)
        );
    }

    #[test]
    fn a_book_that_arrives_resets_the_age_and_a_late_one_does_not_reverse_it() {
        let registry = LiveRegistry::new();
        registry.expect_book("BTCUSDT", 100 * SECOND);
        registry.record_book(&book_at("btcusdt", 110 * SECOND));
        assert_eq!(
            registry.book_age_ns("BTCUSDT", 130 * SECOND),
            Some(20 * SECOND)
        );

        // Out of order, so it must not make the symbol look older.
        registry.record_book(&book_at("BTCUSDT", 105 * SECOND));
        assert_eq!(
            registry.book_age_ns("BTCUSDT", 130 * SECOND),
            Some(20 * SECOND)
        );
    }

    /// A symbol nobody has waited on is not stale. Reporting every symbol in
    /// the watchlist before its feed has even started would make a healthy
    /// restart look like an outage.
    #[test]
    fn a_symbol_nobody_has_waited_on_is_not_reported() {
        let registry = LiveRegistry::new();
        registry.record_book(&book_at("ETHUSDT", SECOND));

        assert_eq!(registry.book_ages(10 * SECOND), vec![("ETHUSDT".to_string(), 9 * SECOND)]);
        assert_eq!(registry.book_ages(10 * SECOND).len(), 1);
    }

    /// Measuring the age is enough to start the clock, so a caller cannot read
    /// a freshness it never registered and get `None` forever.
    #[test]
    fn asking_for_an_age_registers_the_symbol_as_expected() {
        let registry = LiveRegistry::new();

        assert_eq!(registry.book_age_ns("BTCUSDT", 50 * SECOND), Some(0));
        assert_eq!(registry.book_ages(60 * SECOND), vec![("BTCUSDT".to_string(), 10 * SECOND)]);
    }
}
