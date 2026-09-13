//! Errors returned by `market-data`.

use thiserror::Error;

/// Anything that can go wrong while collecting or normalizing market data.
#[derive(Debug, Error)]
pub enum MarketDataError {
    /// WebSocket transport failure.
    #[error("websocket error: {0}")]
    Websocket(String),

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
