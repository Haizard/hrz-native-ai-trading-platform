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
//! * [`tools`] -- registry over analytics-core/backtester with strict JSON
//!   schemas (`analyze_timeframe`, `get_volume_profile`, `detect_absorption`,
//!   `backtest_similar_setups`, ...).
//! * [`llm_client`] -- provider-agnostic `LlmClient` trait, plus the Bedrock
//!   Converse provider in [`providers`].
//! * [`skills`] -- contextual skill retrieval (never wholesale prompt stuffing).
//! * [`multi_timeframe`] -- the 1D -> 4H -> 1H -> 5M ladder.
//! * [`thesis`] -- the explainable `TradeThesis`.
//! * [`agent`] -- the orchestration loop, and NL -> validated Strategy DSL.
//!
//! In the thesis, the prose `narrative` is generated **from** the structured
//! numeric fields, never the reverse. The numbers are ground truth.

#![deny(missing_docs)]

pub mod agent;
pub mod error;
pub mod llm_client;
pub mod multi_timeframe;
pub mod providers;
pub mod sigv4;
pub mod skills;
pub mod thesis;
pub mod tools;

pub use agent::draft_strategy_spec;
pub use agent::{
    Agent, AgentAnswer, AgentConfig, AskRequest, GeneratedStrategy, StrategyRequest,
    DEFAULT_MAX_ATTEMPTS, DEFAULT_MAX_TURNS, DRAFT_STRATEGY,
};
pub use error::AgentError;
pub use llm_client::{
    ContentBlock, LlmClient, LlmRequest, LlmResponse, Message, Role, StopReason, ToolCall,
    ToolChoice, ToolResult, ToolSpec, Usage,
};
pub use multi_timeframe::{LadderView, TimeframeLadder};
pub use providers::bedrock::{BedrockClient, BedrockConfig};
pub use skills::{Skill, SkillLibrary, SkillQuery};
pub use strategy_dsl::StrategyDocument;
pub use thesis::{Bias, CheckStatus, ConditionCheck, PriceRange, ToolTrace};

pub use thesis::TradeThesis;
pub use tools::{BacktestRunner, BacktestSummary, MarketDataSource, ToolContext, ToolRegistry};
