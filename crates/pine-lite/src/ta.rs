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

/// `ta.wma(source, length)` -- linearly weighted: the newest bar carries the
/// greatest weight, `length` down to 1. Pine's definition, and the reason a
/// WMA turns before an SMA of the same length.
#[must_use]
pub fn ta_wma(source: &[f64], length: f64) -> Series {
    let period = length.max(1.0) as usize;
    let denom = (period * (period + 1)) as f64 / 2.0;
    let mut out = vec![NA; source.len()];
    if period == 0 || source.len() < period {
        return out;
    }
    for i in (period - 1)..source.len() {
        let window = &source[i + 1 - period..=i];
        // Any na in the window poisons the average, exactly as Pine's does.
        if window.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let sum: f64 = window
            .iter()
            .enumerate()
            .map(|(k, v)| v * (k + 1) as f64)
            .sum();
        out[i] = sum / denom;
    }
    out
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


// ---------- Tier 1 packs: oscillators, since/when, event helpers ----------

/// `ta.stdev(source, length)` -- population standard deviation (Pine's
/// default `biased = true`).
#[must_use]
pub fn ta_stdev(source: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let w = &source[i + 1 - n..=i];
        if w.iter().all(|v| v.is_finite()) {
            let mean = w.iter().sum::<f64>() / n as f64;
            let var = w.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
            out[i] = var.sqrt();
        }
    }
    out
}

/// `ta.variance(source, length)` -- population variance.
#[must_use]
pub fn ta_variance(source: &[f64], length: f64) -> Series {
    let sd = ta_stdev(source, length);
    sd.iter().map(|v| if v.is_finite() { v * v } else { NA }).collect()
}

/// `ta.hma(source, length)` -- Hull: wma(2*wma(n/2) - wma(n), sqrt(n)).
#[must_use]
pub fn ta_hma(source: &[f64], length: f64) -> Series {
    let n = length.max(1.0);
    let half = (n / 2.0).max(1.0);
    let root = n.sqrt().max(1.0);
    let w1 = ta_wma(source, half);
    let w2 = ta_wma(source, n);
    let diff: Series = w1
        .iter()
        .zip(&w2)
        .map(|(a, b)| if a.is_finite() && b.is_finite() { 2.0 * a - b } else { NA })
        .collect();
    // The diff leads with na until both wmas warm up; trim before the last
    // wma so it seeds on the first real value (the macd pattern).
    let start = diff.iter().position(|v| v.is_finite());
    let mut out = vec![NA; diff.len()];
    if let Some(s) = start {
        let tail = ta_wma(&diff[s..], root);
        for (i, v) in tail.into_iter().enumerate() {
            out[s + i] = v;
        }
    }
    out
}

/// `ta.cci(length)` -- commodity channel index over the typical price.
#[must_use]
pub fn ta_cci(candles: &[Candle], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(candles.len().max(1));
    let tp: Vec<f64> = candles.iter().map(|c| (c.high + c.low + c.close) / 3.0).collect();
    let ma = ta_sma(&tp, length);
    let mut out = vec![NA; candles.len()];
    for i in (n - 1)..candles.len() {
        if ma[i].is_finite() {
            let dev = tp[i + 1 - n..=i].iter().map(|v| (v - ma[i]).abs()).sum::<f64>() / n as f64;
            out[i] = if dev > 0.0 { (tp[i] - ma[i]) / (0.015 * dev) } else { 0.0 };
        }
    }
    out
}

/// `ta.mfi(length)` -- money flow index (volume-weighted rsi shape).
#[must_use]
pub fn ta_mfi(candles: &[Candle], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(candles.len().max(1));
    let mut out = vec![NA; candles.len()];
    for i in n..candles.len() {
        let (mut pos, mut neg) = (0.0, 0.0);
        for j in i + 1 - n..=i {
            let tp = (candles[j].high + candles[j].low + candles[j].close) / 3.0;
            let prev = (candles[j - 1].high + candles[j - 1].low + candles[j - 1].close) / 3.0;
            let rmf = tp * candles[j].volume;
            if tp > prev {
                pos += rmf;
            } else if tp < prev {
                neg += rmf;
            }
        }
        out[i] = if neg == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + pos / neg) };
    }
    out
}

/// `ta.willr(length)` -- Williams %R, -100..0.
#[must_use]
pub fn ta_willr(candles: &[Candle], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(candles.len().max(1));
    let mut out = vec![NA; candles.len()];
    for i in (n - 1)..candles.len() {
        let w = &candles[i + 1 - n..=i];
        let hh = w.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
        let ll = w.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
        if hh != ll {
            out[i] = -100.0 * (hh - candles[i].close) / (hh - ll);
        }
    }
    out
}

/// Wilder smoothing over a dense (no-na) series; leads with na until the
/// first full window, seeded with that window's mean, then the classic
/// recursion.
fn wilder(source: &[f64], length: usize) -> Series {
    let n = source.len();
    let mut out = vec![NA; n];
    if n < length || length == 0 {
        return out;
    }
    let mut acc: f64 = source[..length].iter().sum();
    out[length - 1] = acc / length as f64;
    for i in length..n {
        acc = acc - acc / length as f64 + source[i];
        out[i] = acc / length as f64;
    }
    out
}

/// `ta.dmi(length)` -> `(+DI, -DI, ADX)`, Wilder's definitions.
#[must_use]
pub fn ta_dmi(candles: &[Candle], length: f64) -> (Series, Series, Series) {
    let len = length.max(1.0) as usize;
    let n = candles.len();
    let mut plus_dm = vec![0.0; n];
    let mut minus_dm = vec![0.0; n];
    let mut tr = vec![0.0; n];
    for i in 1..n {
        let up = candles[i].high - candles[i - 1].high;
        let down = candles[i - 1].low - candles[i].low;
        if up > down && up > 0.0 {
            plus_dm[i] = up;
        }
        if down > up && down > 0.0 {
            minus_dm[i] = down;
        }
        tr[i] = (candles[i].high - candles[i].low)
            .max((candles[i].high - candles[i - 1].close).abs())
            .max((candles[i].low - candles[i - 1].close).abs());
    }
    let sm_plus = wilder(&plus_dm, len);
    let sm_minus = wilder(&minus_dm, len);
    let sm_tr = wilder(&tr, len);
    let mut plus_di = vec![NA; n];
    let mut minus_di = vec![NA; n];
    let mut dx = vec![0.0; n];
    let mut dx_first = None;
    for i in 0..n {
        if sm_tr[i].is_finite() && sm_tr[i] > 0.0 {
            plus_di[i] = 100.0 * sm_plus[i] / sm_tr[i];
            minus_di[i] = 100.0 * sm_minus[i] / sm_tr[i];
            let sum = plus_di[i] + minus_di[i];
            dx[i] = if sum > 0.0 { 100.0 * (plus_di[i] - minus_di[i]).abs() / sum } else { 0.0 };
            if dx_first.is_none() {
                dx_first = Some(i);
            }
        }
    }
    let mut adx = vec![NA; n];
    if let Some(s) = dx_first {
        if s + len <= n {
            let tail = wilder(&dx[s..s + (n - s).min(n)], len);
            for (i, v) in tail.into_iter().enumerate() {
                if s + i < n {
                    adx[s + i] = v;
                }
            }
        }
    }
    (plus_di, minus_di, adx)
}

/// `ta.kc(length, mult)` -> `(basis, upper, lower)` -- ema +- mult*atr.
#[must_use]
pub fn ta_kc(candles: &[Candle], length: f64, mult: f64) -> (Series, Series, Series) {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let basis = ta_ema(&closes, length);
    let rng = ta_atr(candles, length);
    let upper: Series = basis
        .iter()
        .zip(&rng)
        .map(|(b, a)| if b.is_finite() && a.is_finite() { b + mult * a } else { NA })
        .collect();
    let lower: Series = basis
        .iter()
        .zip(&rng)
        .map(|(b, a)| if b.is_finite() && a.is_finite() { b - mult * a } else { NA })
        .collect();
    (basis, upper, lower)
}

/// `ta.supertrend(mult, atr_length)` -> `(line, direction)`. Pine's
/// convention: direction -1 = uptrend (line = lower band under price),
/// 1 = downtrend (line = upper band over price).
#[must_use]
pub fn ta_supertrend(candles: &[Candle], mult: f64, atr_length: f64) -> (Series, Series) {
    let n = candles.len();
    let atr = ta_atr(candles, atr_length);
    let mut final_up = vec![NA; n];
    let mut final_lo = vec![NA; n];
    let mut line = vec![NA; n];
    let mut dir = vec![NA; n];
    for i in 0..n {
        if !atr[i].is_finite() {
            continue;
        }
        let hl2 = (candles[i].high + candles[i].low) / 2.0;
        let up_basic = hl2 + mult * atr[i];
        let lo_basic = hl2 - mult * atr[i];
        if i == 0 || !final_up[i - 1].is_finite() {
            final_up[i] = up_basic;
            final_lo[i] = lo_basic;
            dir[i] = -1.0;
            line[i] = final_lo[i];
            continue;
        }
        let prev_close = candles[i - 1].close;
        final_up[i] =
            if up_basic < final_up[i - 1] || prev_close > final_up[i - 1] { up_basic } else { final_up[i - 1] };
        final_lo[i] =
            if lo_basic > final_lo[i - 1] || prev_close < final_lo[i - 1] { lo_basic } else { final_lo[i - 1] };
        let was_down = (line[i - 1] - final_up[i - 1]).abs() < 1e-9;
        dir[i] = if was_down {
            if candles[i].close > final_up[i] { -1.0 } else { 1.0 }
        } else if candles[i].close < final_lo[i] {
            1.0
        } else {
            -1.0
        };
        line[i] = if dir[i] < 0.0 { final_lo[i] } else { final_up[i] };
    }
    (line, dir)
}

/// `ta.barssince(cond)` -- bars since cond held (na before it ever did).
#[must_use]
pub fn ta_barssince(cond: &[f64]) -> Series {
    let mut out = vec![NA; cond.len()];
    let mut last: Option<usize> = None;
    for (i, v) in cond.iter().enumerate() {
        if v.is_finite() && *v != 0.0 {
            last = Some(i);
        }
        if let Some(l) = last {
            out[i] = (i - l) as f64;
        }
    }
    out
}

/// `ta.valuewhen(cond, source, occurrence)` -- the source's value at the
/// (occurrence+1)-th most recent bar where cond held.
#[must_use]
pub fn ta_valuewhen(cond: &[f64], source: &[f64], occurrence: f64) -> Series {
    let occ = occurrence.max(0.0) as usize;
    let mut out = vec![NA; cond.len()];
    let mut hits: Vec<usize> = Vec::new();
    for i in 0..cond.len() {
        let v = cond[i];
        if v.is_finite() && v != 0.0 {
            hits.push(i);
        }
        if hits.len() > occ {
            let idx = hits[hits.len() - 1 - occ];
            out[i] = source[idx];
        }
    }
    out
}

/// `ta.highestbars(length)` -- Pine's negative offset to the window's
/// highest high (0 = this bar, -1 = one back).
#[must_use]
pub fn ta_highestbars(candles: &[Candle], length: f64) -> Series {
    offsetbars(candles, length, true)
}

/// `ta.lowestbars(length)` -- mirror for the lowest low.
#[must_use]
pub fn ta_lowestbars(candles: &[Candle], length: f64) -> Series {
    offsetbars(candles, length, false)
}

fn offsetbars(candles: &[Candle], length: f64, want_high: bool) -> Series {
    let n = (length.max(1.0) as usize).min(candles.len().max(1));
    let mut out = vec![NA; candles.len()];
    for i in (n - 1)..candles.len() {
        let w = &candles[i + 1 - n..=i];
        let mut best = 0usize;
        for (k, c) in w.iter().enumerate() {
            let (b, v) = if want_high {
                (w[best].high, c.high)
            } else {
                (w[best].low, c.low)
            };
            // Pine's offset points at the MOST RECENT bar sharing the
            // extreme, so a tie moves the pointer forward.
            if (want_high && v >= b) || (!want_high && v <= b) {
                best = k;
            }
        }
        out[i] = -((n - 1 - best) as f64);
    }
    out
}

/// `ta.cum(source)` -- cumulative sum; na bars carry the running total
/// (a gap does not poison the series for the rest of the chart).
#[must_use]
pub fn ta_cum(source: &[f64]) -> Series {
    let mut out = vec![NA; source.len()];
    let mut acc = 0.0;
    let mut started = false;
    for (i, v) in source.iter().enumerate() {
        if v.is_finite() {
            acc += v;
            started = true;
        }
        if started {
            out[i] = acc;
        }
    }
    out
}

/// `ta.pivothigh(source, left, right)` -- the pivot's price, returned on the
/// bar that CONFIRMS it (right bars after the pivot), else na.
#[must_use]
pub fn ta_pivothigh(source: &[f64], left: f64, right: f64) -> Series {
    pivot(source, left, right, true)
}

/// `ta.pivotlow(source, left, right)` -- mirror.
#[must_use]
pub fn ta_pivotlow(source: &[f64], left: f64, right: f64) -> Series {
    pivot(source, left, right, false)
}

fn pivot(source: &[f64], left: f64, right: f64, want_high: bool) -> Series {
    let l = left.max(1.0) as usize;
    let r = right.max(1.0) as usize;
    let mut out = vec![NA; source.len()];
    for i in (l + r)..source.len() {
        let c = i - r;
        let v = source[c];
        if !v.is_finite() {
            continue;
        }
        let ok = source[i - l - r..=i].iter().enumerate().all(|(k, o)| {
            k + i - l - r == c
                || !o.is_finite()
                || if want_high { v > *o } else { v < *o }
        });
        if ok {
            out[i] = v;
        }
    }
    out
}

/// `ta.rising(source, length)` -- strictly growing for length bars back.
#[must_use]
pub fn ta_rising(source: &[f64], length: f64) -> Series {
    monotone(source, length, true)
}

/// `ta.falling(source, length)` -- mirror.
#[must_use]
pub fn ta_falling(source: &[f64], length: f64) -> Series {
    monotone(source, length, false)
}

fn monotone(source: &[f64], length: f64, up: bool) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in n..source.len() {
        let ok = (0..n).all(|k| {
            let (a, b) = (source[i - k], source[i - k - 1]);
            a.is_finite() && b.is_finite() && if up { a > b } else { a < b }
        });
        out[i] = if ok { 1.0 } else { 0.0 };
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
    fn volume_classics_accumulate_the_way_their_names_say() {
        let mk = |close: f64, high: f64, low: f64, vol: f64, i: usize| Candle {
            symbol: "T".to_string(),
            timeframe: analytics_core::types::Timeframe::M1,
            open_time: i as i64 * 60_000_000_000,
            open: close,
            high,
            low,
            close,
            volume: vol,
            buy_volume: 0.0,
            sell_volume: 0.0,
        };
        // up bar vol 10, down bar vol 20, flat bar vol 5.
        let cs = vec![
            mk(10.0, 11.0, 9.0, 10.0, 0),
            mk(11.0, 12.0, 10.0, 10.0, 1), // up
            mk(9.0, 11.5, 8.5, 20.0, 2),   // down
            mk(9.0, 9.5, 8.5, 5.0, 3),     // flat
        ];
        let obv = ta_obv(&cs);
        assert_eq!(obv[3], 10.0 - 20.0, "obv adds up-volume, subtracts down (bar 0 carries nothing)");
        let pvt = ta_pvt(&cs);
        let want = 10.0 * (11.0 - 10.0) / 10.0 + 20.0 * (9.0 - 11.0) / 11.0;
        assert!((pvt[3] - want).abs() < 1e-9, "pvt weights by the fractional move");
        let nvi = ta_nvi(&cs);
        // Bar 2's volume (20) > bar 1's (10): no adjustment. Bar 3's (5) <
        // bar 2's (20): adjust by (9 - 9) / 9 * 100 = 0.
        assert_eq!(nvi[3], 1000.0, "nvi only moves on falling volume");
        let wad = ta_wad(&cs);
        // bar1 up: 11 - min(low 10, prev 10) = 1; bar2 down: -(max(11.5, 11) - 9) = -2.5.
        assert!((wad[3] - (1.0 - 2.5)).abs() < 1e-9, "wad uses the true range edges");
        let ad = ta_ad(&cs);
        // bar0: ((10-9)-(11-10))/2 * 10 = 0; bar1: ((11-10)-(12-11))/2*10 = 0;
        // bar2: ((9-8.5)-(11.5-9))/3 * 20 = (0.5-2.5)/3*20 = -13.33; bar3 flat close mid.
        assert!((ad[2] - (-40.0 / 3.0)).abs() < 1e-6, "chaikin money-flow volume");
    }

    #[test]
    fn vwma_weights_by_volume_and_swma_is_the_fixed_four() {
        let candles = |vols: &[f64]| -> Vec<Candle> {
            vols.iter()
                .map(|v| Candle {
                    symbol: "T".to_string(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: 0,
                    open: 0.0,
                    high: 0.0,
                    low: 0.0,
                    close: 0.0,
                    volume: *v,
                    buy_volume: 0.0,
                    sell_volume: 0.0,
                })
                .collect()
        };
        // Flat volume: vwma == sma.
        let src = rising(6);
        let flat = candles(&[1.0; 6]);
        let vw = ta_vwma(&src, &flat, 3.0);
        let sm = ta_sma(&src, 3.0);
        assert!((vw[5] - sm[5]).abs() < 1e-9, "flat volume = plain mean");
        // Weighted: (10*1 + 20*3) / 4 = 17.5.
        let s2 = vec![10.0, 20.0];
        let c2 = candles(&[1.0, 3.0]);
        let vw2 = ta_vwma(&s2, &c2, 2.0);
        assert!((vw2[1] - 17.5).abs() < 1e-9, "volume tips the mean");
        // swma of 1,2,3,4 = (1 + 4 + 6 + 4) / 6 = 2.5.
        let sw = ta_swma(&vec![1.0, 2.0, 3.0, 4.0]);
        assert!((sw[3] - 2.5).abs() < 1e-9, "the (1,2,2,1)/6 window");
        assert!(sw[2].is_nan(), "swma needs its four bars");
    }

    #[test]
    fn alma_of_a_constant_is_the_constant_and_linreg_hits_the_line() {
        let flat = vec![42.0; 12];
        let al = ta_alma(&flat, 9.0, 0.85, 6.0);
        assert!((al[11] - 42.0).abs() < 1e-9, "normalized weights keep a constant");
        // y = 2x + 1: the regression line at the window's last bar IS y.
        let line: Vec<f64> = (0..10).map(|i| 2.0 * i as f64 + 1.0).collect();
        let lr = ta_linreg(&line, 5.0, 0.0);
        assert!((lr[9] - line[9]).abs() < 1e-9, "offset 0 tracks the line");
        // offset 2 projects two bars of slope forward: y + 2 * slope.
        let lp = ta_linreg(&line, 5.0, 2.0);
        assert!((lp[9] - (line[9] + 4.0)).abs() < 1e-9, "offset projects the slope");
    }

    #[test]
    fn tema_of_a_constant_is_the_constant_after_the_triple_warmup() {
        let flat = vec![7.0; 30];
        let te = ta_tema(&flat, 5.0);
        // Triple warmup: ema seeds at 4, the second at 8, the third at 12.
        assert!(te[11].is_nan(), "the third nesting is still warming");
        assert!((te[12] - 7.0).abs() < 1e-9, "constant survives three nestings");
        // On a rising series TEMA lags less than the plain EMA.
        let up = rising(60);
        let te = ta_tema(&up, 10.0);
        let em = ta_ema(&up, 10.0);
        assert!(
            (up[59] - te[59]).abs() < (up[59] - em[59]).abs(),
            "TEMA hugs the ramp closer than EMA: tema={} ema={} src={}",
            te[59],
            em[59],
            up[59]
        );
    }

    #[test]
    fn correlation_of_proportional_series_is_one() {
        let x = rising(10);
        let y: Vec<f64> = x.iter().map(|v| 3.0 * v + 2.0).collect();
        let out = ta_correlation(&x, &y, 5.0);
        assert!((out[9] - 1.0).abs() < 1e-9, "y = 3x+2 correlates perfectly");
        let neg: Vec<f64> = x.iter().map(|v| -2.0 * v).collect();
        let out = ta_correlation(&x, &neg, 5.0);
        assert!((out[9] + 1.0).abs() < 1e-9, "y = -2x anti-correlates perfectly");
        // A constant leg has zero variance: undefined -> na.
        let flat = vec![7.0; 10];
        let out = ta_correlation(&x, &flat, 5.0);
        assert!(out[9].is_nan(), "flat leg -> na");
    }

    #[test]
    fn covariance_is_population_like_stdev() {
        let x = rising(5);
        let out = ta_covariance(&x, &x, 5.0);
        // cov(x, x) = var(x) = mean of squared deviations of 0..5 = 2.0.
        assert!((out[4] - 2.0).abs() < 1e-9, "self-covariance is the variance");
    }

    #[test]
    fn median_mode_and_deviation_on_a_known_window() {
        // window of five at the tail: 1, 2, 2, 9, 10
        let src = vec![0.0, 1.0, 2.0, 2.0, 9.0, 10.0];
        let med = ta_median(&src, 5.0);
        assert!((med[5] - 2.0).abs() < 1e-9, "median of [1,2,2,9,10]");
        let mode = ta_mode(&src, 5.0);
        assert!((mode[5] - 2.0).abs() < 1e-9, "2 appears twice");
        let dev = ta_deviation(&src, 5.0);
        // mean = 4.8; |x-mean| = 3.8, 2.8, 2.8, 4.2, 5.2 -> 18.8/5 = 3.76
        assert!((dev[5] - 3.76).abs() < 1e-9, "mean absolute deviation");
    }

    #[test]
    fn percentiles_and_percentrank_rank_the_window() {
        let src = rising(6); // 0,1,2,3,4,5 -- window of five at tail: 1..5
        let li = ta_percentile_linear_interpolation(&src, 5.0, 50.0);
        assert!((li[5] - 3.0).abs() < 1e-9, "50th pct of 1,2,3,4,5 by interpolation");
        let nr = ta_percentile_nearest_rank(&src, 5.0, 50.0);
        assert!((nr[5] - 3.0).abs() < 1e-9, "nearest rank of the same window");
        let pr = ta_percentrank(&src, 5.0);
        assert!((pr[5] - 100.0).abs() < 1e-9, "the max ranks 100 (4 of 4 below)");
        let flat = vec![3.0; 6];
        let prf = ta_percentrank(&flat, 5.0);
        assert!((prf[5] - 0.0).abs() < 1e-9, "a flat window ranks 0");
        let rng = ta_range(&src, 5.0);
        assert!((rng[5] - 4.0).abs() < 1e-9, "5 - 1");
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

/// `ta.correlation(x, y, length)` -- Pearson correlation of two sources
/// over the window; na when the window is partial or either leg is flat
/// (zero variance -- the correlation of a constant with anything is
/// undefined, and Pine yields na there too).
#[must_use]
pub fn ta_correlation(x: &[f64], y: &[f64], length: f64) -> Series {
    let n = x.len().min(y.len());
    let len = (length.max(1.0) as usize).min(n.max(1));
    let mut out = vec![NA; n];
    for i in (len - 1)..n {
        let xs = &x[i + 1 - len..=i];
        let ys = &y[i + 1 - len..=i];
        if xs.iter().chain(ys.iter()).any(|v| !v.is_finite()) {
            continue;
        }
        let mx = xs.iter().sum::<f64>() / len as f64;
        let my = ys.iter().sum::<f64>() / len as f64;
        let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
        for k in 0..len {
            let (a, b) = (xs[k] - mx, ys[k] - my);
            num += a * b;
            dx += a * a;
            dy += b * b;
        }
        let den = (dx * dy).sqrt();
        if den > 0.0 {
            out[i] = num / den;
        }
    }
    out
}

/// `ta.covariance(x, y, length)` -- population covariance (divide by
/// length), matching this module's stdev/variance convention.
#[must_use]
pub fn ta_covariance(x: &[f64], y: &[f64], length: f64) -> Series {
    let n = x.len().min(y.len());
    let len = (length.max(1.0) as usize).min(n.max(1));
    let mut out = vec![NA; n];
    for i in (len - 1)..n {
        let xs = &x[i + 1 - len..=i];
        let ys = &y[i + 1 - len..=i];
        if xs.iter().chain(ys.iter()).any(|v| !v.is_finite()) {
            continue;
        }
        let mx = xs.iter().sum::<f64>() / len as f64;
        let my = ys.iter().sum::<f64>() / len as f64;
        out[i] = xs
            .iter()
            .zip(ys.iter())
            .map(|(a, b)| (a - mx) * (b - my))
            .sum::<f64>()
            / len as f64;
    }
    out
}

/// `ta.deviation(source, length)` -- mean absolute deviation from the
/// window's mean.
#[must_use]
pub fn ta_deviation(source: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let w = &source[i + 1 - n..=i];
        if w.iter().all(|v| v.is_finite()) {
            let mean = w.iter().sum::<f64>() / n as f64;
            out[i] = w.iter().map(|v| (v - mean).abs()).sum::<f64>() / n as f64;
        }
    }
    out
}

/// Sort the finite window ascending; the shared engine of median, mode
/// and the percentile family. `None` when the window is partial or holds
/// an na -- the same warmup contract every windowed ta follows here.
fn sorted_window(source: &[f64], i: usize, n: usize) -> Option<Vec<f64>> {
    let w = &source[i + 1 - n..=i];
    if w.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut s = w.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(s)
}

/// `ta.median(source, length)` -- the window's middle value (the mean of
/// the two middles when the length is even).
#[must_use]
pub fn ta_median(source: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        if let Some(s) = sorted_window(source, i, n) {
            out[i] = if n % 2 == 1 {
                s[n / 2]
            } else {
                (s[n / 2 - 1] + s[n / 2]) / 2.0
            };
        }
    }
    out
}

/// `ta.mode(source, length)` -- the window's most frequent value; ties
/// resolve to the SMALLEST of the tied values (deterministic -- exact
/// float ties on market data are rare anyway).
#[must_use]
pub fn ta_mode(source: &[f64], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        if let Some(s) = sorted_window(source, i, n) {
            let (mut best, mut best_n) = (s[0], 0usize);
            let mut k = 0;
            while k < n {
                let mut j = k + 1;
                while j < n && s[j] == s[k] {
                    j += 1;
                }
                if j - k > best_n {
                    best = s[k];
                    best_n = j - k;
                }
                k = j;
            }
            out[i] = best;
        }
    }
    out
}

/// `ta.percentile_linear_interpolation(source, length, pct)` -- the pct-th
/// percentile with linear interpolation between the bracketing ranks.
#[must_use]
pub fn ta_percentile_linear_interpolation(source: &[f64], length: f64, pct: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        if let Some(s) = sorted_window(source, i, n) {
            let pos = (pct.clamp(0.0, 100.0) / 100.0) * (n as f64 - 1.0);
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            let frac = pos - lo as f64;
            out[i] = s[lo] + (s[hi.min(n - 1)] - s[lo]) * frac;
        }
    }
    out
}

/// `ta.percentile_nearest_rank(source, length, pct)` -- the pct-th
/// percentile by the nearest-rank method.
#[must_use]
pub fn ta_percentile_nearest_rank(source: &[f64], length: f64, pct: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        if let Some(s) = sorted_window(source, i, n) {
            let rank = ((pct.clamp(0.0, 100.0) / 100.0) * n as f64).ceil() as usize;
            out[i] = s[rank.clamp(1, n) - 1];
        }
    }
    out
}

/// `ta.percentrank(source, length)` -- 100 * the count of window values
/// STRICTLY below the current value, over length-1. A flat window ranks
/// 0; the first bar of a series is na.
#[must_use]
pub fn ta_percentrank(source: &[f64], length: f64) -> Series {
    let n = (length.max(2.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let w = &source[i + 1 - n..=i];
        if w.iter().all(|v| v.is_finite()) {
            let cur = source[i];
            let below = w.iter().filter(|v| **v < cur).count();
            out[i] = 100.0 * below as f64 / (n as f64 - 1.0);
        }
    }
    out
}

/// `ta.range(source, length)` -- highest minus lowest over the window.
#[must_use]
pub fn ta_range(source: &[f64], length: f64) -> Series {
    let hi = ta_highest(source, length);
    let lo = ta_lowest(source, length);
    hi.iter()
        .zip(lo.iter())
        .map(|(a, b)| if a.is_finite() && b.is_finite() { a - b } else { NA })
        .collect()
}

/// `ta.vwma(source, length)` -- volume-weighted: sum(source * volume) /
/// sum(volume) over the window; na when the window's volume sums to zero.
#[must_use]
pub fn ta_vwma(source: &[f64], candles: &[Candle], length: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let mut num = 0.0;
        let mut den = 0.0;
        let mut ok = true;
        for k in i + 1 - n..=i {
            let vol = candles.get(k).map(|c| c.volume).unwrap_or(NA);
            let s = source[k];
            if !s.is_finite() || !vol.is_finite() {
                ok = false;
                break;
            }
            num += s * vol;
            den += vol;
        }
        if ok && den > 0.0 {
            out[i] = num / den;
        }
    }
    out
}

/// `ta.swma(source)` -- the fixed symmetric 4-bar weighted MA, weights
/// (1, 2, 2, 1) / 6. No length argument: Pine's swma is always 4 bars.
#[must_use]
pub fn ta_swma(source: &[f64]) -> Series {
    const W: [f64; 4] = [1.0 / 6.0, 2.0 / 6.0, 2.0 / 6.0, 1.0 / 6.0];
    let mut out = vec![NA; source.len()];
    for i in 3..source.len() {
        let w = &source[i - 3..=i];
        if w.iter().all(|v| v.is_finite()) {
            out[i] = w.iter().zip(W.iter()).map(|(v, k)| v * k).sum();
        }
    }
    out
}

/// `ta.alma(source, length, offset, sigma)` -- Arnaud Legoux: Gaussian
/// weights centered at floor(offset * (length - 1)) with width
/// length / sigma, so offset > 0.5 skews the weight toward recent bars.
#[must_use]
pub fn ta_alma(source: &[f64], length: f64, offset: f64, sigma: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let m = (offset.clamp(0.0, 1.0) * (n as f64 - 1.0)).floor();
    let s = (n as f64 / sigma.max(0.01)).max(0.01);
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let w = &source[i + 1 - n..=i];
        if w.iter().all(|v| v.is_finite()) {
            let mut num = 0.0;
            let mut den = 0.0;
            for (k, v) in w.iter().enumerate() {
                let d = (k as f64 - m) / s;
                let weight = (-d * d / 2.0).exp();
                num += v * weight;
                den += weight;
            }
            if den > 0.0 {
                out[i] = num / den;
            }
        }
    }
    out
}

/// `ta.linreg(source, length, offset)` -- the least-squares regression
/// line's value at the window's last bar plus `offset` (a positive offset
/// projects the line forward; that projection is the classic "linreg band"
/// idiom). `ta.lsma` is Pine's legacy name for the same function.
#[must_use]
pub fn ta_linreg(source: &[f64], length: f64, offset: f64) -> Series {
    let n = (length.max(1.0) as usize).min(source.len().max(1));
    let off = if offset.is_finite() { offset } else { 0.0 };
    let mut out = vec![NA; source.len()];
    for i in (n - 1)..source.len() {
        let w = &source[i + 1 - n..=i];
        if w.iter().all(|v| v.is_finite()) {
            let nf = n as f64;
            let sx = nf * (nf - 1.0) / 2.0;
            let sxx = nf * (nf - 1.0) * (2.0 * nf - 1.0) / 6.0;
            let sy: f64 = w.iter().sum();
            let sxy: f64 = w.iter().enumerate().map(|(k, v)| k as f64 * v).sum();
            let denom = nf * sxx - sx * sx;
            if denom != 0.0 {
                let slope = (nf * sxy - sx * sy) / denom;
                let intercept = (sy - slope * sx) / nf;
                out[i] = intercept + slope * ((nf - 1.0) + off);
            }
        }
    }
    out
}

/// `ta.lsma(source, length, offset)` -- Pine's legacy alias of `ta.linreg`.
#[must_use]
pub fn ta_lsma(source: &[f64], length: f64, offset: f64) -> Series {
    ta_linreg(source, length, offset)
}

/// Run `f` on the finite tail of `source` -- everything from its first
/// finite value -- and re-pad the warmup with na. This is the adapter that
/// lets a crate-backed average (whose seed needs a finite window, and whose
/// all-or-nothing contract otherwise refuses a series that already HAS a
/// warmup) nest under Pine's "an average of an average starts at the first
/// bar the inner one is valid" rule. TEMA is the customer.
fn on_finite_tail(source: &[f64], f: impl Fn(&[f64]) -> Series) -> Series {
    let Some(start) = source.iter().position(|v| v.is_finite()) else {
        return vec![NA; source.len()];
    };
    let tail = f(&source[start..]);
    let mut out = vec![NA; source.len()];
    out[start..].copy_from_slice(&tail);
    out
}

/// `ta.tema(source, length)` -- triple EMA: 3*ema1 - 3*ema2 + ema3. The
/// nested EMAs run on the finite tail of their input, so the double warmup
/// (then triple) is the only na span, matching Pine.
#[must_use]
pub fn ta_tema(source: &[f64], length: f64) -> Series {
    let e1 = ta_ema(source, length);
    let e2 = on_finite_tail(&e1, |s| ta_ema(s, length));
    let e3 = on_finite_tail(&e2, |s| ta_ema(s, length));
    e1.iter()
        .zip(e2.iter())
        .zip(e3.iter())
        .map(|((a, b), c)| {
            if a.is_finite() && b.is_finite() && c.is_finite() {
                3.0 * a - 3.0 * b + c
            } else {
                NA
            }
        })
        .collect()
}

/// `ta.obv()` -- on-balance volume: cumulative +/- volume by the close's
/// direction (flat closes carry nothing). Starts at 0.
#[must_use]
pub fn ta_obv(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    let mut acc = 0.0;
    for (i, c) in candles.iter().enumerate() {
        if i > 0 {
            let p = candles[i - 1].close;
            if c.close > p {
                acc += c.volume;
            } else if c.close < p {
                acc -= c.volume;
            }
        }
        out[i] = acc;
    }
    out
}

/// `ta.pvt()` -- price-volume trend: cumulative volume weighted by the
/// close's fractional move, so a 2% rally on light volume counts less
/// than the same rally on heavy volume.
#[must_use]
pub fn ta_pvt(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    let mut acc = 0.0;
    for (i, c) in candles.iter().enumerate() {
        if i > 0 {
            let p = candles[i - 1].close;
            if p != 0.0 {
                acc += c.volume * (c.close - p) / p;
            }
        }
        out[i] = acc;
    }
    out
}

/// `ta.nvi()` -- negative volume index: starts at 1000 and adjusts only
/// on bars whose volume FELL from the previous bar, by the close's
/// percentage change times 100 (Fosback's idea: the quiet bars are where
/// the informed money moves).
#[must_use]
pub fn ta_nvi(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    let mut acc = 1000.0;
    for (i, c) in candles.iter().enumerate() {
        if i > 0 {
            let p = &candles[i - 1];
            if c.volume < p.volume && p.close != 0.0 {
                acc += (c.close - p.close) / p.close * 100.0;
            }
        }
        out[i] = acc;
    }
    out
}

/// `ta.wad()` -- Williams accumulation/distribution: accumulates the
/// close's distance from the TRUE low of an up bar (or the true high of a
/// down bar), where "true" means measured against the previous close.
#[must_use]
pub fn ta_wad(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    let mut acc = 0.0;
    for (i, c) in candles.iter().enumerate() {
        if i > 0 {
            let p = candles[i - 1].close;
            if c.close > p {
                acc += c.close - c.low.min(p);
            } else if c.close < p {
                acc -= c.high.max(p) - c.close;
            }
        }
        out[i] = acc;
    }
    out
}

/// `ta.ad()` -- Chaikin accumulation/distribution line: cumulative
/// money-flow volume, where each bar's volume counts in proportion to
/// where the close sits inside its range (a close at the high counts the
/// whole bar; a close at the low counts it negatively).
#[must_use]
pub fn ta_ad(candles: &[Candle]) -> Series {
    let mut out = vec![NA; candles.len()];
    let mut acc = 0.0;
    for (i, c) in candles.iter().enumerate() {
        let range = c.high - c.low;
        if range > 0.0 {
            let mfm = ((c.close - c.low) - (c.high - c.close)) / range;
            acc += mfm * c.volume;
        }
        out[i] = acc;
    }
    out
}
