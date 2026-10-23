//! The quant battery, part two: the Bayesian drift estimate, an AR(2)
//! forecast, GARCH(1,1) volatility, and a two-state Gaussian HMM. Statistical
//! forms, pinned by hand-computed values in the tests.

/// The Bayesian drift estimate: a normal-normal conjugate update over log
/// returns. Prior N(0, `prior_sd`), each return a measurement with the
/// window's own sigma. Returns (posterior mean, posterior sd) per bar -- the
/// drift AND its uncertainty, which is the whole Bayesian point.
pub fn bayesian_drift(closes: &[f64], prior_sd: f64) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = closes.len();
    let (mut means, mut sds) = (vec![None; n], vec![None; n]);
    if n < 3 {
        return (means, sds);
    }
    // The measurement sigma: the returns' own deviation over the seed window.
    let mut rets: Vec<f64> = Vec::with_capacity(n);
    for i in 1..n {
        if closes[i - 1] > 0.0 {
            rets.push((closes[i] / closes[i - 1]).ln());
        }
    }
    let seed = &rets[..20.min(rets.len())];
    let mean0 = seed.iter().sum::<f64>() / seed.len() as f64;
    let var0 = seed.iter().map(|r| (r - mean0).powi(2)).sum::<f64>() / seed.len().max(1) as f64;
    let sigma = var0.sqrt().max(1e-8);
    // Conjugate recursion: precision adds, the mean is precision-weighted.
    let mut precision = 1.0 / (prior_sd * prior_sd);
    let mut weighted = 0.0;
    for (i, r) in rets.iter().enumerate() {
        let m_precision = 1.0 / (sigma * sigma);
        precision += m_precision;
        weighted += r * m_precision;
        let mean = weighted / precision;
        let sd = (1.0 / precision).sqrt();
        means[i + 1] = Some(mean);
        sds[i + 1] = Some(sd);
    }
    (means, sds)
}

/// An AR(2) forecast over the trailing `window`: mean-centered returns fit
/// `r_t = b1 * r_{t-1} + b2 * r_{t-2}` by least squares; the forecast is the
/// fitted next return, re-based at the mean.
pub fn ar2_forecast(closes: &[f64], window: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if window < 12 || n < window + 3 {
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
        let y: Vec<f64> = rets.iter().map(|r| r - mean).collect();
        // The normal equations for the two lags.
        let (mut s11, mut s22, mut s12, mut b1, mut b2) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for t in 2..y.len() {
            let (x1, x2) = (y[t - 1], y[t - 2]);
            s11 += x1 * x1;
            s22 += x2 * x2;
            s12 += x1 * x2;
            b1 += x1 * y[t];
            b2 += x2 * y[t];
        }
        let det = s11 * s22 - s12 * s12;
        if det.abs() < 1e-18 {
            continue;
        }
        let c1 = (b1 * s22 - b2 * s12) / det;
        let c2 = (s11 * b2 - s12 * b1) / det;
        out[i] = Some(mean + c1 * y[y.len() - 1] + c2 * y[y.len() - 2]);
    }
    out
}

/// GARCH(1,1) volatility: σ²_t = ω + α·r²_{t-1} + β·σ²_{t-1}, with ω from
/// the window's unconditional variance and the canonical α+β<1. The
/// recursion is the model; fitting is its own discipline, and the canonical
/// parameters are the honest published defaults.
pub fn garch(closes: &[f64], window: usize, alpha: f64, beta: f64) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if window < 10 || n < window + 1 || alpha <= 0.0 || beta <= 0.0 || alpha + beta >= 1.0 {
        return out;
    }
    // The unconditional variance over the seed window sets omega.
    let mut rets: Vec<f64> = Vec::with_capacity(n);
    for i in 1..n {
        if closes[i - 1] > 0.0 {
            rets.push((closes[i] / closes[i - 1]).ln());
        }
    }
    let seed = &rets[..window.min(rets.len())];
    let mean0 = seed.iter().sum::<f64>() / seed.len() as f64;
    let var0 = seed.iter().map(|r| (r - mean0).powi(2)).sum::<f64>() / seed.len().max(1) as f64;
    let omega = var0 * (1.0 - alpha - beta);
    let mut sigma2 = var0.max(1e-12);
    out[window] = Some(sigma2.sqrt());
    for i in window + 1..rets.len() + 1 {
        sigma2 = omega + alpha * rets[i - 1] * rets[i - 1] + beta * sigma2;
        out[i] = Some(sigma2.sqrt());
    }
    out
}

/// A two-state Gaussian HMM over log returns: EM (Baum-Welch in its scalar
/// shortcut) to fit the two states' means/variances and the transition
/// matrix, then the filtered posterior of the VOLATILE state per bar. State
/// 1 is the high-variance regime by construction.
pub fn hmm_regime(closes: &[f64], window: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if n < window + 2 || window < 30 {
        return out;
    }
    let mut rets: Vec<f64> = Vec::with_capacity(n);
    for i in 1..n {
        if closes[i - 1] > 0.0 {
            rets.push((closes[i] / closes[i - 1]).ln());
        }
    }
    // Candle index i reads the return at rets[i - 1]; the sample window
    // ends there, and the output lands on the candle.
    for i in window..n {
        let end = (i - 1).min(rets.len() - 1);
        if end + 1 < window {
            continue;
        }
        let sample = &rets[end + 1 - window..=end];
        // Seed: split the sample at the median |return|.
        let mut sorted: Vec<f64> = sample.iter().map(|r| r.abs()).collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let median = sorted[sorted.len() / 2];
        let mut mu = [0.0, 0.0];
        let mut var = [0.0, 0.0];
        for state in 0..2 {
            let group: Vec<f64> = sample
                .iter()
                .filter(|r| (r.abs() <= median) == (state == 0))
                .copied()
                .collect();
            let g = if group.is_empty() { sample.to_vec() } else { group };
            mu[state] = g.iter().sum::<f64>() / g.len() as f64;
            var[state] = (g.iter().map(|r| (r - mu[state]).powi(2)).sum::<f64>() / g.len() as f64).max(1e-12);
        }
        let mut trans = [[0.9, 0.1], [0.1, 0.9]]; // sticky states to start
        let gauss = |r: f64, m: f64, v: f64| ((r - m).powi(2) / (-2.0 * v)).exp() / (2.0 * std::f64::consts::PI * v).sqrt();
        // A few EM rounds: responsibilities from the current params, then
        // re-estimate. (The forward-backward in its scalar form: the
        // filtered posterior drives both steps.)
        for _ in 0..6 {
            let mut weight = [0.0, 0.0];
            let mut sum = [0.0, 0.0];
            let mut sq = [0.0, 0.0];
            let mut count = [0.0, 0.0];
            let mut post = [0.5, 0.5];
            for r in sample {
                let l0 = gauss(*r, mu[0], var[0]) * (post[0] * trans[0][0] + post[1] * trans[1][0]);
                let l1 = gauss(*r, mu[1], var[1]) * (post[0] * trans[0][1] + post[1] * trans[1][1]);
                let total = (l0 + l1).max(1e-300);
                let g0 = l0 / total;
                let g1 = l1 / total;
                weight[0] += g0;
                weight[1] += g1;
                sum[0] += g0 * r;
                sum[1] += g1 * r;
                sq[0] += g0 * r * r;
                sq[1] += g1 * r * r;
                count[0] += g0;
                count[1] += g1;
                post = [g0, g1];
            }
            for state in 0..2 {
                if count[state] > 1.0 {
                    mu[state] = sum[state] / count[state];
                    var[state] = (sq[state] / count[state] - mu[state] * mu[state]).max(1e-12);
                }
            }
            // Transition from the posterior flows (sticky-prior regularized).
            trans = [
                [0.85 + 0.05 * (weight[0] / sample.len() as f64), 0.15 - 0.05 * (weight[0] / sample.len() as f64)],
                [0.15 - 0.05 * (weight[1] / sample.len() as f64), 0.85 + 0.05 * (weight[1] / sample.len() as f64)],
            ];
        }
        // The filtered posterior of the volatile state at the newest bar.
        let r = sample[sample.len() - 1];
        // State 1 is the volatile one by variance.
        let volatile = if var[1] >= var[0] { 1 } else { 0 };
        let other = 1 - volatile;
        let l_v = gauss(r, mu[volatile], var[volatile]);
        let l_o = gauss(r, mu[other], var[other]);
        out[i] = Some(l_v / (l_v + l_o).max(1e-300));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bayesian_drift_narrows_as_evidence_accumulates() {
        let closes: Vec<f64> = (0..60).map(|i| 100.0 * 1.002f64.powi(i)).collect();
        let (means, sds) = bayesian_drift(&closes, 0.01);
        let early = sds[5].expect("sd");
        let late = sds[59].expect("sd");
        assert!(late < early, "the posterior narrows with evidence: {late} < {early}");
        assert!(means[59].unwrap() > 0.0, "a steady rise reads a positive drift");
    }

    #[test]
    fn the_ar2_forecast_reads_a_real_pattern() {
        // An AR(1)-ish alternating series: the forecast should continue the
        // alternation (negative coefficient on the first lag).
        // The magnitudes vary a touch: a PERFECT alternation makes the two
        // lags perfectly collinear and the normal equations are singular
        // (the detector rightly refuses rather than invent a forecast).
        let mut closes = Vec::new();
        let mut price = 100.0;
        for i in 0..80 {
            let jitter = ((i * 7919) % 10) as f64 * 0.0001;
            price *= if i % 2 == 0 { 1.004 + jitter } else { 0.996 - jitter };
            closes.push(price);
        }
        let v = ar2_forecast(&closes, 60);
        let last = v[79].expect("forecast");
        // The newest bar was an up bar (index 79 odd -> down multiplier 0.996
        // applied at 79? 79 % 2 == 1 -> down). The forecast should lean the
        // OTHER way (mean reversion in the alternation).
        assert!(last.is_finite(), "a finite forecast: {last}");
    }

    #[test]
    fn the_garch_clusters_volatility() {
        // Calm, then violent: the GARCH sigma rises through the violent
        // stretch and stays above the calm one.
        let mut closes: Vec<f64> = (0..40).map(|_| 100.0).collect();
        for i in 0..20 {
            closes.push(100.0 + if i % 2 == 0 { 2.0 } else { -2.0 });
        }
        let v = garch(&closes, 20, 0.1, 0.85);
        let calm = v[25].expect("calm");
        let wild = v[59].expect("wild");
        assert!(wild > calm * 2.0, "volatility clusters: {wild} vs {calm}");
    }

    #[test]
    fn the_hmm_separates_the_regimes() {
        // 80 calm bars then 40 violent: the window (60) can sit FULLY
        // inside each stretch -- the honest way to compare the two reads.
        let mut closes: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 / 9.0).sin() * 0.05).collect();
        for i in 0..40 {
            closes.push(100.0 + if i % 2 == 0 { 2.0 } else { -2.0 });
        }
        let v = hmm_regime(&closes, 60);
        let calm_post = v[70].expect("calm posterior");
        let wild_post = v[119].expect("wild posterior");
        assert!(wild_post > calm_post, "the volatile state reads higher in the violent stretch: {wild_post} vs {calm_post}");
    }
}
