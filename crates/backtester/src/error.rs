//! Errors returned by `backtester`.

use thiserror::Error;

/// Errors while running or reporting a backtest.
#[derive(Debug, Error)]
pub enum BacktestError {
    /// The requested data window could not be loaded.
    #[error("no data for {symbol} between {from} and {to}")]
    MissingData {
        /// Symbol requested.
        symbol: String,
        /// Start of the requested window (unix nanos).
        from: i64,
        /// End of the requested window (unix nanos).
        to: i64,
    },

    /// Strategy execution failed mid-replay.
    #[error("strategy execution failed: {0}")]
    Execution(#[from] strategy_runtime::RuntimeError),

    /// The report could not be rendered.
    #[error("failed to render report: {0}")]
    Report(String),
}
