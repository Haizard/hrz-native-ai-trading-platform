//! Classic technical indicators.
//!
//! These exist for three reasons: parity checks against other platforms,
//! because they are useful building blocks inside user-authored Strategy DSL
//! conditions, and because an indicator library is expected of anything calling
//! itself a trading terminal.
//!
//! ## Convention: `Option<f64>`, not `NaN`
//!
//! Every function returns one entry per input, with `None` for the warm-up
//! period. `NaN` would silently propagate through the Strategy DSL and into
//! order sizing; `None` forces the caller to decide. See the NaN guard in
//! `docs/08-SANDBOX-WASM.md`'s adversarial test list.
//!
//! All smoothing that Wilder specified (RSI, ATR) uses **Wilder's** smoothing,
//! not a plain EMA -- they are not the same and mixing them silently gives
//! subtly wrong values.

pub mod atr;
pub mod ema;
pub mod rsi;
pub mod sma;

pub use atr::{atr, atr_percent, true_range};
pub use ema::ema;
pub use rsi::rsi;
pub use sma::sma;

/// Simple average of a slice, or `None` when empty.
#[must_use]
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

/// Fill a `Vec<Option<f64>>` with `None`.
pub(crate) fn warmup(len: usize) -> Vec<Option<f64>> {
    vec![None; len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_of_empty_is_none() {
        assert!(mean(&[]).is_none());
    }

    #[test]
    fn mean_of_values() {
        assert!((mean(&[1.0, 2.0, 3.0]).unwrap() - 2.0).abs() < 1e-9);
    }
}
