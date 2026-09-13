//! # `ai-agent`
//!
//! Orchestrates LLM reasoning **over structured market data** -- never over raw
//! calculation (principle #2).
//!
//! ## The non-negotiable boundary
//!
//! The agent calls tools that return numbers; it never computes the numbers
//! itself. If a question needs a calculation that is not yet a tool, the answer
//! is to add it to `analytics-core` and expose a tool -- never to let the model
//! approximate it in prose.
//!
//! ## Phase 5 deliverables (`docs/09-AI-AGENT-SYSTEM.md`)
//!
//! * `tools.rs` -- registry over analytics-core/backtester with strict JSON
//!   schemas (`analyze_timeframe`, `get_volume_profile`, `detect_absorption`,
//!   `backtest_similar_setups`, ...).
//! * `llm_client.rs` -- provider-agnostic `LlmClient` trait.
//! * `skills.rs` -- contextual skill retrieval (never wholesale prompt stuffing).
//! * `multi_timeframe.rs` -- the 1D -> 4H -> 1H -> 5M ladder.
//! * `thesis.rs` -- the explainable `TradeThesis`.
//!
//! In the thesis, the prose `narrative` is generated **from** the structured
//! numeric fields, never the reverse. The numbers are ground truth.
//!
//! ## Status
//!
//! Phase 0 skeleton.

#![deny(missing_docs)]

pub mod error;

pub use error::AgentError;
