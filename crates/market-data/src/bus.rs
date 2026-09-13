//! Internal pub/sub for live market data.
//!
//! A plain in-process `tokio::sync::broadcast`, one bus per (symbol,
//! stream-type). Deliberately **not** Kafka/NATS: `docs/04-MARKET-DATA-ENGINE.md`
//! and `docs/17-DEPLOYMENT-INFRA.md` both say not to introduce a broker until a
//! measured cross-process need appears (e.g. several api-gateway instances
//! needing the same tick stream).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use analytics_core::{Candle, OrderBookSnapshot, Trade};
use tokio::sync::broadcast;

/// Default per-channel capacity.
///
/// Slow consumers get `Lagged` errors rather than blocking the ingest loop, so
/// this only needs to absorb short bursts.
pub const DEFAULT_CAPACITY: usize = 4096;

/// Fan-out channels for one symbol.
#[derive(Debug)]
pub struct MarketEventBus {
    symbol: String,
    trades: broadcast::Sender<Trade>,
    candles: broadcast::Sender<Candle>,
    orderbook: broadcast::Sender<OrderBookSnapshot>,
}

impl MarketEventBus {
    /// Create a bus with `capacity` buffered messages per channel.
    #[must_use]
    pub fn new(symbol: impl Into<String>, capacity: usize) -> Self {
        let (trades, _) = broadcast::channel(capacity);
        let (candles, _) = broadcast::channel(capacity);
        let (orderbook, _) = broadcast::channel(capacity);
        Self {
            symbol: symbol.into(),
            trades,
            candles,
            orderbook,
        }
    }

    /// Symbol this bus carries.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Publish a trade. Fails only when every receiver has been dropped.
    pub fn publish_trade(&self, trade: Trade) {
        let _ = self.trades.send(trade);
    }

    /// Publish a closed candle.
    pub fn publish_candle(&self, candle: Candle) {
        let _ = self.candles.send(candle);
    }

    /// Publish an order-book snapshot.
    pub fn publish_orderbook(&self, snapshot: OrderBookSnapshot) {
        let _ = self.orderbook.send(snapshot);
    }

    /// Subscribe to trades.
    #[must_use]
    pub fn subscribe_trades(&self) -> broadcast::Receiver<Trade> {
        self.trades.subscribe()
    }

    /// Subscribe to closed candles.
    #[must_use]
    pub fn subscribe_candles(&self) -> broadcast::Receiver<Candle> {
        self.candles.subscribe()
    }

    /// Subscribe to order-book snapshots.
    #[must_use]
    pub fn subscribe_orderbook(&self) -> broadcast::Receiver<OrderBookSnapshot> {
        self.orderbook.subscribe()
    }

    /// Number of live trade subscribers.
    #[must_use]
    pub fn trade_receiver_count(&self) -> usize {
        self.trades.receiver_count()
    }
}

/// Holds one bus per symbol, creating them on demand.
#[derive(Debug, Default)]
pub struct MarketBusRegistry {
    buses: RwLock<HashMap<String, Arc<MarketEventBus>>>,
}

impl MarketBusRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The bus for `symbol`, created on first access.
    #[must_use]
    pub fn bus(&self, symbol: &str) -> Arc<MarketEventBus> {
        if let Some(bus) = self.buses.read().ok().and_then(|m| m.get(symbol).cloned()) {
            return bus;
        }

        let mut map = self.buses.write().expect("bus registry lock poisoned");
        map.entry(symbol.to_string())
            .or_insert_with(|| Arc::new(MarketEventBus::new(symbol, DEFAULT_CAPACITY)))
            .clone()
    }

    /// Every symbol currently registered.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        self.buses
            .read()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::{OrderBookLevel, Timeframe};
    use tokio::sync::broadcast::error::TryRecvError;

    fn candle(open_time: i64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    #[test]
    fn publishing_with_no_subscribers_is_a_noop() {
        let bus = MarketEventBus::new("BTCUSDT", 16);
        bus.publish_candle(candle(1)); // must not panic
        assert_eq!(bus.trade_receiver_count(), 0);
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = MarketEventBus::new("BTCUSDT", 16);
        let mut rx = bus.subscribe_candles();

        bus.publish_candle(candle(60));
        let received = rx.recv().await.expect("should receive candle");
        assert_eq!(received.open_time, 60);
    }

    #[test]
    fn registry_returns_the_same_bus_per_symbol() {
        let registry = MarketBusRegistry::new();
        let a = registry.bus("BTCUSDT");
        let b = registry.bus("BTCUSDT");
        assert!(Arc::ptr_eq(&a, &b));

        let eth = registry.bus("ETHUSDT");
        assert!(!Arc::ptr_eq(&a, &eth));
        assert_eq!(registry.symbols().len(), 2);
    }

    #[test]
    fn orderbook_events_reach_subscribers() {
        let bus = MarketEventBus::new("BTCUSDT", 16);
        let mut rx = bus.subscribe_orderbook();
        bus.publish_orderbook(OrderBookSnapshot {
            symbol: "BTCUSDT".into(),
            timestamp: 42,
            bids: vec![OrderBookLevel {
                price: 100.0,
                quantity: 1.0,
            }],
            asks: vec![],
        });
        let snap = rx.try_recv().expect("should receive snapshot");
        assert_eq!(snap.timestamp, 42);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }
}
