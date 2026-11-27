//! Additional deterministic research methods from the market reverse-engineering
//! programme.
//!
//! Every function here is synchronous, seeded when randomness would otherwise
//! be needed, and free of learned weights. Results are measurements of the
//! supplied data, not calibrated trading signals.

use std::collections::BTreeMap;

/// Seed a value into one of `bins` equal-width buckets over `[low, high]`.
fn bin(value: f64, low: f64, high: f64, bins: usize) -> usize {
    if bins == 0 || !value.is_finite() || !(high > low) {
        return 0;
    }
    let scaled = (value - low) / (high - low);
    ((scaled * bins as f64) as usize).min(bins - 1)
}

fn entropy(counts: &BTreeMap<(usize, usize), usize>, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    counts
        .values()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

/// Categorical transfer entropy from `source` to `target`, in bits.
///
/// Both series are binned over their own observed range. TE measures whether
/// the source's previous state reduces uncertainty about the target's current
/// state beyond the target's own previous state. Finite samples bias TE
/// upward; callers must compare against a shuffled baseline out of sample.
#[must_use]
pub fn transfer_entropy(source: &[f64], target: &[f64], bins: usize) -> Option<f64> {
    if source.len() != target.len() || source.len() < 30 || bins < 2 {
        return None;
    }
    let (s_low, s_high) = bounds(source)?;
    let (t_low, t_high) = bounds(target)?;
    let mut joint: BTreeMap<(usize, usize, usize), usize> = BTreeMap::new();
    let mut target_pair: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut target_now: BTreeMap<usize, usize> = BTreeMap::new();
    for index in 1..source.len() {
        if !source[index - 1].is_finite()
            || !target[index].is_finite()
            || !target[index - 1].is_finite()
        {
            return None;
        }
        let s = bin(source[index - 1], s_low, s_high, bins);
        let now = bin(target[index], t_low, t_high, bins);
        let past = bin(target[index - 1], t_low, t_high, bins);
        *joint.entry((now, past, s)).or_default() += 1;
        *target_pair.entry((now, past)).or_default() += 1;
        *target_now.entry(now).or_default() += 1;
    }
    let total = (source.len() - 1) as f64;
    let x_past_counts: BTreeMap<(usize, usize), usize> = joint
        .iter()
        .map(|((now, past, s), count)| ((*past, *s), *count))
        .collect();
    let mut h_y_given_past = 0.0;
    let mut h_y_given_past_and_source = 0.0;
    let pair_counts = collapse3(&joint, |(_, past, _)| *past);
    for ((now, past), count) in &target_pair {
        let p_y_past = *count as f64 / total;
        let p_y = target_now[now] as f64 / total;
        let p_past = pair_counts[past] as f64 / total;
        h_y_given_past += p_y_past * (p_y / p_past).log2();
    }
    for ((now, past, source_state), count) in &joint {
        let p = *count as f64 / total;
        let p_y_past = target_pair[&(*now, *past)] as f64 / total;
        let p_past_source = x_past_counts[&(*past, *source_state)] as f64 / total;
        h_y_given_past_and_source += p * (p_y_past / p_past_source).log2();
    }
    Some((h_y_given_past - h_y_given_past_and_source).max(0.0))
}

fn bounds(values: &[f64]) -> Option<(f64, f64)> {
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for value in values {
        if !value.is_finite() {
            return None;
        }
        low = low.min(*value);
        high = high.max(*value);
    }
    (high > low).then_some((low, high))
}

fn collapse3(
    source: &BTreeMap<(usize, usize, usize), usize>,
    key: impl Fn(&(usize, usize, usize)) -> usize,
) -> BTreeMap<usize, usize> {
    let mut out = BTreeMap::new();
    for (k, count) in source {
        *out.entry(key(k)).or_default() += *count;
    }
    out
}

fn variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64
}

/// Hurst exponent from aggregated-variance scaling.
///
/// `H > 0.5` indicates persistence, `H < 0.5` mean reversion. This is an
/// estimate on the supplied window, not a claim about the generating process.
#[must_use]
pub fn hurst_exponent(values: &[f64], min_window: usize, max_window: usize) -> Option<f64> {
    if values.len() < max_window * 2 || min_window < 2 || max_window <= min_window {
        return None;
    }
    let mut points = Vec::new();
    let mut window = min_window;
    while window <= max_window {
        let mut means = Vec::new();
        for chunk in values.chunks(window) {
            if chunk.len() == window {
                means.push(chunk.iter().sum::<f64>() / window as f64);
            }
        }
        if means.len() >= 2 {
            let variance = variance(&means);
            if variance > 0.0 && variance.ln().is_finite() {
                points.push(((window as f64).ln(), variance.ln()));
            }
        }
        window *= 2;
    }
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let sx: f64 = points.iter().map(|p| p.0).sum();
    let sy: f64 = points.iter().map(|p| p.1).sum();
    let sxx: f64 = points.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let denominator = n * sxx - sx * sx;
    if denominator.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denominator;
    slope.is_finite().then_some(slope)
}

/// Haar wavelet detail energy at one resolution level.
#[must_use]
pub fn wavelet_energy(values: &[f64], level: usize) -> Option<f64> {
    if level == 0 || values.len() < (1usize << level) {
        return None;
    }
    let mut detail = Vec::new();
    for chunk in values.chunks(1usize << level) {
        if chunk.len() < (1usize << level) {
            continue;
        }
        let half = chunk.len() / 2;
        let mean_a: f64 = chunk[..half].iter().sum::<f64>() / half as f64;
        let mean_b: f64 = chunk[half..].iter().sum::<f64>() / half as f64;
        detail.push(mean_a - mean_b);
    }
    if detail.is_empty() {
        return None;
    }
    let energy: f64 = detail.iter().map(|value| value * value).sum();
    energy.is_finite().then_some(energy)
}

/// Recurrence rate of a delay embedding: share of state pairs closer than
/// `threshold`. Deterministic and bounded in `[0, 1]`.
#[must_use]
pub fn recurrence_rate(
    values: &[f64],
    embedding: usize,
    delay: usize,
    threshold: f64,
) -> Option<f64> {
    if embedding == 0 || delay == 0 || threshold <= 0.0 {
        return None;
    }
    let count = values.len().saturating_sub((embedding - 1) * delay);
    if count < 10 {
        return None;
    }
    let states: Vec<Vec<f64>> = (0..count)
        .map(|start| {
            (0..embedding)
                .map(|lag| values[start + lag * delay])
                .collect()
        })
        .collect();
    let mut recurrent = 0usize;
    let mut pairs = 0usize;
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            let distance = states[i]
                .iter()
                .zip(&states[j])
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            pairs += 1;
            if distance <= threshold {
                recurrent += 1;
            }
        }
    }
    (pairs > 0).then_some(recurrent as f64 / pairs as f64)
}

/// A deterministic particle filter for a local-level model.
///
/// Residual resampling uses a seeded xorshift64*, so repeated calls with the
/// same inputs give the same answer. `observations` are prices in any scale.
#[must_use]
pub fn particle_filter(
    observations: &[f64],
    particles: usize,
    process_noise: f64,
    observation_noise: f64,
    seed: u64,
) -> Option<Vec<f64>> {
    if observations.is_empty() || particles < 2 || process_noise <= 0.0 || observation_noise <= 0.0
    {
        return None;
    }
    let mut rng = XorShift(seed);
    let mut states: Vec<f64> = (0..particles).map(|_| observations[0]).collect();
    let mut out = Vec::with_capacity(observations.len());
    for observation in observations {
        for state in &mut states {
            *state += process_noise * (rng.next_f64() - 0.5);
        }
        let mut weights = Vec::with_capacity(particles);
        for state in &states {
            let error = observation - state;
            let likelihood = (-error * error / (2.0 * observation_noise * observation_noise)).exp();
            weights.push(likelihood.max(1e-300));
        }
        let total: f64 = weights.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return None;
        }
        let mut cumulative = Vec::with_capacity(particles);
        let mut running = 0.0;
        for weight in &weights {
            running += weight / total;
            cumulative.push(running);
        }
        let mut resampled = Vec::with_capacity(particles);
        for _ in 0..particles {
            let draw = rng.next_f64();
            let index = cumulative
                .iter()
                .position(|value| draw <= *value)
                .unwrap_or(particles - 1);
            resampled.push(states[index]);
        }
        states = resampled;
        out.push(states.iter().sum::<f64>() / particles as f64);
    }
    Some(out)
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x << 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Bayesian online change-point detection over a Gaussian mean.
///
/// Returns the most probable run length at each observation. The prior is a
/// constant hazard, the observation variance is supplied by the caller, and
/// the recursion uses only closed-form Gaussian predictive updates.
#[must_use]
pub fn bocpd(observations: &[f64], hazard: f64, obs_var: f64) -> Option<Vec<usize>> {
    if observations.is_empty() || !(0.0..1.0).contains(&hazard) || obs_var <= 0.0 {
        return None;
    }
    let mut growth: Vec<f64> = vec![1.0];
    let mut run_lengths = Vec::with_capacity(observations.len());
    for observation in observations {
        if !observation.is_finite() {
            return None;
        }
        let mut next = Vec::with_capacity(growth.len() + 1);
        next.push(growth[0] * hazard);
        for (run, weight) in growth.iter().enumerate() {
            let mean = 0.0;
            let predictive_var = obs_var * (1.0 + 1.0 / (run as f64 + 1.0));
            let density = (-0.5 * (observation - mean).powi(2) / predictive_var).exp()
                / (2.0 * std::f64::consts::PI * predictive_var).sqrt();
            next[0] += *weight * (1.0 - hazard) * density;
            if run + 1 < growth.len() + 1 {
                if next.len() <= run + 1 {
                    next.push(0.0);
                }
                next[run + 1] += *weight * density;
            }
        }
        let sum: f64 = next.iter().sum();
        if !sum.is_finite() || sum <= 0.0 {
            return None;
        }
        for value in &mut next {
            *value /= sum;
        }
        run_lengths.push(
            next.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(0, |(index, _)| index),
        );
        growth = next;
    }
    Some(run_lengths)
}

/// EGARCH(1,1) conditional log-variance for volatility asymmetry.
#[must_use]
pub fn egarch(
    returns: &[f64],
    window: usize,
    alpha: f64,
    beta: f64,
    gamma: f64,
) -> Option<Vec<Option<f64>>> {
    if window < 10 || returns.len() <= window || beta <= 0.0 || beta >= 1.0 {
        return None;
    }
    let seed = &returns[..window];
    let mean = seed.iter().sum::<f64>() / seed.len() as f64;
    let variance = seed.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / seed.len() as f64;
    if variance <= 0.0 {
        return None;
    }
    let mut log_variance = variance.ln();
    let mut out = vec![None; returns.len()];
    for index in window..returns.len() {
        let standardized = returns[index - 1] / variance.sqrt();
        log_variance = beta * log_variance + alpha * standardized.abs() + gamma * standardized
            - alpha * (2.0 / std::f64::consts::PI).sqrt();
        out[index] = Some(log_variance.exp().sqrt());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_entropy_is_nonnegative_and_bounded() {
        let source: Vec<f64> = (0..80).map(|i| (i % 7) as f64).collect();
        let target: Vec<f64> = (0..80).map(|i| ((i + 1) % 7) as f64).collect();
        let te = transfer_entropy(&source, &target, 4).unwrap();
        assert!(te >= 0.0 && te <= 8.0, "{te}");
        assert!(transfer_entropy(&source[..10], &target[..10], 4).is_none());
    }

    #[test]
    fn hurst_sepersists_and_mean_reverts() {
        let trend: Vec<f64> = (0..256).map(|i| i as f64 * 0.1).collect();
        let alternating: Vec<f64> = (0..256)
            .map(|i| {
                (if i % 2 == 0 { 1.0 } else { -1.0 }) + (i % 5) as f64 * 0.01
            })
            .collect();
        let trend_h = hurst_exponent(&trend, 2, 16).unwrap();
        let alternating_h = hurst_exponent(&alternating, 2, 16).unwrap();
        assert!(trend_h > alternating_h, "{trend_h} vs {alternating_h}");
    }

    #[test]
    fn wavelet_energy_and_recurrence_are_finite() {
        let values: Vec<f64> = (0..128).map(|i| (i as f64 * 0.05).sin()).collect();
        assert!(wavelet_energy(&values, 2).unwrap() > 0.0);
        let rate = recurrence_rate(&values, 2, 1, 0.5).unwrap();
        assert!((0.0..=1.0).contains(&rate));
    }

    #[test]
    fn particle_filter_is_reproducible() {
        let observations: Vec<f64> = (0..40).map(|i| 100.0 + i as f64 * 0.1).collect();
        let a = particle_filter(&observations, 32, 0.1, 1.0, 7).unwrap();
        let b = particle_filter(&observations, 32, 0.1, 1.0, 7).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), observations.len());
    }

    #[test]
    fn bocpd_resets_on_a_clear_regime_shift() {
        let mut observations = vec![100.0; 40];
        observations.extend(vec![110.0; 40]);
        let runs = bocpd(&observations, 0.05, 1.0).unwrap();
        assert!(runs.last().copied().unwrap_or(0) < 15, "{runs:?}");
    }

    #[test]
    fn egarch_returns_positive_conditional_volatility() {
        let returns: Vec<f64> = (0..80)
            .map(|i| if i % 2 == 0 { 0.01 } else { -0.012 })
            .collect();
        let series = egarch(&returns, 30, 0.1, 0.85, -0.05).unwrap();
        assert!(series[30..]
            .iter()
            .all(|v| v.is_some_and(|v| v > 0.0 && v.is_finite())));
    }
}
