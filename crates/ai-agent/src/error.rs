//! Errors returned by `ai-agent`.

use thiserror::Error;

/// Errors in the agent orchestration layer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The LLM provider call failed or returned an unusable payload.
    #[error("llm provider error: {0}")]
    Llm(String),

    /// The model asked for a tool that is not registered.
    #[error("unknown tool `{0}`")]
    UnknownTool(String),

    /// The model supplied arguments that do not match the tool's JSON schema.
    #[error("invalid arguments for tool `{tool}`: {reason}")]
    InvalidToolArgs {
        /// Tool the model attempted to call.
        tool: String,
        /// Why the arguments were rejected.
        reason: String,
    },

    /// The model produced a strategy document that failed validation.
    #[error("model produced an invalid strategy document: {0}")]
    InvalidStrategyDocument(#[from] strategy_dsl::DslError),

    /// No skill matched the request.
    ///
    /// Deliberately an error rather than a fallback: the agent must not invent
    /// methodology the user never defined (`docs/10-SKILLS-SYSTEM.md`).
    ///
    /// Carries the requested id so a pinned-but-missing skill is
    /// distinguishable from an empty library -- "you asked for X" and "there is
    /// nothing here at all" call for different responses.
    #[error("no skill matches `{0}`; ask the user to define one")]
    NoMatchingSkill(String),

    /// A tool ran and failed. The tool name is kept so the trace shows which
    /// step of the reasoning broke.
    #[error("tool `{tool}` failed: {reason}")]
    ToolFailed {
        /// The tool that failed.
        tool: String,
        /// Why it failed.
        reason: String,
    },

    /// Market data could not be read.
    #[error("market data unavailable for {symbol} {timeframe}: {reason}")]
    DataUnavailable {
        /// Symbol that was requested.
        symbol: String,
        /// Timeframe that was requested, as its exchange-style string.
        timeframe: String,
        /// Why the read failed.
        reason: String,
    },

    /// No data at all in the requested window, so there is nothing to reason
    /// about. Distinct from [`AgentError::DataUnavailable`]: that is a
    /// failure, this is an empty result, and the caller may want to widen the
    /// window rather than give up.
    #[error("no {timeframe} candles for {symbol} in the requested window")]
    NoData {
        /// Symbol that was requested.
        symbol: String,
        /// Timeframe that was requested.
        timeframe: String,
    },

    /// A skill document could not be parsed.
    #[error("invalid skill document: {0}")]
    InvalidSkill(String),

    /// The provider credentials or endpoint are missing/misconfigured.
    #[error("llm provider is not configured: {0}")]
    NotConfigured(String),

    /// The model looped without producing a thesis.
    ///
    /// Not the same as an LLM failure: the conversation was healthy, the model
    /// just never called `submit_thesis`. Surfaced so the caller can retry
    /// with a narrower question instead of seeing a generic error.
    #[error("the agent used {turns} turns without producing a thesis")]
    NoThesis {
        /// Number of model turns consumed before giving up.
        turns: usize,
    },

    /// The model returned a thesis whose numbers cannot be true of the data
    /// that was actually observed.
    ///
    /// This is the guardrail behind `docs/09`'s "the numbers are ground
    /// truth": a thesis is only accepted when its levels are reconcilable
    /// with the tool results that preceded it.
    #[error("thesis failed grounding check: {0}")]
    Ungrounded(String),

    /// JSON serialization/deserialization failed somewhere in the pipeline.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
