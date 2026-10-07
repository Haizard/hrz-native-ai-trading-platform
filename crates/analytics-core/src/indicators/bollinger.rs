//! Bollinger Bands.
//!
//! A moving average with bands at `mult` standard deviations on each side.
//! The bands answer "how stretched is price relative to its recent mean" --
//! squeezes (bands narrowing) precede expansions, and touches of the outer
//! band are continuation in a trend, exhaustion in a range.

use super::warmup;

/// One Bollinger reading: the middle band (SMA), and the two outer bands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BollingerPoint {
    /// The SMA over `period`.
    pub middle: f64,
    /// `middle + mult * σ`.
    pub upper: f64,
    /// `middle - mult * σ`.
    pub lower: f64,
    /// Bandwidth relative to the middle: `(upper - lower) / middle`. The
    /// squeeze metric -- a falling bandwidth over several bars is the squeeze.
    pub bandwidth: f64,
    /// Where the close sits inside the bands: 0 = lower, 1 = upper. Outside
    /// values are possible and meaningful (a close beyond the band).
    pub percent_b: f64,
}

/// Population standard deviation of `values` around `mean`.
///
/// Population (÷n), not sample (÷n−1): the window IS the whole population
/// being described -- there is no larger sample it estimates. This matches
/// TradingView's `ta.stdev` default.
fn population_stddev(values: &[f64], mean: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let variance =
        values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64;
    variance.sqrt()
}

/// Bollinger Bands over `closes`.
///
/// The first `period - 1` entries are `None`. `percent_b` uses the close at
/// the same index, so it is only defined where the bands are.
#[must_use]
pub fn bollinger(closes: &[f64], period: usize, mult: f64) -> Vec<Option<BollingerPoint>> {
    let mut out = warmup(closes.len());
    if period == 0 || closes.len() < period || !mult.is_finite() || mult < 0.0 {
        return out;
    }

    #[allow(clippy::cast_precision_loss)]
    for i in (period - 1)..closes.len() {
        let window = &closes[(i + 1 - period)..=i];
        #[allow(clippy::cast_precision_loss)]
        let middle = window.iter().sum::<f64>() / period as f64;
        let sd = population_stddev(window, middle);
        let upper = middle + mult * sd;
        let lower = middle - mult * sd;
        let width = upper - lower;
        let close = closes[i];
        out[i] = Some(BollingerPoint {
            middle,
            upper,
            lower,
            bandwidth: if middle == 0.0 { 0.0 } else { width / middle },
            percent_b: if width == 0.0 {
                0.5
            } else {
                (close - lower) / width
            },
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_input_has_zero_width_and_half_percent_b() {
        let values = vec![10.0; 30];
        for point in bollinger(&values, 20, 2.0).into_iter().flatten() {
            assert_eq!(point.upper, point.lower);
            assert_eq!(point.bandwidth, 0.0);
            assert_eq!(point.percent_b, 0.5);
        }
    }

    #[test]
    fn bands_are_symmetric_around_the_middle() {
        let values: Vec<f64> = (0..30).map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0).collect();
        for point in bollinger(&values, 20, 2.0).into_iter().flatten() {
            assert!(((point.upper - point.middle) - (point.middle - point.lower)).abs() < 1e-9);
        }
    }

    #[test]
    fn two_sigma_covers_the_known_window() {
        // 0..=19 has population σ ≈ 5.77; bands at ±2σ ≈ ±11.55 around 9.5.
        let values: Vec<f64> = (0..20).map(f64::from).collect();
        let point = bollinger(&values, 20, 2.0)[19].expect("defined at index 19");
        assert!((point.middle - 9.5).abs() < 1e-9);
        assert!((point.upper - point.middle - 2.0 * 5.7662812973).abs() < 1e-6);
    }

    #[test]
    fn a_close_above_the_band_scores_above_one() {
        let mut values = vec![10.0; 25];
        values.push(100.0);
        let point = bollinger(&values, 20, 2.0)[25].expect("defined at the spike");
        assert!(point.percent_b > 1.0, "percent_b={}", point.percent_b);
    }

    #[test]
    fn degenerate_inputs_are_all_none() {
        assert!(bollinger(&[1.0], 20, 2.0).iter().all(Option::is_none));
        assert!(bollinger(&[1.0; 30], 0, 2.0).iter().all(Option::is_none));
        assert!(bollinger(&[1.0; 30], 20, -1.0).iter().all(Option::is_none));
        assert!(bollinger(&[1.0; 30], 20, f64::NAN).iter().all(Option::is_none));
    }
}
