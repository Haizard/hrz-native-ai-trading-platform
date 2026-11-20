//! GARCH, EGARCH and regime-switching volatility.

use std::collections::BTreeMap;

use crate::state::Regime;

/// GARCH(1,1) conditional variance: `σ²_t = ω + α ε²_{t-1} + β σ²_{t-1}`.
pub fn garch(returns: &[f64], omega: f64, alpha: f64, beta: f64) -> Vec<f64> {
    let mut variance = returns
        .iter()
        .take(20)
        .filter(|value| value.is_finite())
        .map(|value| value * value)
        .sum::<f64>()
        / 20.0;
    if !variance.is_finite() || variance <= 0.0 {
        variance = 1e-6;
    }
    let mut out = Vec::with_capacity(returns.len());
    for (index, return_value) in returns.iter().enumerate() {
        let shock = if index > 0 {
            returns[index - 1] * returns[index - 1]
        } else {
            return_value * return_value
        };
        variance = omega + alpha * shock + beta * variance;
        if !variance.is_finite() || variance <= 0.0 {
            variance = 1e-6;
        }
        out.push(variance);
    }
    out
}

/// EGARCH(1,1): asymmetric response to negative shocks via `gamma`.
pub fn egarch(returns: &[f64], omega: f64, alpha: f64, beta: f64, gamma: f64) -> Vec<f64> {
    let mut log_variance = 0.0f64;
    let mut out = Vec::with_capacity(returns.len());
    for (index, return_value) in returns.iter().enumerate() {
        let previous = if index > 0 {
            returns[index - 1]
        } else {
            *return_value
        };
        let normalized = previous / 1e-4;
        let shock = alpha * normalized.abs() + gamma * normalized;
        log_variance = omega + beta * log_variance + shock;
        out.push(log_variance.exp());
    }
    out
}

/// Markov-switching GARCH: one GARCH parameter triple per regime.
pub fn ms_garch(
    returns: &[f64],
    regimes: &[Regime],
    parameters: &BTreeMap<Regime, (f64, f64, f64)>,
) -> Vec<f64> {
    let mut variances: BTreeMap<Regime, f64> = BTreeMap::new();
    let mut out = Vec::with_capacity(returns.len());
    for (index, return_value) in returns.iter().enumerate() {
        let regime = regimes.get(index).copied().unwrap_or(Regime::Sideways);
        let (omega, alpha, beta) = parameters
            .get(&regime)
            .copied()
            .unwrap_or((1e-6, 0.08, 0.9));
        let variance = variances.entry(regime).or_insert(1e-6);
        let shock = if index > 0 {
            returns[index - 1] * returns[index - 1]
        } else {
            *return_value * *return_value
        };
        *variance = omega + alpha * shock + beta * *variance;
        if !variance.is_finite() || *variance <= 0.0 {
            *variance = 1e-6;
        }
        out.push(*variance);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garch_keeps_variance_positive_and_finite() {
        let returns: Vec<f64> = (0..50)
            .map(|i| if i % 2 == 0 { 0.01 } else { -0.012 })
            .collect();
        let variance = garch(&returns, 1e-6, 0.1, 0.85);
        assert_eq!(variance.len(), 50);
        assert!(variance
            .iter()
            .all(|value| value.is_finite() && *value > 0.0));
    }

    #[test]
    fn egarch_reacts_more_to_negative_shocks() {
        let positive = egarch(&[0.01, 0.0], 0.0, 0.1, 0.9, -0.2);
        let negative = egarch(&[-0.01, 0.0], 0.0, 0.1, 0.9, -0.2);
        assert!(negative[1] > positive[1]);
    }

    #[test]
    fn ms_garch_uses_regime_specific_parameters() {
        let returns = vec![0.02; 6];
        let regimes = vec![Regime::LowVolatility; 3]
            .into_iter()
            .chain(vec![Regime::HighVolatility; 3])
            .collect::<Vec<_>>();
        let mut parameters = BTreeMap::new();
        parameters.insert(Regime::LowVolatility, (1e-8, 0.01, 0.5));
        parameters.insert(Regime::HighVolatility, (1e-4, 0.5, 0.5));
        let variance = ms_garch(&returns, &regimes, &parameters);
        assert!(variance[5] > variance[2] * 10.0);
    }
}
