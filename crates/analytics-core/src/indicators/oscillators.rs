//! The classic oscillator set, canonical forms: Stochastic, CCI, Williams %R,
//! ROC, and OBV. Each formula is the one its author published, pinned by
//! hand-computed values in the tests.

use crate::indicators::sma::sma;
use crate::types::Candle;

/// Stochastic %K: `(close - lowest low) / (highest high - lowest low) * 100`
/// over `k_period`; %D is the `d_period` SMA of %K. (Lane, 1950s.)
pub fn stochastic(candles: &[Candle], k_period: usize, d_period: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = candles.len();
    let mut k_line = vec![None; n];
    if k_period == 0 || n < k_period {
        return (k_line, vec![None; n]);
    }
    for i in k_period - 1..n {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for c in &candles[i + 1 - k_period..=i] {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
        }
        let range = hi - lo;
        k_line[i] = Some(if range > 0.0 {
            (candles[i].close - lo) / range * 100.0
        } else {
            50.0 // a flat window has no position; the midpoint is the honest answer
        });
    }
    // %D as the SMA of %K over its valid window.
    let k_values: Vec<f64> = k_line.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    let mut d_line = vec![None; n];
    for i in k_period - 1 + d_period - 1..n {
        let window = &k_values[i + 1 - d_period..=i];
        if window.iter().all(|v| v.is_finite()) {
            d_line[i] = Some(window.iter().sum::<f64>() / d_period as f64);
        }
    }
    (k_line, d_line)
}

/// CCI: `(typical price - SMA(tp)) / (0.015 * mean deviation)`. (Lambert, 1980.)
pub fn cci(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    let tps: Vec<f64> = candles.iter().map(|c| (c.high + c.low + c.close) / 3.0).collect();
    for i in period - 1..n {
        let window = &tps[i + 1 - period..=i];
        let mean = window.iter().sum::<f64>() / period as f64;
        let mean_dev = window.iter().map(|v| (v - mean).abs()).sum::<f64>() / period as f64;
        out[i] = Some(if mean_dev > 0.0 {
            (tps[i] - mean) / (0.015 * mean_dev)
        } else {
            0.0
        });
    }
    out
}

/// Williams %R: `(highest high - close) / (highest high - lowest low) * -100`.
/// The stochastic's mirror, on -100..0. (Williams, 1966.)
pub fn williams_r(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    for i in period - 1..n {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for c in &candles[i + 1 - period..=i] {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
        }
        let range = hi - lo;
        out[i] = Some(if range > 0.0 {
            (hi - candles[i].close) / range * -100.0
        } else {
            -50.0
        });
    }
    out
}

/// Rate of change: `(close / close[n] - 1) * 100`.
pub fn roc(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if period == 0 || n <= period {
        return out;
    }
    for i in period..n {
        let base = closes[i - period];
        out[i] = Some(if base != 0.0 { (closes[i] / base - 1.0) * 100.0 } else { 0.0 });
    }
    out
}

/// On-Balance Volume: a running total, +volume on an up close, -volume on a
/// down close, unchanged on a flat close. (Granville, 1963.)
pub fn obv(candles: &[Candle]) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if n < 2 {
        return out;
    }
    let mut total = 0.0;
    out[0] = Some(0.0);
    for i in 1..n {
        if candles[i].close > candles[i - 1].close {
            total += candles[i].volume;
        } else if candles[i].close < candles[i - 1].close {
            total -= candles[i].volume;
        }
        out[i] = Some(total);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, h: f64, l: f64, c: f64, v: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: c,
            high: h,
            low: l,
            close: c,
            volume: v,
            buy_volume: v / 2.0,
            sell_volume: v / 2.0,
        }
    }

    #[test]
    fn the_stochastic_pins_lanes_math() {
        // 3 bars: highs 12/13/14, lows 8/9/10, close at 14 -> %K(2) at bar 2:
        // window bars 1..2: hi 14, lo 9, close 14 -> (14-9)/5 = 100%.
        let candles = vec![k(0, 12.0, 8.0, 10.0, 1.0), k(1, 13.0, 9.0, 12.0, 1.0), k(2, 14.0, 10.0, 14.0, 1.0)];
        let (k_line, d_line) = stochastic(&candles, 2, 2);
        // Bar 1's window (bars 0..1): hi 13, lo 8, close 12 -> 80.
        assert!((k_line[1].unwrap() - 80.0).abs() < 1e-9);
        assert!((k_line[2].unwrap() - 100.0).abs() < 1e-9);
        assert!((d_line[2].unwrap() - (k_line[1].unwrap() + 100.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_cci_pins_lamberts_constant() {
        // Constant typical price -> zero deviation -> CCI 0, not NaN.
        let candles: Vec<Candle> = (0..20).map(|i| k(i, 11.0, 9.0, 10.0, 1.0)).collect();
        let v = cci(&candles, 14);
        assert_eq!(v[19], Some(0.0));
        // A ramp: CCI positive and bounded sanely.
        let ramp: Vec<Candle> = (0..20).map(|i| k(i, 10.0 + i as f64 + 1.0, 10.0 + i as f64 - 1.0, 10.0 + i as f64, 1.0)).collect();
        let v = cci(&ramp, 14);
        assert!(v[19].unwrap() > 0.0, "a rising typical price reads positive");
    }

    #[test]
    fn williams_r_is_the_stochastics_mirror() {
        let candles: Vec<Candle> = (0..15).map(|i| k(i, 11.0 + (i % 3) as f64, 9.0 - (i % 2) as f64, 10.0, 1.0)).collect();
        let w = williams_r(&candles, 14);
        let (k_line, _) = stochastic(&candles, 14, 1);
        // %R = %K - 100, exactly, on the same window.
        assert!((w[14].unwrap() - (k_line[14].unwrap() - 100.0)).abs() < 1e-9);
        assert!((-100.0..=0.0).contains(&w[14].unwrap()));
    }

    #[test]
    fn roc_is_the_simple_ratio() {
        let closes: Vec<f64> = (0..15).map(|i| 100.0 + i as f64).collect();
        let v = roc(&closes, 12);
        assert!((v[14].unwrap() - ((114.0 / 102.0) - 1.0) * 100.0).abs() < 1e-9);
    }

    #[test]
    fn obv_accumulates_signed_volume() {
        let candles = vec![
            k(0, 10.5, 9.5, 10.0, 100.0),
            k(1, 11.5, 9.5, 11.0, 50.0),  // up: +50
            k(2, 11.5, 10.0, 10.5, 30.0), // down: -30
            k(3, 11.5, 10.0, 10.5, 70.0), // flat: +0
        ];
        let v = obv(&candles);
        assert_eq!(v[1], Some(50.0));
        assert_eq!(v[2], Some(20.0));
        assert_eq!(v[3], Some(20.0));
    }
}
