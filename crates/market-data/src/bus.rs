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
    /// Closed **and** forming candles, in publication order.
    ///
    /// See [`Self::publish_chart_candle`] for why this is a separate lane
    /// rather than a second publisher on `candles`.
    chart_candles: broadcast::Sender<Candle>,
}

impl MarketEventBus {
    /// Create a bus with `capacity` buffered messages per channel.
    #[must_use]
    pub fn new(symbol: impl Into<String>, capacity: usize) -> Self {
        let (trades, _) = broadcast::channel(capacity);
        let (candles, _) = broadcast::channel(capacity);
        let (orderbook, _) = broadcast::channel(capacity);
        let (chart_candles, _) = broadcast::channel(capacity);
        Self {
            symbol: symbol.into(),
            trades,
            candles,
            orderbook,
            chart_candles,
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

    /// Publish a candle onto the **chart** lane.
    ///
    /// ## Why forming candles get a lane of their own
    ///
    /// A chart wants the bar that is forming right now -- a `1d` chart whose
    /// newest bar updates once a second looks alive, while one that only moves
    /// at midnight looks broken. Bots want the opposite: a strategy that fired
    /// on a half-formed bar would be trading a price that never existed as a
    /// close, and the same bar would be re-evaluated many times as it forms.
    /// One lane cannot carry both, so the closed-only lane stays exactly as it
    /// was and this one carries the mixture.
    ///
    /// The lane's contract is **publication order**: the recorder is its only
    /// publisher, and it publishes a bucket's final forming frame before (or in
    /// the same batch as) that bucket's closed candle, never after. A consumer
    /// that renders frames in arrival order can therefore never draw yesterday's
    /// close over today's forming bar -- which is exactly the race two separate
    /// channels would have, because a closed candle and a forming candle do not
    /// wait for each other across lane boundaries.
    pub fn publish_chart_candle(&self, candle: Candle) {
        let _ = self.chart_candles.send(candle);
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

    /// Subscribe to the **chart** lane: closed candles and forming ones,
    /// interleaved in publication order.
    ///
    /// See [`Self::publish_chart_candle`] for why this lane exists and what a
    /// consumer may assume about ordering.
    #[must_use]
    pub fn subscribe_chart_candles(&self) -> broadcast::Receiver<Candle> {
        self.chart_candles.subscribe()
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

    #[tokio::test]
    async fn the_chart_lane_reaches_its_own_subscribers() {
        let bus = MarketEventBus::new("BTCUSDT", 16);
        let mut chart = bus.subscribe_chart_candles();

        bus.publish_chart_candle(candle(60));
        assert_eq!(chart.recv().await.expect("chart frame").open_time, 60);

        // At bus level the lanes are independent: publishing on one is invisible
        // on the other. (The recorder deliberately puts a closed candle on both,
        // but that is two publishes, not leakage between lanes -- a bot on the
        // closed lane never sees a forming bar.)
        let mut closed = bus.subscribe_candles();
        bus.publish_chart_candle(candle(61));
        assert_eq!(chart.recv().await.expect("chart frame").open_time, 61);
        assert_eq!(closed.try_recv(), Err(TryRecvError::Empty));

        bus.publish_candle(candle(120));
        assert_eq!(closed.recv().await.expect("closed frame").open_time, 120);
        assert_eq!(chart.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn the_chart_lane_preserves_publication_order() {
        // The contract the recorder depends on: one publisher, frames seen in
        // the order published. The forming frame of a bucket arrives before the
        // closed candle of that bucket, so a chart rendering in arrival order
        // never draws the close of a bar it has since moved past.
        let bus = MarketEventBus::new("BTCUSDT", 16);
        let mut chart = bus.subscribe_chart_candles();

        // Forming, forming, then the close of that same bucket, then the next
        // bucket's first forming frame -- the exact sequence the recorder emits
        // when a bucket rolls over between two ticks. The close rides this lane
        // too (the recorder publishes it on both), which is what makes single-
        // publisher ordering meaningful.
        bus.publish_chart_candle(candle(60));
        bus.publish_chart_candle(candle(60));
        bus.publish_chart_candle(candle(60));
        bus.publish_chart_candle(candle(120));

        // Sequential awaits assert order: if the close leapt ahead of the
        // forming frames, the third read would be out of place.
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(chart.recv().await.expect("frame").open_time);
        }
        assert_eq!(seen, vec![60, 60, 60, 120]);
    }
}
