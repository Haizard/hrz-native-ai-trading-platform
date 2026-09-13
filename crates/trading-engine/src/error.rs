//! Errors returned by `trading-engine`.

use thiserror::Error;

/// Errors in order execution and risk handling.
#[derive(Debug, Error)]
pub enum ExecutionError {
    /// The exchange rejected or failed to acknowledge the order.
    #[error("order rejected: {0}")]
    OrderRejected(String),

    /// A configured risk limit blocked the action.
    #[error("risk limit breached: {limit} (value: {value})")]
    RiskLimitBreached {
        /// Which limit was hit.
        limit: String,
        /// The observed value that breached it.
        value: String,
    },

    /// The kill-switch is engaged; no new signals are being executed.
    #[error("kill-switch active: {0}")]
    KillSwitchActive(String),

    /// Reconciliation found the platform and the exchange disagree.
    #[error("reconciliation mismatch: {0}")]
    ReconciliationMismatch(String),

    /// Persistence of an order/trade/audit record failed.
    #[error("storage error: {0}")]
    Storage(#[from] db::DbError),
}
