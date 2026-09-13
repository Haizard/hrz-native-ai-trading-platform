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
    #[error("no skill matches this request; ask the user to define one")]
    NoMatchingSkill,
}
