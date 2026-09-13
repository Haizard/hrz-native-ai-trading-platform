//! # `backtester`
//!
//! Deterministically replays historical market data through a
//! `StrategyDocument` (via `strategy-runtime`) and produces trustworthy
//! performance statistics. This is what makes an AI thesis credible -- when the
//! agent says "historical win rate: 68.5%", that number comes from here.
//!
//! ## Phase 3 deliverables (`docs/07-BACKTESTING-ENGINE.md`)
//!
//! * `replay.rs` -- chronological event-driven replay; the strategy may only
//!   ever see data up to and including the current candle close.
//! * `simulator.rs` -- order/position/PnL simulation.
//! * `report.rs` -- win rate, profit factor, Sharpe, max drawdown, average R.
//!
//! Two non-negotiables: **no look-ahead bias** (enforced by a dedicated test
//! that fails if a strategy can read the future) and **shardability** -- the
//! replay function is designed to run per (symbol, date-range) in parallel.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::BacktestError;
