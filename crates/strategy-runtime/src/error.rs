//! Errors returned by `strategy-runtime`.

use thiserror::Error;

/// Errors while executing a strategy document.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The strategy referenced a timeframe it did not declare.
    #[error("strategy referenced undeclared timeframe `{0}`")]
    UndeclaredTimeframe(String),

    /// A condition could not be evaluated against the current context.
    #[error("failed to evaluate condition `{0}`: {1}")]
    ConditionEvaluation(String, String),

    /// The sandbox terminated execution (fuel/timeout/memory).
    #[error("execution terminated: {0}")]
    Terminated(String),

    /// The strategy emitted a signal that failed the risk gate.
    #[error("signal rejected by risk engine: {0}")]
    RiskRejected(String),
}
