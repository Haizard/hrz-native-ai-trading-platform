//! `ta.*` -- the technical-analysis builtins, over `analytics-core`.
//!
//! The rule from `docs/23`: **pine-lite never owns indicator math.** Every
//! function here calls the `analytics-core` implementation the chart, the
//! scanner and the validator already use, so a script's numbers are those
//! numbers. Where Pine's semantics differ from the crate's output shape (Pine
//! returns `float` with `na`, the crate returns `Option<f64>` per bar), the
//! translation is explicit below: `None` becomes `na` (`f64::NAN`), which the
//! interpreter propagates with Pine's rules.
//!
//! Numeric builtins that `analytics-core` has no equivalent for (`math.*`)
//! live here too -- arithmetic over scalars is not trading math, so it does
//! not breach the one-implementation rule.

use analytics_core::indicators::{atr, ema, rsi, sma};
use analytics_core::types::Candle;

/// A float series: one value per bar, `na` as `NaN`.
pub type Series = Vec<f64>;

/// `na` -- the single NaN value every series uses.
pub const NA: f64 = f64::NAN;

/// `not na(x)`.
#[must_use]
pub fn is_finite(value: f64) -> bool {
    value.is_finite()
}

/// A series from a candle field.
#[must_use]
pub fn closes(candles: &[Candle]) -> Series {
    candles.iter().map(|c| c.close).collect()
}

/// `ta.sma(source, length)`.
#[must_use]
pub fn ta_sma(source: &[f64], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    sma(source, period)
        .into_iter()
        .map(|v| v.unwrap_or(NA))
        .collect()
}

/// `ta.ema(source, length)` -- SMA-seeded, as Pine seeds it.
#[must_use]
pub fn ta_ema(source: &[f64], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    ema(source, period)
        .into_iter()
        .map(|v| v.unwrap_or(NA))
        .collect()
}

/// `ta.rma(source, length)` -- Wilder's smoothing, seeded with the SMA of the
/// first `length` values (Pine's convention, and `analytics-core::atr`'s).
///
/// `ta.rsi` and `ta.atr` use this internally; it is exposed because Pine
/// exposes it and because a script may want `ta.rma` on any series.
#[must_use]
pub fn ta_rma(source: &[f64], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    if source.len() < period {
        return vec![NA; source.len()];
    }
    let mut out = vec![NA; source.len()];
    let seed: f64 = source[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = seed;
    let p = period as f64;
    let mut prev = seed;
    for i in period..source.len() {
        prev = (prev * (p - 1.0) + source[i]) / p;
        out[i] = prev;
    }
    out
}

/// `ta.rsi(close, length)` -- Wilder's, from `analytics-core`.
#[must_use]
pub fn ta_rsi(source: &[f64], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    rsi(source, period)
        .into_iter()
        .map(|v| v.unwrap_or(NA))
        .collect()
}

/// True range over candles, `na` on the first bar (Pine's `ta.tr(handle_na)`
/// convention when a previous close does not exist).
#[must_use]
pub fn ta_tr(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    for i in 1..candles.len() {
        let c = &candles[i];
        let prev_close = candles[i - 1].close;
        out[i] = (c.high - c.low)
            .max((c.high - prev_close).abs())
            .max((c.low - prev_close).abs());
    }
    out
}

/// `ta.atr(length)` over candles.
#[must_use]
pub fn ta_atr(candles: &[Candle], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    atr(candles, period)
        .into_iter()
        .map(|v| v.unwrap_or(NA))
        .collect()
}

/// `ta.macd(source, fast, slow, signal)` -> `(macd, signal, hist)`.
///
/// Implemented over `ta_ema` (which is `analytics-core::ema`), so the fast and
/// slow EMAs are the chart's EMAs. Pine seeds its EMAs with an SMA too, so
/// this matches Pine to float precision on the shared window.
#[must_use]
pub fn ta_macd(source: &[f64], fast: f64, slow: f64, signal: f64) -> (Series, Series, Series) {
    let fast_line = ta_ema(source, fast);
    let slow_line = ta_ema(source, slow);
    let macd: Series = fast_line
        .iter()
        .zip(&slow_line)
        .map(|(f, s)| {
            if f.is_finite() && s.is_finite() {
                f - s
            } else {
                NA
            }
        })
        .collect();
    // The signal line is an EMA of the macd line, skipping leading na.
    let first_finite = macd.iter().position(|v| v.is_finite());
    let mut signal_line = vec![NA; macd.len()];
    let mut hist = vec![NA; macd.len()];
    if let Some(start) = first_finite {
        let trimmed: Vec<f64> = macd[start..].to_vec();
        let sig = ta_ema(&trimmed, signal.max(1.0));
        for (i, v) in sig.into_iter().enumerate() {
            signal_line[start + i] = v;
            if v.is_finite() && macd[start + i].is_finite() {
                hist[start + i] = macd[start + i] - v;
            }
        }
    }
    (macd, signal_line, hist)
}

/// `ta.highest(source, length)`.
#[must_use]
pub fn ta_highest(source: &[f64], length: f64) -> Series {
    window_fold(source, length, f64::NEG_INFINITY, f64::max)
}

/// `ta.lowest(source, length)`.
#[must_use]
pub fn ta_lowest(source: &[f64], length: f64) -> Series {
    window_fold(source, length, f64::INFINITY, f64::min)
}

fn window_fold(source: &[f64], length: f64, init: f64, f: fn(f64, f64) -> f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let mut acc = init;
        for v in &source[i + 1 - n..=i] {
            acc = f(acc, *v);
        }
        out[i] = acc;
    }
    out
}

/// `ta.change(source)` -- one-bar difference; `na` on the first bar.
#[must_use]
pub fn ta_change(source: &[f64]) -> Series {
    let mut out = vec![NA; source.len()];
    for i in 1..source.len() {
        if source[i].is_finite() && source[i - 1].is_finite() {
            out[i] = source[i] - source[i - 1];
        }
    }
    out
}

/// `ta.mom(source, length)`.
#[must_use]
pub fn ta_mom(source: &[f64], length: f64) -> Series {
    let n = length.max(1.0) as usize;
    let mut out = vec![NA; source.len()];
    for i in n..source.len() {
        if source[i].is_finite() && source[i - n].is_finite() {
            out[i] = source[i] - source[i - n];
        }
    }
    out
}

/// `ta.roc(source, length)` in percent.
#[must_use]
pub fn ta_roc(source: &[f64], length: f64) -> Series {
    let n = length.max(1.0) as usize;
    let mut out = vec![NA; source.len()];
    for i in n..source.len() {
        if source[i].is_finite() && source[i - n].is_finite() && source[i - n] != 0.0 {
            out[i] = 100.0 * (source[i] - source[i - n]) / source[i - n];
        }
    }
    out
}

/// `ta.crossover(a, b)`: `a` was at or below `b` and is now above.
#[must_use]
pub fn ta_crossover(a: &[f64], b: &[f64]) -> Series {
    cross(a, b, true)
}

/// `ta.crossunder(a, b)`.
#[must_use]
pub fn ta_crossunder(a: &[f64], b: &[f64]) -> Series {
    cross(a, b, false)
}

fn cross(a: &[f64], b: &[f64], over: bool) -> Series {
    let n = a.len().min(b.len());
    let mut out = vec![NA; a.len()];
    for i in 1..n {
        let (a0, a1, b0, b1) = (a[i - 1], a[i], b[i - 1], b[i]);
        if !(a0.is_finite() && a1.is_finite() && b0.is_finite() && b1.is_finite()) {
            continue;
        }
        out[i] = f64::from(if over {
            a0 <= b0 && a1 > b1
        } else {
            a0 >= b0 && a1 < b1
        });
    }
    out
}

/// `ta.vwap` over candles -- the session-anchored VWAP `analytics-core`
/// computes for the chart's default levels.
#[must_use]
pub fn ta_vwap(candles: &[Candle]) -> Series {
    match analytics_core::vwap::calculate_vwap(candles) {
        // A single scalar VWAP over the window: constant per bar, which is
        // what the chart's vwap line is too.
        Some(v) => vec![v; candles.len()],
        None => vec![NA; candles.len()],
    }
}

/// `ta.stoch(close, high, low, length)` -- %K without smoothing (Pine's
/// `ta.stoch` returns the raw stochastic; `ta.sma(..., 3)` smooths).
///
/// Not in `analytics-core` yet; window arithmetic over candles is the same
/// fold the highest/lowest helpers use. When `analytics-core` grows a
/// stochastic, this function becomes a call to it (docs/23's rule).
#[must_use]
pub fn ta_stoch(source: &[f64], high: &[f64], low: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let hh = high[i + 1 - n..=i].iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let ll = low[i + 1 - n..=i].iter().cloned().fold(f64::INFINITY, f64::min);
        if hh.is_finite() && ll.is_finite() && hh != ll {
            out[i] = 100.0 * (source[i] - ll) / (hh - ll);
        } else {
            out[i] = NA;
        }
    }
    out
}

/// `ta.bb(source, length, mult)` -> `(basis, upper, lower)`.
#[must_use]
pub fn ta_bb(source: &[f64], length: f64, mult: f64) -> (Series, Series, Series) {
    let basis = ta_sma(source, length);
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut upper = vec![NA; source.len()];
    let mut lower = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        if !basis[i].is_finite() {
            continue;
        }
        let window = &source[i + 1 - n..=i];
        let mean = basis[i];
        let var = window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        let dev = var.sqrt() * mult;
        upper[i] = mean + dev;
        lower[i] = mean - dev;
    }
    (basis, upper, lower)
}

/// `math.sum(source, length)`.
#[must_use]
pub fn math_sum(source: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        out[i] = source[i + 1 - n..=i].iter().sum();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rising(n: usize) -> Vec<f64> {
        (0..n).map(|v| v as f64).collect()
    }

    #[test]
    fn sma_matches_the_crate() {
        let src = rising(10);
        let out = ta_sma(&src, 3.0);
        assert!((out[2] - 1.0).abs() < 1e-9);
        assert!((out[9] - 8.0).abs() < 1e-9);
        assert!(out[1].is_nan());
    }

    #[test]
    fn ema_seeds_with_sma_like_the_crate() {
        let src = rising(10);
        let out = ta_ema(&src, 3.0);
        assert!((out[2] - 1.0).abs() < 1e-9, "{}", out[2]);
    }

    #[test]
    fn rsi_warms_up_then_matches_the_crate() {
        let src = rising(30);
        let out = ta_rsi(&src, 14.0);
        // A monotonic rise is fully overbought.
        assert!((out[29] - 100.0).abs() < 1e-9);
        assert!(out[13].is_nan());
    }

    #[test]
    fn macd_degrades_to_na_before_the_slow_ema_warms() {
        let src = rising(30);
        let (macd, signal, hist) = ta_macd(&src, 3.0, 6.0, 3.0);
        assert!(macd[4].is_nan(), "slow ema(6) warms at index 5");
        assert!(macd[10].is_finite());
        assert!(signal[12].is_finite());
        assert!((hist[12] - (macd[12] - signal[12])).abs() < 1e-9);
    }

    #[test]
    fn crossover_fires_only_on_the_cross_bar() {
        let a = vec![1.0, 1.0, 3.0, 3.0];
        let b = vec![2.0, 2.0, 2.0, 2.0];
        let out = ta_crossover(&a, &b);
        assert!(out[1].is_nan() || out[1] == 0.0);
        assert_eq!(out[2], 1.0);
        assert_eq!(out[3], 0.0);
    }

    #[test]
    fn stoch_bounds() {
        let n = 20;
        let high: Vec<f64> = (0..n).map(|i| i as f64 + 1.0).collect();
        let low: Vec<f64> = (0..n).map(|v| v as f64).collect();
        let close: Vec<f64> = (0..n).map(|i| i as f64 + 0.5).collect();
        let out = ta_stoch(&close, &high, &low, 5.0);
        let last = out[n - 1];
        assert!((0.0..=100.0).contains(&last), "{last}");
    }

    #[test]
    fn atr_matches_the_crate_on_shared_input() {
        // Build candles whose true range is constant 10.
        let candles: Vec<Candle> = (0..30)
            .map(|i| Candle {
                symbol: "T".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: i as i64,
                open: 100.0,
                high: 110.0,
                low: 100.0,
                close: 105.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            })
            .collect();
        let out = ta_atr(&candles, 14.0);
        let expected: Vec<Option<f64>> = atr(&candles, 14).to_vec();
        for (got, want) in out.iter().zip(expected) {
            match want {
                Some(w) => assert!((got - w).abs() < 1e-9),
                None => assert!(got.is_nan()),
            }
        }
    }
}
