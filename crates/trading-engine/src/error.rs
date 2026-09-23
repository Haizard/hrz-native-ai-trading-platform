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

    /// The network failed while talking to the exchange.
    ///
    /// Distinct from [`ExecutionError::OrderRejected`] because the two demand
    /// opposite responses: a rejection tells us the order did *not* happen, so
    /// retrying is safe, while a transport failure tells us nothing at all --
    /// the order may be live. Collapsing them is how a retry becomes a
    /// duplicate position.
    #[error("exchange unreachable: {0}")]
    Transport(String),

    /// The exchange answered with an error body.
    #[error("exchange error {code}: {message}")]
    Exchange {
        /// The exchange's error code.
        code: i64,
        /// The exchange's message.
        message: String,
    },

    /// Credentials for the venue are missing or unusable.
    ///
    /// Deliberately carries no part of the key: this error is logged, and a
    /// message that echoed a credential back would put it in the audit trail
    /// that `docs/15` says must never contain one.
    #[error("credentials for {venue} are not configured ({hint})")]
    Credentials {
        /// Which venue.
        venue: String,
        /// What to set, without repeating any secret.
        hint: String,
    },

    /// A stored credential could not be sealed, opened or parsed.
    ///
    /// Carries no part of any key or secret: this error reaches a log line, and
    /// the audit trail `docs/15` requires must never contain a credential. The
    /// message names the *variable* or the *scope* at fault instead, which is
    /// what an operator can actually act on.
    #[error("credential vault: {0}")]
    Vault(String),

    /// Live trading was refused by the gate (`docs/15`).
    #[error("live trading refused: {0}")]
    NotAllowed(String),

    /// Reconciliation found the platform and the exchange disagree.
    #[error("reconciliation mismatch: {0}")]
    ReconciliationMismatch(String),

    /// Persistence of an order/trade/audit record failed.
    #[error("storage error: {0}")]
    Storage(#[from] db::DbError),
}
