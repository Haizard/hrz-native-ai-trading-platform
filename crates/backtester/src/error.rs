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

    /// The document declares a timeframe that the replay input has no series for.
    ///
    /// Reported separately from [`BacktestError::MissingData`] because the two
    /// have different fixes: missing data means "load more candles", while this
    /// means the caller forgot to supply a timeframe the document asked for --
    /// and silently replaying it would leave that context view permanently empty.
    #[error("no candles supplied for declared timeframe `{0}`")]
    MissingTimeframe(String),

    /// Strategy execution failed mid-replay.
    #[error("strategy execution failed: {0}")]
    Execution(#[from] strategy_runtime::RuntimeError),

    /// The report could not be rendered.
    #[error("failed to render report: {0}")]
    Report(String),
}
