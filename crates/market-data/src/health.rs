//! Health reporting and trade-id gap detection.
//!
//! Every engine must expose health/readiness from Phase 1 onward so deployment
//! and observability have something to hook into
//! (`docs/04-MARKET-DATA-ENGINE.md`, `docs/18-OBSERVABILITY.md`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use analytics_core::Trade;
use observability::metrics::{
    Labels, Registry, MD_CONNECTED, MD_DECODE_ERRORS, MD_GAPS, MD_MESSAGES, MD_RECONNECTS,
};

/// Lock-free counters describing one collector's live state.
#[derive(Debug, Default)]
pub struct CollectorHealth {
    connected: AtomicBool,
    last_message_ns: AtomicI64,
    messages: AtomicU64,
    reconnects: AtomicU64,
    decode_errors: AtomicU64,
    gaps: AtomicU64,
    /// What was published last time, so a total can be turned into a delta.
    ///
    /// A `Registry` counter accumulates; this struct holds absolutes since
    /// process start. Publishing the absolute value into a counter would make
    /// the counter jump to the total on every scrape and `rate()` would see
    /// spikes that never happened -- the deltas are what makes these numbers
    /// mean the same thing as every other counter in the platform.
    published: Mutex<Published>,
}

/// The last absolute totals handed to a registry.
#[derive(Debug, Default, Clone, Copy)]
struct Published {
    messages: u64,
    reconnects: u64,
    gaps: u64,
    decode_errors: u64,
}

impl CollectorHealth {
    /// A fresh, disconnected health record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the socket as connected/disconnected.
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }

    /// Record that a message arrived at `ts` (unix nanos).
    pub fn record_message(&self, ts: i64) {
        self.last_message_ns.store(ts, Ordering::Relaxed);
        self.messages.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a reconnect attempt.
    pub fn record_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a detected sequence gap.
    pub fn record_gap(&self) {
        self.gaps.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a frame on a known topic that could not be decoded.
    ///
    /// Distinct from a message: a decode error is the venue drifting away from
    /// our model of it, and it is the one failure that the collector
    /// deliberately *does not* treat as fatal, so without a counter it would be
    /// visible only in a log line nobody is reading at 3am.
    pub fn record_decode_error(&self) {
        self.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Total frames that reached a known topic but did not decode.
    #[must_use]
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors.load(Ordering::Relaxed)
    }

    /// Whether the socket is currently connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Timestamp of the last message, or `None` if none has arrived.
    #[must_use]
    pub fn last_message_ns(&self) -> Option<i64> {
        match self.last_message_ns.load(Ordering::Relaxed) {
            0 => None,
            ts => Some(ts),
        }
    }

    /// Total messages received.
    #[must_use]
    pub fn messages(&self) -> u64 {
        self.messages.load(Ordering::Relaxed)
    }

    /// Total reconnects.
    #[must_use]
    pub fn reconnects(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }

    /// Total detected gaps.
    #[must_use]
    pub fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// Snapshot for a health endpoint or log line.
    #[must_use]
    pub fn snapshot(&self, streams: Vec<String>) -> HealthStatus {
        HealthStatus {
            connected: self.is_connected(),
            last_message_ns: self.last_message_ns(),
            messages: self.messages(),
            reconnects: self.reconnects(),
            gaps: self.gaps(),
            streams,
        }
    }

    /// Publish this collector's state into a metric registry.
    ///
    /// Called on a ticker rather than per message. Per message would put a
    /// registry lock on the hot path of a stream that carries every trade on
    /// the venue, to update numbers nobody reads more than once every fifteen
    /// seconds; a ticker costs one lock a second and loses nothing, because a
    /// counter that is one second behind is still a counter.
    ///
    /// The three totals are published as *deltas* since the last call. See
    /// [`Published`].
    pub fn publish(&self, registry: &Registry, venue: &str) {
        let labels = Labels::new(&[("venue", venue)]);

        registry.set_gauge(
            MD_CONNECTED,
            "Whether the exchange socket is connected",
            &labels,
            if self.is_connected() { 1.0 } else { 0.0 },
        );

        let Ok(mut published) = self.published.lock() else {
            return;
        };
        let now = Published {
            messages: self.messages(),
            reconnects: self.reconnects(),
            gaps: self.gaps(),
            decode_errors: self.decode_errors(),
        };

        // `saturating_sub` rather than `-`: the atomics are only ever added to,
        // but a counter that went backwards would otherwise underflow into a
        // number near u64::MAX and be reported as an enormous burst.
        //
        // Called even when the delta is zero, so the counter is *defined* and
        // appears in the exposition from the first tick. A counter that only
        // materialises once it has moved cannot be graphed as flat, and a
        // dashboard showing nothing is indistinguishable from a collector that
        // never started.
        for (delta, name, help) in [
            (
                now.messages.saturating_sub(published.messages),
                MD_MESSAGES,
                "Exchange messages received",
            ),
            (
                now.reconnects.saturating_sub(published.reconnects),
                MD_RECONNECTS,
                "Exchange socket reconnects",
            ),
            (
                now.gaps.saturating_sub(published.gaps),
                MD_GAPS,
                "Trade-id gaps detected",
            ),
            (
                now.decode_errors.saturating_sub(published.decode_errors),
                MD_DECODE_ERRORS,
                "Frames on a known topic that did not decode",
            ),
        ] {
            registry.inc_counter(name, help, &labels, delta);
        }

        *published = now;
    }
}

/// How often a collector's counters are copied into the registry.
pub const HEALTH_PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Publish a collector's counters into a registry, forever.
///
/// Returns the task handle so a caller can stop it; the collector itself has no
/// interest in stopping it, because a task that publishes "the socket is down"
/// is most useful exactly when the socket is down.
#[must_use]
pub fn spawn_health_publisher(
    health: Arc<CollectorHealth>,
    registry: Arc<Registry>,
    venue: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEALTH_PUBLISH_INTERVAL);
        loop {
            ticker.tick().await;
            health.publish(&registry, venue);
        }
    })
}

/// A point-in-time health report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthStatus {
    /// Whether the exchange socket is connected.
    pub connected: bool,
    /// Timestamp of the most recent message (unix nanos), if any.
    pub last_message_ns: Option<i64>,
    /// Messages received since start.
    pub messages: u64,
    /// Reconnect attempts since start.
    pub reconnects: u64,
    /// Sequence gaps detected since start.
    pub gaps: u64,
    /// Streams currently subscribed.
    pub streams: Vec<String>,
}

/// Detects missing trades by watching for jumps in exchange trade ids.
///
/// A gap means we lost messages (a dropped WS frame, a reconnect window) and
/// must REST-resync that window -- silently continuing would corrupt delta/CVD.
#[derive(Debug, Default)]
pub struct TradeGapDetector {
    last_id: HashMap<String, u64>,
    gaps: u64,
}

impl TradeGapDetector {
    /// An empty detector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a trade; returns `(expected_next_id, actual_id)` when a gap is seen.
    ///
    /// The first trade for a symbol just seeds the baseline. Ids are allowed to
    /// go backwards (some venues restart id sequences), which is not treated as
    /// a gap.
    pub fn on_trade(&mut self, trade: &Trade) -> Option<(u64, u64)> {
        let id = trade.trade_id;

        match self.last_id.get_mut(&trade.symbol) {
            None => {
                self.last_id.insert(trade.symbol.clone(), id);
                None
            }
            Some(last) => {
                let expected = *last + 1;
                if id > expected {
                    self.gaps += 1;
                    *last = id;
                    Some((expected, id))
                } else {
                    *last = (*last).max(id);
                    None
                }
            }
        }
    }

    /// Total gaps detected.
    #[must_use]
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// Last seen id for a symbol, if any.
    #[must_use]
    pub fn last_id(&self, symbol: &str) -> Option<u64> {
        self.last_id.get(symbol).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(symbol: &str, id: u64) -> Trade {
        Trade {
            symbol: symbol.into(),
            trade_id: id,
            price: 100.0,
            quantity: 1.0,
            is_buyer_maker: false,
            timestamp: 0,
        }
    }

    #[test]
    fn first_trade_seeds_without_reporting_a_gap() {
        let mut d = TradeGapDetector::new();
        assert_eq!(d.on_trade(&trade("BTCUSDT", 100)), None);
        assert_eq!(d.gaps(), 0);
        assert_eq!(d.last_id("BTCUSDT"), Some(100));
    }

    #[test]
    fn consecutive_ids_are_not_gaps() {
        let mut d = TradeGapDetector::new();
        for id in 100..=105 {
            assert_eq!(d.on_trade(&trade("BTCUSDT", id)), None);
        }
        assert_eq!(d.gaps(), 0);
    }

    #[test]
    fn skipped_ids_report_the_expected_and_actual() {
        let mut d = TradeGapDetector::new();
        d.on_trade(&trade("BTCUSDT", 100));
        assert_eq!(d.on_trade(&trade("BTCUSDT", 103)), Some((101, 103)));
        assert_eq!(d.gaps(), 1);
        assert_eq!(d.last_id("BTCUSDT"), Some(103));
    }

    #[test]
    fn symbols_are_tracked_independently() {
        let mut d = TradeGapDetector::new();
        d.on_trade(&trade("BTCUSDT", 10));
        d.on_trade(&trade("ETHUSDT", 500));
        assert_eq!(d.on_trade(&trade("BTCUSDT", 12)), Some((11, 12)));
        assert_eq!(d.on_trade(&trade("ETHUSDT", 501)), None);
        assert_eq!(d.gaps(), 1);
    }

    #[test]
    fn health_counters_and_status() {
        let h = CollectorHealth::new();
        assert!(!h.is_connected());
        assert_eq!(h.last_message_ns(), None);

        h.set_connected(true);
        h.record_message(1_000);
        h.record_message(2_000);
        h.record_reconnect();
        h.record_gap();

        let status = h.snapshot(vec!["btcusdt@trade".into()]);
        assert!(status.connected);
        assert_eq!(status.last_message_ns, Some(2_000));
        assert_eq!(status.messages, 2);
        assert_eq!(status.reconnects, 1);
        assert_eq!(status.gaps, 1);
        assert_eq!(status.streams, vec!["btcusdt@trade".to_string()]);
    }

    #[test]
    fn publishing_sends_deltas_rather_than_running_totals() {
        // The bug this is here to catch: `CollectorHealth` holds absolutes since
        // process start, and a `Registry` counter accumulates. Publishing
        // `self.messages()` on every tick would make the counter climb by the
        // whole total each time -- 2, 4, 6, 8 -- so `rate()` would show a burst
        // that never happened and the feed would look busier the longer it ran.
        use observability::metrics::{Labels, Registry, MD_CONNECTED, MD_GAPS, MD_MESSAGES};
        let registry = Registry::new();
        let labels = Labels::new(&[("venue", "binance")]);
        let health = CollectorHealth::new();

        health.set_connected(true);
        health.record_message(1_000);
        health.record_message(2_000);
        health.record_gap();
        health.publish(&registry, "binance");

        assert_eq!(registry.gauge(MD_CONNECTED, &labels), Some(1.0));
        assert_eq!(registry.counter(MD_MESSAGES, &labels), 2);
        assert_eq!(registry.counter(MD_GAPS, &labels), 1);

        // Nothing new happened, so nothing moves.
        health.publish(&registry, "binance");
        assert_eq!(
            registry.counter(MD_MESSAGES, &labels),
            2,
            "a second publish with no new messages must add nothing"
        );

        // One more message moves it by exactly one.
        health.record_message(3_000);
        health.publish(&registry, "binance");
        assert_eq!(registry.counter(MD_MESSAGES, &labels), 3);

        // A disconnect is a level, so it goes back to zero rather than counting.
        health.set_connected(false);
        health.publish(&registry, "binance");
        assert_eq!(registry.gauge(MD_CONNECTED, &labels), Some(0.0));
    }

    #[test]
    fn a_collector_that_never_connected_publishes_a_disconnected_gauge() {
        // The gauge has to exist at zero from the first tick. An absent metric
        // and a metric reading zero are the same thing to a graph but not to an
        // alert: "the collector never started" would otherwise be silent.
        use observability::metrics::{Labels, Registry, MD_CONNECTED, MD_MESSAGES};
        let registry = Registry::new();
        let labels = Labels::new(&[("venue", "binance")]);
        CollectorHealth::new().publish(&registry, "binance");

        assert_eq!(registry.gauge(MD_CONNECTED, &labels), Some(0.0));
        assert_eq!(registry.counter(MD_MESSAGES, &labels), 0);
        assert!(
            registry.render().contains("market_data_messages_total"),
            "the counter must be defined from the first tick, so a flat line is \
             visible rather than missing"
        );
    }
}
