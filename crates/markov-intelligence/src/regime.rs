//! Markov-switching autoregression and hidden semi-Markov duration modelling.

use std::collections::BTreeMap;

use crate::state::Regime;

/// A regime-specific autoregressive model: `y_t = intercept + Σ phi_k y_{t-k}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArModel {
    pub intercept: f64,
    pub coefficients: Vec<f64>,
}

impl ArModel {
    pub fn predict(&self, history: &[f64]) -> f64 {
        let mut value = self.intercept;
        for (index, coefficient) in self.coefficients.iter().enumerate() {
            if let Some(lag) = history.get(index) {
                value += coefficient * lag;
            }
        }
        value
    }
}

/// Solve a small symmetric linear system by Gaussian elimination with a ridge
/// term, keeping the implementation dependency-free and deterministic.
fn solve(mut matrix: Vec<Vec<f64>>, mut rhs: Vec<f64>, ridge: f64) -> Option<Vec<f64>> {
    let n = rhs.len();
    for index in 0..n {
        matrix[index][index] += ridge;
    }
    for col in 0..n {
        let mut pivot = col;
        for row in col + 1..n {
            if matrix[row][col].abs() > matrix[pivot][col].abs() {
                pivot = row;
            }
        }
        if matrix[pivot][col].abs() < 1e-12 {
            return None;
        }
        matrix.swap(col, pivot);
        rhs.swap(col, pivot);
        let divisor = matrix[col][col];
        for value in matrix[col].iter_mut().skip(col) {
            *value /= divisor;
        }
        rhs[col] /= divisor;
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = matrix[row][col];
            if factor == 0.0 {
                continue;
            }
            for k in col..n {
                matrix[row][k] -= factor * matrix[col][k];
            }
            rhs[row] -= factor * rhs[col];
        }
    }
    Some(rhs)
}

/// Fit one AR model per regime (MS-AR). Regimes with too few observations get
/// a flat zero model instead of an unstable fit.
pub fn ms_ar(returns: &[f64], regimes: &[Regime], order: usize) -> BTreeMap<Regime, ArModel> {
    let mut out = BTreeMap::new();
    for regime in Regime::ALL {
        let mut rows = Vec::new();
        let mut targets = Vec::new();
        for t in order..returns.len() {
            if regimes.get(t) != Some(&regime) {
                continue;
            }
            let mut row = vec![1.0];
            for lag in 1..=order {
                row.push(returns[t - lag]);
            }
            if row.iter().all(|value| value.is_finite()) && returns[t].is_finite() {
                rows.push(row);
                targets.push(returns[t]);
            }
        }
        let width = order + 1;
        let mut normal = vec![vec![0.0f64; width]; width];
        let mut rhs = vec![0.0f64; width];
        for (row, target) in rows.iter().zip(targets.iter()) {
            for (i, left) in row.iter().enumerate() {
                rhs[i] += left * target;
                for (j, right) in row.iter().enumerate() {
                    normal[i][j] += left * right;
                }
            }
        }
        let model = match solve(normal, rhs, 1e-6) {
            Some(coefficients) if rows.len() > width => ArModel {
                intercept: coefficients[0],
                coefficients: coefficients[1..].to_vec(),
            },
            _ => ArModel {
                intercept: 0.0,
                coefficients: vec![0.0; order],
            },
        };
        out.insert(regime, model);
    }
    out
}

/// Empirical duration model for a hidden semi-Markov process.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hsmm {
    durations: BTreeMap<Regime, Vec<usize>>,
}

impl Hsmm {
    /// Observe a regime sequence and record completed run lengths.
    pub fn observe(&mut self, sequence: &[Regime]) {
        if sequence.is_empty() {
            return;
        }
        let mut current = sequence[0];
        let mut length = 1usize;
        for &regime in &sequence[1..] {
            if regime == current {
                length += 1;
            } else {
                self.durations.entry(current).or_default().push(length);
                current = regime;
                length = 1;
            }
        }
        self.durations.entry(current).or_default().push(length);
    }

    /// Mean observed duration in bars.
    pub fn expected_duration(&self, regime: Regime) -> Option<f64> {
        let samples = self.durations.get(&regime)?;
        if samples.is_empty() {
            return None;
        }
        Some(samples.iter().sum::<usize>() as f64 / samples.len() as f64)
    }

    /// Empirical probability that a state of `regime` has already ended by
    /// `elapsed` bars — the hazard behind "young / mature / overdue" states.
    pub fn transition_probability_after(&self, regime: Regime, elapsed: usize) -> f64 {
        let Some(samples) = self.durations.get(&regime) else {
            return 0.0;
        };
        if samples.is_empty() {
            return 0.0;
        }
        let ended = samples
            .iter()
            .filter(|length| **length <= elapsed.max(1))
            .count();
        ended as f64 / samples.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_ar_learns_a_regime_specific_slope() {
        let regimes = vec![Regime::BullTrend; 40];
        let returns: Vec<f64> = (1..=40).map(|i| i as f64 / 100.0).collect();
        let models = ms_ar(&returns, &regimes, 1);
        let bull = &models[&Regime::BullTrend];
        assert!(
            bull.coefficients[0] > 0.9,
            "slope was {}",
            bull.coefficients[0]
        );
        assert_eq!(models[&Regime::BearTrend].coefficients[0], 0.0);
    }

    #[test]
    fn hsmm_records_run_lengths_and_hazard() {
        let mut hsmm = Hsmm::default();
        hsmm.observe(&[
            Regime::BullTrend,
            Regime::BullTrend,
            Regime::BullTrend,
            Regime::Sideways,
            Regime::Sideways,
        ]);
        assert_eq!(hsmm.expected_duration(Regime::BullTrend), Some(3.0));
        assert_eq!(hsmm.expected_duration(Regime::Sideways), Some(2.0));
        assert_eq!(hsmm.transition_probability_after(Regime::BullTrend, 2), 0.0);
        assert_eq!(hsmm.transition_probability_after(Regime::BullTrend, 3), 1.0);
    }
}
