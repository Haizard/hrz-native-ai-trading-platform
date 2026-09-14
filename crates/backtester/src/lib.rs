//! # `backtester`
//!
//! Deterministically replays historical market data through a
//! `StrategyDocument` (via `strategy-runtime`) and produces trustworthy
//! performance statistics. This is what makes an AI thesis credible -- when the
//! agent says "historical win rate: 68.5%", that number comes from here.
//!
//! ## Modules
//!
//! * [`replay`] -- chronological event-driven replay; the strategy may only
//!   ever see data up to and including the current candle close.
//! * [`simulator`] -- order/position/PnL simulation.
//! * [`report`] -- win rate, profit factor, Sharpe, max drawdown, average R.
//!
//! ## The two non-negotiables
//!
//! **No look-ahead bias.** Not enforced by care, but by construction: the only
//! component that decides visibility is [`replay`], and
//! [`strategy_runtime::MarketContext`] has no API that can reach a future bar.
//! The spec asks for a dedicated test that fails if future data leaks into a
//! decision; that test checks the invariant on *every* bar of a run rather than
//! spot-checking a few.
//!
//! **Shardability.** [`replay`] takes its input and returns its output, holding
//! no shared mutable state, so independent `(symbol, date-range)` shards run
//! concurrently. A test runs four shards at once and asserts each produces
//! byte-identical results to running alone.
//!
//! ## Reading the numbers
//!
//! Results are in **R multiples**, not currency: 1R is the risk a trade accepted
//! at entry. [`report::BacktestReport::assumptions`] states this -- and the fill
//! simplifications behind it -- in the report output itself, so the numbers are
//! never separated from the assumptions that produced them.
//!
//! ## Example
//!
//! ```no_run
//! use std::collections::BTreeMap;
//!
//! use analytics_core::types::{Candle, Timeframe};
//! use backtester::{replay::ReplayInput, replay::ReplayConfig, replay::run_backtest};
//! use strategy_runtime::{RuntimeConfig, StrategyEngine};
//!
//! # fn candles() -> Vec<Candle> { Vec::new() }
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let validated = strategy_dsl::parse_and_validate(
//!     r#"
//! name: "Example"
//! version: "1"
//! kind: strategy
//! market: BTCUSDT
//! timeframes:
//!   entry: 5m
//! entry:
//!   all_of:
//!     - timeframe: entry
//!       condition: delta > threshold(1)
//! risk:
//!   max_risk_pct: 1.0
//!   stop: {kind: below_recent_low, bars: 20}
//! invalidation:
//!   - timeframe: entry
//!     condition: close_below(vwap)
//! "#,
//! )?;
//! let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default())?;
//!
//! let mut timeframes = BTreeMap::new();
//! timeframes.insert("entry".to_string(), Timeframe::M5);
//! let mut series = BTreeMap::new();
//! series.insert("entry".to_string(), candles());
//! let input = ReplayInput { timeframes, candles: series };
//!
//! let report = run_backtest(&mut engine, &input, &ReplayConfig::default())?;
//! println!("{}", report.summary());
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod error;
pub mod replay;
pub mod report;

pub use error::BacktestError;
pub use replay::{replay, run_backtest, ReplayConfig, ReplayInput, ReplayOutput};
pub use report::{
    build_report, compute_metrics, worst_regime, BacktestReport, Metrics, ParameterSweep,
};

// The fill model now lives in `strategy-runtime` so that the replay and the
// Phase 6 paper trader cannot drift apart (`docs/03` forbids `trading-engine`
// from depending on this crate). Re-exported here so callers that know these
// as backtester types keep working.
pub use strategy_runtime::{
    FillAssumptions, OpenPosition, Simulator, SimulatorConfig, TradeRecord,
};

/// The types most callers need.
pub mod prelude {
    pub use crate::error::BacktestError;
    pub use crate::replay::{replay, run_backtest, ReplayConfig, ReplayInput, ReplayOutput};
    pub use crate::report::{BacktestReport, FillAssumptions, Metrics, TradeRecord};
    pub use strategy_runtime::{Simulator, SimulatorConfig};
}
