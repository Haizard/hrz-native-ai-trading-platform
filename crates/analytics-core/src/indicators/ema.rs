//! Exponential Moving Average.

use super::{mean, warmup};

/// Exponential moving average, seeded with the SMA of the first `period`
/// values.
///
/// Seeding matters. Starting the recursion from the very first value (instead
/// of an SMA seed) makes the early values depend heavily on an arbitrary
/// starting point, which shows up as a visible "hook" at the left edge of a
/// chart and as disagreements with other platforms. The SMA seed is what
/// TradingView and most charting libraries use.
///
/// The first `period - 1` entries are `None`.
#[must_use]
pub fn ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = warmup(values.len());
    if period == 0 || values.len() < period {
        return out;
    }

    let Some(seed) = mean(&values[..period]) else {
        return out;
    };

    out[period - 1] = Some(seed);

    #[allow(clippy::cast_precision_loss)]
    let k = 2.0 / (period as f64 + 1.0);
    let mut prev = seed;

    for i in period..values.len() {
        prev = values[i].mul_add(k, prev * (1.0 - k));
        out[i] = Some(prev);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_value_is_the_sma_seed() {
        let values = vec![1.0, 2.0, 3.0, 4.0];
        let result = ema(&values, 3);
        assert_eq!(result[0], None);
        assert_eq!(result[1], None);
        // seed = (1+2+3)/3 = 2
        assert!((result[2].unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn recursion_uses_the_standard_multiplier() {
        let values = vec![1.0, 2.0, 3.0, 4.0];
        let result = ema(&values, 3);
        // k = 2/4 = 0.5; next = 4*0.5 + 2*0.5 = 3
        assert!((result[3].unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn flat_input_stays_flat() {
        let values = vec![7.0; 10];
        for v in ema(&values, 4).into_iter().flatten() {
            assert!((v - 7.0).abs() < 1e-9);
        }
    }

    #[test]
    fn period_one_tracks_the_input_exactly() {
        let values = vec![5.0, 7.0, 9.0];
        let result = ema(&values, 1);
        assert!((result[0].unwrap() - 5.0).abs() < 1e-9);
        // k = 1.0, so each step fully adopts the new value.
        assert!((result[1].unwrap() - 7.0).abs() < 1e-9);
        assert!((result[2].unwrap() - 9.0).abs() < 1e-9);
    }

    #[test]
    fn insufficient_data_is_all_none() {
        assert_eq!(ema(&[1.0], 3), vec![None]);
        assert_eq!(ema(&[], 3), vec![]);
        assert_eq!(ema(&[1.0, 2.0], 0), vec![None, None]);
    }

    #[test]
    fn reacts_faster_than_the_sma() {
        // At the *first* bar of a new regime the EMA has already moved further
        // than the SMA, because the SMA is still averaging the old, lower
        // values. (By the end of the step the SMA window is entirely in the new
        // regime and the SMA is the one that has fully caught up.)
        let mut values = vec![10.0; 20];
        values.extend(vec![20.0; 10]);

        let e = ema(&values, 10);
        let s = super::super::sma::sma(&values, 10);

        // Index 20 is the first bar after the step.
        assert!(
            e[20].unwrap() > s[20].unwrap(),
            "ema={:?} should lead sma={:?}",
            e[20],
            s[20]
        );

        // A full window later the SMA has converged exactly, and the EMA --
        // still carrying the old regime -- sits below it.
        let last = values.len() - 1;
        assert!((s[last].unwrap() - 20.0).abs() < 1e-9);
        assert!(e[last].unwrap() < s[last].unwrap());
    }
}
