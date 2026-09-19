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
//! | [`history`] | Recent candles held **in RAM**, never persisted |
//! | [`backfill`] | Historical REST backfill (klines or aggregate trades) |
//! | [`tape`] | Recent trades and the newest book, held **in RAM** |
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
pub mod history;
pub mod orderbook;
pub mod scanner;
pub mod symbols;
pub mod tape;
pub mod window;

pub use backfill::{BackfillClient, BackfillSource};
pub use bus::{MarketBusRegistry, MarketEventBus};
pub use candle_builder::{
    CandleBuilder, MultiTimeframeCandleBuilder, LIVE_TIMEFRAMES, STANDARD_TIMEFRAMES,
};
pub use error::MarketDataError;
pub use exchanges::binance::{BinanceCollector, BinanceConfig};
pub use exchanges::wire;
pub use exchanges::ExchangeCollector;
pub use health::{
    spawn_health_publisher, CollectorHealth, HealthStatus, TradeGapDetector,
    HEALTH_PUBLISH_INTERVAL,
};
pub use history::{CandleHistory, HistoryRegistry, DEFAULT_HISTORY_BARS};
pub use orderbook::{DepthDiff, DiffOutcome, OrderBook, OrderBookSynchronizer};
pub use symbols::{Instrument, SymbolCheck, SymbolIndex, INDEX_TTL};
pub use tape::{BookCache, LiveRegistry, TradeTape, DEFAULT_TAPE_TRADES};
pub use window::{Window, WindowService, WindowSource, MAX_VENUE_BARS};
