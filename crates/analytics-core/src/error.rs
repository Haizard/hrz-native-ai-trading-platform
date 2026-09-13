//! Errors returned by `analytics-core`.
//!
//! One public error enum per crate (`docs/03-PROJECT-STRUCTURE.md`).

use thiserror::Error;

/// Errors produced by analytics-core.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnalyticsError {
    /// An unrecognized timeframe string was supplied.
    #[error("unknown timeframe `{0}`")]
    InvalidTimeframe(String),

    /// An input slice was empty where at least one element is required.
    #[error("expected at least one input element, got none")]
    EmptyInput,

    /// A non-finite value (`NaN` or infinity) reached a calculation.
    #[error("non-finite value encountered in calculation: {0}")]
    NonFiniteValue(String),
}
