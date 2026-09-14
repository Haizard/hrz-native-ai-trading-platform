//! Errors returned by the `db` crate.

use thiserror::Error;

/// Anything that can go wrong in the persistence layer.
#[derive(Debug, Error)]
pub enum DbError {
    /// A required environment variable was missing or empty.
    #[error("missing required environment variable: {0}")]
    MissingEnv(String),

    /// An environment variable was present but not usable.
    #[error("invalid database configuration: {0}")]
    InvalidConfig(String),

    /// The pool could not be created, or acquiring a connection failed.
    #[error("connection pool error: {0}")]
    Pool(#[from] sqlx::Error),

    /// A declared timeframe could not be loaded, or has no usable candles.
    #[error("candle data unavailable: {0}")]
    CandlesUnavailable(String),

    /// A migration failed to apply.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
