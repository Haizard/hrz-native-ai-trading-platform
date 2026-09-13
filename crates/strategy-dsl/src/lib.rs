//! # `strategy-dsl`
//!
//! **One** declarative representation of trading logic (principle #5),
//! producible by natural language via the AI agent, by the visual builder, or
//! by a developer -- and executable unchanged as a chart indicator, a backtest,
//! a paper bot or a live bot.
//!
//! ## Phase 3 deliverables (`docs/06-STRATEGY-DSL.md`)
//!
//! * `schema.rs` -- serde structs with `deny_unknown_fields` so hallucinated
//!   fields fail fast.
//! * `parser.rs` -- YAML/JSON to a typed `StrategyDocument`.
//! * `validator.rs` -- semantic checks (declared timeframes, known condition
//!   vocabulary, risk ceiling, non-empty invalidation).
//!
//! Validation is a hard gate: **nothing reaches the sandbox or the runtime
//! without passing it**, and a failure returns the specific field-level error
//! to whoever produced the document (including the AI agent's retry loop) --
//! it is never silently patched.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::DslError;
