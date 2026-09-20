//! Exchange collectors.
//!
//! ## Two seams, not one
//!
//! The [`ExchangeCollector`] trait is the seam that keeps ingestion
//! exchange-agnostic: adding a second venue means implementing this trait, not
//! touching the analytics, persistence or agent layers
//! (`docs/04-MARKET-DATA-ENGINE.md`).
//!
//! That sentence was written about **REST** and it is true there -- `venue.rs`
//! is that seam and it holds. It was **false for live**, and the honest version
//! is worth writing down because the false version cost a session: the live
//! path used to name Binance in five places (a two-name `FeedMode`, three
//! `BinanceCollector::with_defaults` call sites, and a decoder that was
//! Binance-shaped by its own doc comment), so a second venue could not be added
//! by implementing a trait.
//!
//! The live seam is now [`WireCodec`]: a venue describes its socket and decodes
//! its own frames, and one generic [`Collector`] holds the reconnect loop, the
//! diff buffer, the gap detector, the candle fanout and every other thing that
//! was a fixed defect. See `collector.rs` for why that loop is not duplicated
//! per venue.

pub mod binance_codec;
pub mod bybit_codec;
pub mod codec;
pub mod collector;
pub mod venue;
pub mod wire;

pub use binance_codec::{BinanceCodec, BINANCE_WS_URL};
pub use bybit_codec::{
    BybitCodec, BYBIT_BOOK_DEPTH, BYBIT_INVERSE_WS, BYBIT_LINEAR_WS, BYBIT_REST, BYBIT_SPOT_WS,
};
pub use codec::{DepthDiff as CodecDepthDiff, Frame, Incoming, Subscription, WireCodec};
pub use collector::{BookBootstrap, Collector, CollectorConfig, SnapshotFetcher};
pub use venue::{BinanceVenue, BybitVenue, Columns, KlinePage, RawKline, Venue};

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

    /// The live health counters behind [`Self::health`].
    ///
    /// On the trait rather than only on `Collector` because a caller that holds
    /// a `Box<dyn ExchangeCollector>` -- which is what venue selection produces,
    /// since two collectors are two different types -- otherwise cannot read the
    /// counters at all. `xtask collect`'s status line is exactly that caller, and
    /// without this the venue-aware version could not report gaps or reconnects.
    fn health_counters(&self) -> std::sync::Arc<crate::health::CollectorHealth>;
}
