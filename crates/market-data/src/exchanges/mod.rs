//! Exchange collectors.
//!
//! The [`ExchangeCollector`] trait is the seam that keeps ingestion
//! exchange-agnostic: adding a second venue means implementing this trait, not
//! touching the analytics, persistence or agent layers
//! (`docs/04-MARKET-DATA-ENGINE.md`).

pub mod binance;
pub mod wire;

pub use binance::{BinanceCollector, BinanceConfig};

use analytics_core::{Candle, OrderBookSnapshot, Trade};
use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::error::MarketDataError;
use crate::health::HealthStatus;

/// A live connection to one exchange.
///
/// Implementations own reconnect/backoff and gap detection internally and
/// publish normalized data to the internal bus.
#[async_trait]
pub trait ExchangeCollector: Send + Sync {
    /// Exchange name, e.g. `"binance"`.
    fn name(&self) -> &'static str;

    /// Open the connection and start the ingest loop.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if the initial connection fails.
    async fn connect(&mut self) -> Result<(), MarketDataError>;

    /// Subscribe to the trade stream for `symbol`.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if not connected.
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), MarketDataError>;

    /// Subscribe to the order-book diff stream for `symbol`.
    ///
    /// The implementation fetches a REST snapshot and keeps the book
    /// synchronized from diffs; subscribers receive periodic snapshots.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if not connected, or
    /// [`MarketDataError::Normalization`] if the REST snapshot is unusable.
    async fn subscribe_order_book(&mut self, symbol: &str) -> Result<(), MarketDataError>;

    /// Subscribe to the trade stream for `symbol`, if it has one.
    fn trade_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Trade>>;

    /// Subscribe to the order-book stream for `symbol`, if it has one.
    fn order_book_stream(&self, symbol: &str) -> Option<broadcast::Receiver<OrderBookSnapshot>>;

    /// Subscribe to the closed-candle stream for `symbol`, if it has one.
    ///
    /// ## Why this is on the trait and not left to the bus
    ///
    /// The collector already aggregates trades into every resolution the
    /// platform uses and publishes the closed ones, but until this method
    /// existed nothing outside `market-data` could reach them: `xtask collect`
    /// subscribed to trades and order books only, so a collector run wrote
    /// zero candles and the candle table could only ever be filled by
    /// `backfill`. Phase 1's exit criterion is about a *collector* run
    /// producing a candle table, and it could not be met.
    ///
    /// A collector that does not aggregate candles returns `None`, the same
    /// contract the other two streams have.
    fn candle_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Candle>>;

    /// Current health of the connection.
    fn health(&self) -> HealthStatus;
}
