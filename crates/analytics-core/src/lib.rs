//! # `analytics-core`
//!
//! The deterministic heart of the platform (principle #1 in
//! `00-VISION-AND-PRINCIPLES.md`).
//!
//! Every trading calculation the platform performs lives here **once** and is
//! reused by the backend, the WASM frontend, the backtester and the sandbox.
//! That is what guarantees:
//!
//! > what you see on the chart == what the backtester tested == what the bot executed
//!
//! ## Hard constraints
//!
//! * No `async`, no network, no filesystem, no database access.
//! * Every function is pure: same input => same output.
//! * Must compile unchanged for `wasm32-unknown-unknown`.
//!
//! ## Status
//!
//! Phase 0 skeleton. The types module is populated because every other crate
//! depends on these definitions; the calculation modules land in Phase 2
//! (`docs/05-ANALYTICS-ENGINE.md`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod error;
pub mod types;

pub use error::AnalyticsError;
pub use types::{Candle, FootprintCell, OrderBookLevel, OrderBookSnapshot, Side, Timeframe, Trade};
