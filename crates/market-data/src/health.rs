//! Health reporting and trade-id gap detection.
//!
//! Every engine must expose health/readiness from Phase 1 onward so deployment
//! and observability have something to hook into
//! (`docs/04-MARKET-DATA-ENGINE.md`, `docs/18-OBSERVABILITY.md`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use analytics_core::Trade;

/// Lock-free counters describing one collector's live state.
#[derive(Debug, Default)]
pub struct CollectorHealth {
    connected: AtomicBool,
    last_message_ns: AtomicI64,
    messages: AtomicU64,
    reconnects: AtomicU64,
    gaps: AtomicU64,
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
}
