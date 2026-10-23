//! The quantitative set: z-score, the Kalman filter, CUSUM change-points,
//! and Shannon entropy. Statistical forms, pinned by hand-computed values in
//! the tests.

use crate::indicators::sma::sma;

/// The z-score: `(close - mean) / σ` over `period`. The mean-reversion
/// measurement itself: how many sigmas from home price currently is.
pub fn zscore(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    for i in period - 1..n {
        let window = &closes[i + 1 - period..=i];
        let mean = window.iter().sum::<f64>() / period as f64;
        let var = window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / period as f64;
        let sd = var.sqrt();
        out[i] = Some(if sd > 0.0 { (closes[i] - mean) / sd } else { 0.0 });
    }
    out
}

/// The scalar Kalman filter's local-level estimate of price. The recursion
/// is the whole model: predict (the state persists), update (gain-weighted
/// blend of prediction and measurement). `q` is the process noise, `r` the
/// measurement noise; their RATIO is the responsiveness.
pub fn kalman(closes: &[f64], q: f64, r: f64) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if n == 0 || q <= 0.0 || r <= 0.0 {
        return out;
    }
    // Seed at the first close with the measurement's own uncertainty.
    let mut level = closes[0];
    let mut variance = r;
    out[0] = Some(level);
    for i in 1..n {
        // Predict: the level persists, its uncertainty grows by the process noise.
        variance += q;
        // Update: the Kalman gain blends prediction and measurement.
        let gain = variance / (variance + r);
        level += gain * (closes[i] - level);
        variance *= 1.0 - gain;
        out[i] = Some(level);
    }
    out
}

/// The signed CUSUM stat: the normalized maximum excursion, given the
/// CURRENT deviation's sign. Continuous (a line for the pane); the
/// change-point signal is this stat crossing its threshold.
pub fn cusum_stat(closes: &[f64], window: usize) -> Vec<Option<f64>> {
    cusum_inner(closes, window, 0.0)
}

/// The CUSUM change-point: over the trailing `window`, the cumulative
/// signed deviation of returns from their own mean, normalized by sigma.
/// The largest excursion marks the change; its SIGN is the new direction.
/// Fires when the normalized excursion passes `threshold` (the classic
/// 4-sigma-ish gate) and the newest bar confirms it.
pub fn cusum_changepoint(closes: &[f64], window: usize, threshold: f64) -> Vec<Option<f64>> {
    cusum_inner(closes, window, threshold)
}

fn cusum_inner(closes: &[f64], window: usize, threshold: f64) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if window < 10 || n < window + 1 {
        return out;
    }
    for i in window..n {
        let mut rets = Vec::with_capacity(window);
        for j in i + 1 - window..=i {
            if closes[j - 1] > 0.0 {
                rets.push((closes[j] / closes[j - 1]).ln());
            }
        }
        if rets.len() < window / 2 {
            continue;
        }
        let mean = rets.iter().sum::<f64>() / rets.len() as f64;
        let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rets.len() as f64;
        let sd = var.sqrt();
        if sd <= 0.0 {
            continue;
        }
        // The maximum |CUSUM| excursion and its sign.
        let mut sum: f64 = 0.0;
        let mut max_pos: f64 = 0.0;
        let mut max_neg: f64 = 0.0;
        for r in &rets {
            sum += r - mean;
            max_pos = max_pos.max(sum);
            max_neg = max_neg.min(sum);
        }
        let stat = max_pos.max(-max_neg) / (sd * (rets.len() as f64).sqrt());
        // The direction is the CURRENT cumulative deviation's sign: the
        // window's older peak says where the regime came FROM, and
        // reporting that as the change's direction inverts the answer
        // exactly when the break is fresh.
        if threshold <= 0.0 {
            out[i] = Some(stat * sum.signum());
        } else if stat >= threshold {
            out[i] = Some(sum.signum());
        }
    }
    out
}

/// Shannon entropy of the return DISTRIBUTION over `window`, normalized to
/// 0..1 (1 = maximally disordered, uniform across the bins). Returns of the
/// window are binned into `bins` equal-width buckets.
pub fn shannon_entropy(closes: &[f64], window: usize, bins: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if window < 10 || bins < 2 || n < window + 1 {
        return out;
    }
    for i in window..n {
        let mut rets = Vec::with_capacity(window);
        for j in i + 1 - window..=i {
            if closes[j - 1] > 0.0 {
                rets.push((closes[j] / closes[j - 1]).ln());
            }
        }
        if rets.len() < window / 2 {
            continue;
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for r in &rets {
            lo = lo.min(*r);
            hi = hi.max(*r);
        }
        let span = hi - lo;
        if span <= 0.0 {
            out[i] = Some(0.0);
            continue;
        }
        let mut counts = vec![0usize; bins];
        for r in &rets {
            let b = (((r - lo) / span) * bins as f64).min((bins - 1) as f64) as usize;
            counts[b] += 1;
        }
        let total = rets.len() as f64;
        let mut h = 0.0;
        for c in counts.iter().filter(|c| **c > 0) {
            let p = *c as f64 / total;
            h -= p * p.ln();
        }
        out[i] = Some(h / (bins as f64).ln());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zscore_is_sigmas_from_home() {
        // Flat at 10 forever, then one bar at 12: the z of the outlier
        // against a tight window reads very high.
        let mut closes = vec![10.0; 30];
        closes.push(12.0);
        let v = zscore(&closes, 20);
        assert!(v[30].unwrap() > 2.0, "the outlier reads several sigmas: {}", v[30].unwrap());
        // Flat is zero, not NaN.
        let flat = zscore(&vec![10.0; 25], 20);
        assert_eq!(flat[24], Some(0.0));
    }

    #[test]
    fn the_kalman_tracks_and_smooths() {
        // A step from 100 to 110 with noise: the filter approaches the new
        // level and lags it by less than the naive last price would claim.
        let mut closes = Vec::new();
        for i in 0..40 {
            closes.push(if i < 20 { 100.0 } else { 110.0 } + if i % 2 == 0 { 0.3 } else { -0.3 });
        }
        let v = kalman(&closes, 0.01, 0.5);
        let last = v[39].unwrap();
        assert!(last > 105.0 && last < 110.5, "settled near the new level: {last}");
        // The filtered line is smoother than the raw: its bar-to-bar swing
        // is smaller than the raw series'.
        let raw_swing: f64 = closes.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>();
        let kal_swing: f64 = v.windows(2).map(|w| (w[1].unwrap() - w[0].unwrap()).abs()).sum::<f64>();
        assert!(kal_swing < raw_swing, "the filter smooths: {kal_swing} < {raw_swing}");
    }

    #[test]
    fn the_cusum_marks_the_regime_change() {
        // Calm drift up, then a sharp regime break down.
        let mut closes: Vec<f64> = (0..50).map(|i| 100.0 + i as f64 * 0.05).collect();
        for i in 0..15 {
            closes.push(closes[49] - (i + 1) as f64 * 0.8);
        }
        let v = cusum_changepoint(&closes, 40, 2.5);
        let fires: Vec<usize> = (0..v.len()).filter(|i| v[*i].is_some()).collect();
        assert!(!fires.is_empty(), "the regime break is marked");
        let last = v[*fires.last().unwrap()].unwrap();
        assert!(last < 0.0, "the new direction is down");
        // A constant-rate (geometric) drift has no change-point: constant
        // log returns mean the CUSUM stays home. (Linear growth's log
        // returns SHRINK with price, and a systematic decline in returns is
        // itself a change -- the first calm fixture was the trap.)
        let calm: Vec<f64> = (0..50).map(|i| 100.0 * 1.0005f64.powi(i)).collect();
        assert!(cusum_changepoint(&calm, 40, 2.5).iter().all(|v| v.is_none()));
    }

    #[test]
    fn the_entropy_orders_disorder() {
        // GEOMETRIC growth: constant log returns, one bin dominates -> low
        // entropy. (Linear growth's log returns shrink with price and SPREAD
        // across bins -- entropy measures the magnitude spread, not the
        // sign's consistency.) A coin-flip series: returns spread -> high.
        let trend: Vec<f64> = (0..50).map(|i| 100.0 * 1.001f64.powi(i)).collect();
        // The disordered series: returns SPREAD across the range (a
        // deterministic pseudo-random magnitude), covering every bin.
        let mut flip = Vec::new();
        let mut price = 100.0;
        for i in 0..50 {
            let mag = ((i * 7919) % 100) as f64 / 100.0 * 0.012 + 0.002;
            price *= if i % 2 == 0 { 1.0 + mag } else { 1.0 - mag };
            flip.push(price);
        }
        let et = shannon_entropy(&trend, 30, 6)[49].unwrap();
        let ef = shannon_entropy(&flip, 30, 6)[49].unwrap();
        assert!(ef > et, "disorder reads higher: {ef} vs {et}");
        assert!((0.0..=1.0).contains(&ef));
    }
}
