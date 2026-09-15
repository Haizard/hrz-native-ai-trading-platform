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

    /// A concept document's name is unusable.
    #[error("the concept name `{name}` cannot be used: {reason}")]
    BadConceptName {
        /// The name as supplied.
        name: String,
        /// What is wrong with it.
        reason: String,
    },

    /// A concept's pattern window is outside the supported range.
    #[error("a pattern window of {window} candles is out of range: {min} to {max}")]
    ConceptWindowOutOfRange {
        /// The window as supplied.
        window: usize,
        /// The smallest supported window.
        min: usize,
        /// The largest supported window.
        max: usize,
    },

    /// A concept referred to a candle it cannot read.
    #[error("`{selector}` cannot be used here: {reason}")]
    BadConceptSelector {
        /// The selector as written.
        selector: String,
        /// What is wrong with it.
        reason: String,
    },

    /// A comparison put two different kinds of quantity together.
    #[error("`{left}` and `{right}` are different kinds of quantity and cannot be compared")]
    MismatchedConceptComparison {
        /// The left operand.
        left: String,
        /// The right operand.
        right: String,
    },

    /// A concept's minimum band ratio is not usable.
    ///
    /// Carries the value as a string rather than an `f64`, so this enum keeps
    /// its `Eq` -- the same reason [`AnalyticsError::NonFiniteValue`] does.
    #[error("`min_band_ratio` must be a positive finite number, got {ratio}")]
    BadConceptRatio {
        /// The ratio as supplied.
        ratio: String,
    },
}
