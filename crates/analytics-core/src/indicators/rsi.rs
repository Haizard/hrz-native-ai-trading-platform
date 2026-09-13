//! Relative Strength Index, using Wilder's smoothing.

use super::warmup;

/// Wilder's RSI over `closes`.
///
/// The first `period` entries are `None`: producing a value at index `i`
/// requires `period` price changes, and index `i` only has `i` of them.
///
/// Smoothing is Wilder's (`avg = (avg * (period - 1) + current) / period`),
/// **not** an EMA with the same period. They differ, and using an EMA here is
/// a common source of "why doesn't my RSI match TradingView".
///
/// A run with no losses returns `100.0`, and one with no gains returns `0.0`,
/// rather than dividing by zero.
#[must_use]
pub fn rsi(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = warmup(closes.len());
    if period == 0 || closes.len() <= period {
        return out;
    }

    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;

    // Seed from the first `period` changes (changes start at index 1).
    for i in 1..=period {
        let change = closes[i] - closes[i - 1];
        if change >= 0.0 {
            avg_gain += change;
        } else {
            avg_loss -= change;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let divisor = period as f64;
    avg_gain /= divisor;
    avg_loss /= divisor;

    out[period] = Some(from_averages(avg_gain, avg_loss));

    #[allow(clippy::cast_precision_loss)]
    let p = period as f64;
    for i in (period + 1)..closes.len() {
        let change = closes[i] - closes[i - 1];
        let (gain, loss) = if change >= 0.0 {
            (change, 0.0)
        } else {
            (0.0, -change)
        };

        avg_gain = (avg_gain * (p - 1.0) + gain) / p;
        avg_loss = (avg_loss * (p - 1.0) + loss) / p;

        out[i] = Some(from_averages(avg_gain, avg_loss));
    }

    out
}

fn from_averages(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss.abs() < f64::EPSILON {
        return 100.0;
    }
    if avg_gain.abs() < f64::EPSILON {
        return 0.0;
    }
    let rs = avg_gain / avg_loss;
    100.0 - (100.0 / (1.0 + rs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_entries_are_none() {
        let closes: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        let result = rsi(&closes, 5);
        assert_eq!(result[4], None);
        assert!(result[5].is_some());
    }

    #[test]
    fn monotonic_rise_is_fully_overbought() {
        let closes: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        let result = rsi(&closes, 5);
        assert!((result[9].unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn monotonic_fall_is_fully_oversold() {
        let closes: Vec<f64> = (1..=10).rev().map(|i| i as f64).collect();
        let result = rsi(&closes, 5);
        assert!((result[9].unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn flat_prices_yield_100_by_convention() {
        // No losses at all: the guarded branch returns 100 rather than NaN.
        let closes = vec![50.0; 10];
        let result = rsi(&closes, 5);
        assert!((result[9].unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn known_reference_value() {
        // Hand-computed for a simple alternating series, period 2.
        // closes: 10, 12, 11, 13
        // changes: +2, -1, +2
        // seed (first 2 changes): avg_gain = (2+0)/2 = 1, avg_loss = (0+1)/2 = 0.5
        //   -> rs = 2, rsi = 100 - 100/3 = 66.666...
        let closes = vec![10.0, 12.0, 11.0, 13.0];
        let result = rsi(&closes, 2);
        assert!((result[2].unwrap() - 66.666_666_66).abs() < 1e-6);
    }

    #[test]
    fn output_is_always_within_bounds() {
        let closes = vec![
            44.0, 44.5, 43.8, 45.1, 46.0, 45.2, 44.4, 43.9, 44.8, 45.9, 46.5, 45.0,
        ];
        for value in rsi(&closes, 3).into_iter().flatten() {
            assert!((0.0..=100.0).contains(&value), "rsi out of range: {value}");
        }
    }

    #[test]
    fn insufficient_data_is_all_none() {
        assert_eq!(rsi(&[1.0, 2.0], 5), vec![None, None]);
        assert_eq!(rsi(&[1.0, 2.0], 0), vec![None, None]);
    }
}
