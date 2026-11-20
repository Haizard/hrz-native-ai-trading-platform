//! Transition modelling: fixed, time-varying (TVTP) and higher-order.

use std::collections::HashMap;

/// A row-stochastic transition matrix.
pub type Matrix = Vec<Vec<f64>>;

/// Rescale every row to sum to one. Rows that cannot be normalized become a
/// uniform distribution rather than a `NaN` row.
pub fn normalize_rows(mut matrix: Matrix) -> Matrix {
    for row in &mut matrix {
        let total: f64 = row.iter().filter(|v| v.is_finite() && **v > 0.0).sum();
        if total.is_finite() && total > 0.0 {
            for value in row.iter_mut() {
                *value = if value.is_finite() && *value > 0.0 {
                    *value / total
                } else {
                    0.0
                };
            }
        } else {
            let uniform = 1.0 / row.len().max(1) as f64;
            for value in row.iter_mut() {
                *value = uniform;
            }
        }
    }
    matrix
}

/// A fixed matrix that keeps `persistence` in the current state and spreads
/// the remainder evenly over the others.
pub fn fixed(count: usize, persistence: f64) -> Matrix {
    if count == 0 {
        return Vec::new();
    }
    let stay = persistence.clamp(0.0, 1.0);
    let other = (1.0 - stay) / (count - 1).max(1) as f64;
    (0..count)
        .map(|i| {
            (0..count)
                .map(|j| if i == j { stay } else { other })
                .collect()
        })
        .collect()
}

/// Time-varying transition probabilities.
///
/// Each row j of `base` is reweighted by `exp(beta_j · features)`, which lets
/// volume, ATR, delta and spread change how likely a transition is without
/// replacing the whole matrix on every tick.
pub fn tvtp(base: &Matrix, features: &[f64], betas: &[Vec<f64>]) -> Result<Matrix, String> {
    if base.is_empty() || base[0].is_empty() {
        return Ok(Vec::new());
    }
    let states = base.len();
    if betas.len() != states {
        return Err(format!(
            "TVTP needs {states} beta rows, got {}",
            betas.len()
        ));
    }
    let mut out = Vec::with_capacity(states);
    for (i, row) in base.iter().enumerate() {
        if row.len() != states || betas.iter().any(|beta| beta.len() != features.len()) {
            let _ = i;
            return Err("TVTP matrix and feature dimensions disagree".into());
        }
        let mut weighted = Vec::with_capacity(states);
        for (j, probability) in row.iter().enumerate() {
            let base = if *probability > f64::MIN_POSITIVE {
                probability.ln()
            } else {
                f64::MIN_POSITIVE.ln()
            };
            // Betas belong to the DESTINATION state: a feature makes a
            // particular next regime more or less likely, independent of
            // which state we are leaving.
            let shift: f64 = betas[j]
                .iter()
                .zip(features.iter())
                .map(|(beta, feature)| beta * feature)
                .sum();
            weighted.push((base + shift).exp());
        }
        let total: f64 = weighted.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return Err("TVTP produced a non-finite row".into());
        }
        out.push(weighted.iter_mut().map(|value| *value / total).collect());
    }
    Ok(out)
}

/// Higher-order, context-dependent transition estimate.
///
/// `context` is the recent state sequence oldest-first; only the last `order`
/// states form the key. Unseen contexts fall back to the first-order counts.
pub fn higher_order(
    counts: &HashMap<Vec<usize>, usize>,
    context: &[usize],
    order: usize,
    states: usize,
) -> Vec<f64> {
    if states == 0 {
        return Vec::new();
    }
    let start = context.len().saturating_sub(order.max(1));
    let key = context[start..].to_vec();
    let base_key = key.last().map_or_else(Vec::new, |state| vec![*state]);
    let mut row = vec![0usize; states];
    for (candidate, count) in counts {
        if candidate == &key || (candidate.len() == 1 && candidate == &base_key) {
            if let Some(&next) = candidate.last() {
                if next < states {
                    row[next] += count;
                }
            }
        }
    }
    let total: usize = row.iter().sum();
    if total == 0 {
        return vec![1.0 / states as f64; states];
    }
    row.iter()
        .map(|count| *count as f64 / total as f64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_matrix_is_row_stochastic() {
        let matrix = fixed(4, 0.7);
        assert_eq!(matrix.len(), 4);
        for row in matrix {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn tvtp_shifts_probability_toward_features() {
        let base = fixed(2, 0.5);
        let features = [0.9, -0.4];
        let betas = vec![vec![0.0; 2], vec![3.0, 0.0]];
        let shifted = tvtp(&base, &features, &betas).expect("a valid TVTP matrix");
        assert!(shifted[0][1] > base[0][1]);
        for row in shifted {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn higher_order_uses_the_recent_context() {
        let mut counts = HashMap::new();
        counts.insert(vec![0, 1], 9); // after 0,1 the next state is 1
        counts.insert(vec![1], 4); // first-order fallback
        let result = higher_order(&counts, &[0, 0, 1], 2, 2);
        assert!(result[1] > result[0]);
    }
}
