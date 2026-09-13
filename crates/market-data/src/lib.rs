//! # `market-data`
//!
//! Ingests, normalizes and persists real-time and historical market data
//! (trades, order book, OHLCV) from exchanges, and republishes it internally.
//!
//! ## Phase 1 deliverables (`docs/04-MARKET-DATA-ENGINE.md`)
//!
//! * `ExchangeCollector` trait -- a second exchange must be a new
//!   implementation of the trait, never a rewrite of this crate.
//! * `BinanceCollector` for the trade stream, order-book diff stream and
//!   multi-resolution candle aggregation (1m/5m/15m/1h/4h/1d).
//! * Candles built **from the trade stream**, not from exchange klines, so the
//!   `buy_volume`/`sell_volume` split stays consistent with delta/CVD.
//! * Backfill CLI over the same normalization path as live data.
//! * Auto-reconnect with backoff, gap detection, health reporting.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::MarketDataError;
