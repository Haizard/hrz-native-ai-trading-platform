//! # `market-data`
//!
//! Ingests, normalizes and persists real-time and historical market data
//! (trades, order book, OHLCV) from exchanges, and republishes it internally.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`exchanges`] | `ExchangeCollector` trait + `BinanceCollector` (WebSocket + REST) |
//! | [`orderbook`] | Maintaining a book from a diff stream, incl. snapshot resync |
//! | [`candle_builder`] | Building OHLCV **from trades** at multiple resolutions |
//! | [`bus`] | In-process pub/sub, one bus per symbol |
//! | [`health`] | Connection health counters + trade-id gap detection |
//! | [`backfill`] | Historical REST backfill (klines or aggregate trades) |
//!
//! ## Design notes
//!
//! * **Candles come from the trade stream**, not exchange klines, so the
//!   buy/sell volume split is consistent with delta/CVD downstream.
//! * **Nothing is silently dropped.** A trade-id gap or a book sequence gap is
//!   counted and logged; it is the caller's job to resync that window.
//! * **No broker.** Internal fan-out is `tokio::sync::broadcast`; adding Kafka
//!   or NATS is deferred until there's a measured cross-process need.
//!
//! See `docs/04-MARKET-DATA-ENGINE.md`.

#![deny(missing_docs)]

pub mod backfill;
pub mod bus;
pub mod candle_builder;
pub mod error;
pub mod exchanges;
pub mod health;
pub mod orderbook;

pub use backfill::{BackfillClient, BackfillSource};
pub use bus::{MarketBusRegistry, MarketEventBus};
pub use candle_builder::{CandleBuilder, MultiTimeframeCandleBuilder};
pub use error::MarketDataError;
pub use exchanges::binance::{BinanceCollector, BinanceConfig};
pub use exchanges::wire;
pub use exchanges::ExchangeCollector;
pub use health::{CollectorHealth, HealthStatus, TradeGapDetector};
pub use orderbook::{DepthDiff, DiffOutcome, OrderBook, OrderBookSynchronizer};
