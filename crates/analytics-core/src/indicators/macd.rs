//! Moving Average Convergence Divergence.
//!
//! The MACD is two EMAs and one more: the fast/slow difference is the MACD
//! line, an EMA of that line is the signal, and their difference is the
//! histogram. It exists here for the same reason as the rest of the classic
//! set -- parity with other platforms, DSL building blocks, and an agent tool
//! that reports momentum without the model computing anything.

use super::ema::ema;
use super::warmup;

/// One MACD reading: the line, its signal, and the histogram (line − signal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MacdPoint {
    /// Fast EMA minus slow EMA.
    pub line: f64,
    /// EMA of the MACD line.
    pub signal: f64,
    /// `line − signal`.
    pub histogram: f64,
}

/// MACD over `closes` with the classic (12, 26, 9) default when the caller
/// passes the conventional periods.
///
/// The first `slow + signal - 2` entries are `None`: the MACD line needs a
/// slow-EMA warm-up, and the signal line needs a signal-length warm-up of
/// *that*. Reporting a signal before both seeds exist would mix two warm-up
/// regimes into one number.
#[must_use]
pub fn macd(closes: &[f64], fast: usize, slow: usize, signal: usize) -> Vec<Option<MacdPoint>> {
    let mut out = warmup(closes.len());
    if fast == 0 || slow == 0 || signal == 0 || fast >= slow || closes.len() < slow + signal - 1 {
        return out;
    }

    let fast_ema = ema(closes, fast);
    let slow_ema = ema(closes, slow);

    // The MACD line exists from the slow seed onward.
    let mut line: Vec<Option<f64>> = warmup(closes.len());
    for i in 0..closes.len() {
        if let (Some(f), Some(s)) = (fast_ema[i], slow_ema[i]) {
            line[i] = Some(f - s);
        }
    }

    // Signal: EMA of the line over its defined tail. `ema` takes a dense slice,
    // so the line's defined prefix is extracted, smoothed, and written back at
    // the same absolute indices.
    let defined: Vec<f64> = line.iter().skip(slow - 1).flatten().copied().collect();
    let signal_ema = ema(&defined, signal);
    for (offset, sig) in signal_ema.iter().enumerate() {
        if let Some(sig) = sig {
            let i = slow - 1 + offset;
            if let Some(l) = line[i] {
                out[i] = Some(MacdPoint {
                    line: l,
                    signal: *sig,
                    histogram: l - sig,
                });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_input_is_zero_everywhere() {
        let values = vec![10.0; 60];
        for point in macd(&values, 12, 26, 9).into_iter().flatten() {
            assert!(point.line.abs() < 1e-9);
            assert!(point.signal.abs() < 1e-9);
            assert!(point.histogram.abs() < 1e-9);
        }
    }

    #[test]
    fn warmup_covers_both_seeds() {
        // slow + signal - 2 = 26 + 9 - 2 = 33 entries are None.
        let values = vec![10.0; 40];
        let result = macd(&values, 12, 26, 9);
        assert!(result[..33].iter().all(Option::is_none));
        assert!(result[33].is_some());
    }

    #[test]
    fn a_step_up_turns_the_line_positive() {
        let mut values = vec![10.0; 40];
        values.extend(vec![20.0; 20]);
        let result = macd(&values, 12, 26, 9);
        let last = result.last().and_then(|p| *p).expect("defined at the end");
        assert!(last.line > 0.0, "line={}", last.line);
    }

    #[test]
    fn histogram_is_line_minus_signal() {
        let values: Vec<f64> = (0..60).map(|i| 100.0 + i as f64 * 0.5).collect();
        for point in macd(&values, 12, 26, 9).into_iter().flatten() {
            assert!((point.histogram - (point.line - point.signal)).abs() < 1e-9);
        }
    }

    #[test]
    fn degenerate_inputs_are_all_none() {
        assert!(macd(&[1.0], 12, 26, 9).iter().all(Option::is_none));
        assert!(macd(&[1.0; 60], 26, 12, 9).iter().all(Option::is_none));
        assert!(macd(&[1.0; 60], 12, 26, 0).iter().all(Option::is_none));
        // One short of the full warm-up.
        assert!(macd(&[1.0; 33], 12, 26, 9).iter().all(Option::is_none));
    }
}
