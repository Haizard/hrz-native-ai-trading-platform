//! # `strategy-runtime`
//!
//! Executes a validated [`StrategyDocument`](strategy_dsl) against any data
//! source -- live ticks, historical replay, or simulated. This is the single
//! execution path behind the chart overlay, the backtester, the paper trader
//! and the live bot (principle #5).
//!
//! ## The path through this crate
//!
//! ```text
//!   ValidatedStrategy  (strategy-dsl -- already parsed and checked)
//!          |
//!          v
//!   StrategyEngine::new            -- compiles conditions, resolves direction
//!          |
//!   MarketContext                  -- what the strategy may see right now
//!          |
//!          v
//!   Strategy::on_candle            -- one candle in, Option<Signal> out
//! ```
//!
//! ## Why the entry point is `ValidatedStrategy`
//!
//! [`StrategyEngine::new`] takes a `&ValidatedStrategy`, not a
//! `StrategyDocument`. The spec's rule is that nothing reaches the runtime
//! without passing validation; expressing that as a type means the rule is
//! enforced by the compiler instead of by everyone remembering it.
//!
//! ## Modules
//!
//! * [`engine`] -- the interpreter, plus the `Strategy` trait it implements.
//! * [`context`] -- `MarketContext`: a [`MarketState`](analytics_core::MarketState)
//!   per declared timeframe plus position state, and the runtime binding for
//!   the DSL's field vocabulary.
//! * [`signal`] -- the `Signal` a strategy emits.
//!
//! ## Example
//!
//! ```
//! use strategy_runtime::{RuntimeConfig, StrategyEngine};
//!
//! let yaml = r#"
//! name: "Simple breakout"
//! version: "1.0"
//! kind: strategy
//! market: BTCUSDT
//! timeframes:
//!   entry: 5m
//! entry:
//!   all_of:
//!     - timeframe: entry
//!       condition: close_above(vah)
//! risk:
//!   max_risk_pct: 1.0
//!   stop: below_swing_low
//! invalidation:
//!   - timeframe: entry
//!     condition: close_below(vwap)
//! "#;
//!
//! let validated = strategy_dsl::parse_and_validate(yaml).expect("valid");
//! let engine = StrategyEngine::new(&validated, RuntimeConfig::default())
//!     .expect("tradable");
//! assert_eq!(engine.decision_timeframe(), "entry");
//! ```

#![deny(missing_docs)]

pub mod context;
pub mod engine;
pub mod error;
pub mod rolling;
pub mod signal;
pub mod simulator;

pub use context::{
    divergence_name, swept_level_price, swept_side, trend_name, FieldValue, MarketContext,
    PositionView, TimeframeView,
};
pub use engine::{RuntimeConfig, SkipRecord, Strategy, StrategyEngine};
pub use error::RuntimeError;
pub use rolling::{RollingConfig, RollingLadder, RollingTimeframe};
pub use signal::{EnterSignal, ExitSignal, ExitTrigger, Signal, SignalAction};
pub use simulator::{FillAssumptions, OpenPosition, Simulator, SimulatorConfig, TradeRecord};

/// The types most callers need.
pub mod prelude {
    pub use crate::context::{FieldValue, MarketContext, PositionView, TimeframeView};
    pub use crate::engine::{RuntimeConfig, Strategy, StrategyEngine};
    pub use crate::error::RuntimeError;
    pub use crate::signal::{EnterSignal, ExitSignal, ExitTrigger, Signal, SignalAction};
    pub use crate::simulator::{
        FillAssumptions, OpenPosition, Simulator, SimulatorConfig, TradeRecord,
    };
}
