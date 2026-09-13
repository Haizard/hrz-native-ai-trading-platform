//! Errors returned by `market-data`.

use thiserror::Error;

/// Anything that can go wrong while collecting or normalizing market data.
#[derive(Debug, Error)]
pub enum MarketDataError {
    /// Transport failure: WebSocket disconnect, or an HTTP request to the
    /// exchange REST API that failed or returned a non-2xx status.
    #[error("transport error: {0}")]
    Transport(String),

    /// The exchange sent a payload we could not interpret.
    #[error("failed to normalize exchange payload: {0}")]
    Normalization(String),

    /// A gap in trade ids / sequence numbers was detected.
    #[error("gap detected on {symbol}: expected next id {expected}, got {actual}")]
    Gap {
        /// Symbol the gap was detected on.
        symbol: String,
        /// The id we expected to see next.
        expected: u64,
        /// The id we actually received.
        actual: u64,
    },

    /// Persistence failure.
    #[error("storage error: {0}")]
    Storage(String),
}
