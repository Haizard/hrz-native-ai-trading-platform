//! Simple Moving Average.

use super::warmup;

/// Rolling simple moving average.
///
/// Returns one entry per input value; the first `period - 1` entries are `None`.
/// Uses a rolling sum, so the cost is O(n) rather than O(n * period).
///
/// A `period` of `0`, or a `period` longer than the input, yields all `None`.
#[must_use]
pub fn sma(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = warmup(values.len());
    if period == 0 || values.len() < period {
        return out;
    }

    let mut sum: f64 = values[..period].iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let divisor = period as f64;
    out[period - 1] = Some(sum / divisor);

    for i in period..values.len() {
        sum += values[i] - values[i - period];
        out[i] = Some(sum / divisor);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_matches_hand_computed_values() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let result = sma(&values, 3);
        assert_eq!(result[0], None);
        assert_eq!(result[1], None);
        // (1+2+3)/3 = 2
        assert!((result[2].unwrap() - 2.0).abs() < 1e-9);
        // (2+3+4)/3 = 3
        assert!((result[3].unwrap() - 3.0).abs() < 1e-9);
        // (3+4+5)/3 = 4
        assert!((result[4].unwrap() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn period_one_echoes_the_input() {
        let values = vec![5.0, 7.0, 9.0];
        assert_eq!(sma(&values, 1), vec![Some(5.0), Some(7.0), Some(9.0)]);
    }

    #[test]
    fn period_longer_than_input_is_all_none() {
        assert_eq!(sma(&[1.0, 2.0], 5), vec![None, None]);
    }

    #[test]
    fn zero_period_is_all_none() {
        assert_eq!(sma(&[1.0, 2.0], 0), vec![None, None]);
    }

    #[test]
    fn exact_period_gives_one_value() {
        let values = vec![2.0, 4.0, 6.0];
        let result = sma(&values, 3);
        assert_eq!(result[0], None);
        assert_eq!(result[1], None);
        assert!((result[2].unwrap() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn rolling_sum_does_not_drift_over_long_inputs() {
        let values: Vec<f64> = (0..10_000).map(|i| (i % 7) as f64).collect();
        let result = sma(&values, 5);
        // Compare against a naive window at the very end.
        let n = values.len();
        let expected: f64 = values[n - 5..].iter().sum::<f64>() / 5.0;
        assert!((result[n - 1].unwrap() - expected).abs() < 1e-9);
    }
}
