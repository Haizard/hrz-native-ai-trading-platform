//! # `strategy-runtime`
//!
//! Executes a validated [`StrategyDocument`](strategy_dsl) against any data
//! source -- live ticks, historical replay, or simulated. This is the single
//! execution path behind the chart overlay, the backtester, the paper trader
//! and the live bot (principle #5).
//!
//! ## Phase 3 deliverables (`docs/06-STRATEGY-DSL.md`)
//!
//! ```text
//! engine.rs   -- the interpreter driving on_candle
//! context.rs  -- MarketContext: MarketState per declared timeframe + position state
//! signal.rs   -- Signal emitted by a strategy
//! ```
//!
//! The document is **interpreted**, not code-generated, in Phase 3; a compiled
//! path is a later optimization gated on profiling.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::RuntimeError;
