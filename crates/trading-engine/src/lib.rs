//! # `trading-engine`
//!
//! Runs an approved `StrategyDocument` continuously against live market data --
//! first simulated (Phase 6), later with real orders (Phase 8) -- always through
//! the same `strategy-runtime`/sandbox path the backtester uses.
//!
//! ## Phases
//!
//! * Phase 6 -- paper trading: subscribe to live `MarketState`, feed closed
//!   candles into the sandboxed strategy, simulate fills, persist every
//!   decision (including "no signal") for audit.
//! * Phase 8 -- live trading, gated behind a paper track record, explicit
//!   per-venue opt-in and active risk limits.
//!
//! ## Risk is always on
//!
//! Per-trade risk cap clamped to the platform ceiling, per-account daily/weekly
//! loss limits, max concurrent positions, and a kill-switch that must work even
//! when the agent or market-data engine is degraded
//! (`docs/15-RISK-COMPLIANCE.md`).
//!
//! Credentials live here, never in the sandbox: order placement happens outside
//! the sandbox, driven only by the sandbox's `Signal` output.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::ExecutionError;
