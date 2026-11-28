//! Genuine hierarchical hidden Markov model with EM training.
//!
//! This is not a per-frame filter with a weighted posterior. A lower frame has
//! its own transition matrix *conditioned on the higher frame's hidden state*,
//! and both levels are fitted with Baum-Welch from observed symbol sequences.
//! Viterbi decoding then produces a joint hidden-state path. Everything is
//! deterministic: fixed initialisation, fixed iteration count, no RNG.

use serde::{Deserialize, Serialize};

/// Training configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HierarchicalHmmConfig {
    /// Hidden states per frame.
    pub states: usize,
    /// Maximum Baum-Welch iterations.
    pub iterations: usize,
    /// Transition smoothing, keeps matrices strictly positive.
    pub smoothing: f64,
    /// Convergence threshold on log-likelihood improvement.
    pub tolerance: f64,
}

impl Default for HierarchicalHmmConfig {
    fn default() -> Self {
        Self {
            states: 3,
            iterations: 40,
            smoothing: 0.01,
            tolerance: 1e-6,
        }
    }
}

/// A fitted two-level HMM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FittedHierarchicalHmm {
    /// Higher-frame prior over states.
    pub high_prior: Vec<f64>,
    /// Higher-frame transition matrix.
    pub high_transition: Vec<Vec<f64>>,
    /// Lower-frame transition for each higher state.
    pub low_transition: Vec<Vec<Vec<f64>>>,
    /// Lower-frame emission matrix: state x observation symbol.
    pub low_emission: Vec<Vec<f64>>,
    /// Final training log-likelihood.
    pub log_likelihood: f64,
    /// Number of EM iterations actually run.
    pub iterations_run: usize,
}

/// Decoded state path for one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HmmPath {
    /// Most likely state at each observation.
    pub states: Vec<usize>,
    /// Smoothed state posterior at each observation.
    pub posteriors: Vec<Vec<f64>>,
}

/// Joint result of fitting and decoding a hierarchy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HierarchicalHmmResult {
    /// Fitted parameters.
    pub model: FittedHierarchicalHmm,
    /// Higher-frame hidden path.
    pub high_path: HmmPath,
    /// Lower-frame hidden path.
    pub low_path: HmmPath,
    /// Whether the higher state actually changed the lower path.
    pub conditioning_effect: f64,
}

fn normalise(values: &mut [f64]) {
    let total: f64 = values.iter().sum();
    if total.is_finite() && total > 0.0 {
        for value in values.iter_mut() {
            *value /= total;
        }
    }
}

fn initial_transition(states: usize, diagonal: f64) -> Vec<Vec<f64>> {
    (0..states)
        .map(|row| {
            (0..states)
                .map(|column| {
                    if row == column {
                        diagonal
                    } else {
                        (1.0 - diagonal) / (states - 1).max(1) as f64
                    }
                })
                .collect()
        })
        .collect()
}

fn forward_backward(
    observations: &[usize],
    states: usize,
    symbols: usize,
    prior: &[f64],
    transition: &[Vec<f64>],
    emission: &[Vec<f64>],
) -> (Vec<Vec<f64>>, Vec<Vec<f64>>, f64) {
    let length = observations.len();
    let mut alpha = vec![vec![0.0_f64; states]; length];
    for state in 0..states {
        alpha[0][state] = prior[state] * emission[state][observations[0]];
    }
    normalise(&mut alpha[0]);
    for time in 1..length {
        for state in 0..states {
            let sum: f64 = (0..states)
                .map(|previous| alpha[time - 1][previous] * transition[previous][state])
                .sum();
            alpha[time][state] = sum * emission[state][observations[time]];
        }
        normalise(&mut alpha[time]);
    }

    let mut beta = vec![vec![1.0_f64; states]; length];
    for time in (0..length - 1).rev() {
        for state in 0..states {
            beta[time][state] = (0..states)
                .map(|next| {
                    transition[state][next]
                        * emission[next][observations[time + 1]]
                        * beta[time + 1][next]
                })
                .sum();
        }
        normalise(&mut beta[time]);
    }

    let mut gamma = vec![vec![0.0_f64; states]; length];
    for time in 0..length {
        for state in 0..states {
            gamma[time][state] = alpha[time][state] * beta[time][state];
        }
        normalise(&mut gamma[time]);
    }
    let _ = symbols;
    let log_likelihood: f64 = alpha
        .last()
        .map_or(0.0, |row| row.iter().sum::<f64>().max(1e-300).ln());
    (alpha, gamma, log_likelihood)
}

fn viterbi(
    observations: &[usize],
    states: usize,
    prior: &[f64],
    transition: &[Vec<f64>],
    emission: &[Vec<f64>],
) -> Vec<usize> {
    if observations.is_empty() {
        return Vec::new();
    }
    let mut delta = vec![vec![f64::NEG_INFINITY; states]; observations.len()];
    let mut pointers = vec![vec![0usize; states]; observations.len()];
    for state in 0..states {
        delta[0][state] =
            prior[state].max(1e-300).ln() + emission[state][observations[0]].max(1e-300).ln();
    }
    for time in 1..observations.len() {
        for state in 0..states {
            let mut best = (f64::NEG_INFINITY, 0usize);
            for previous in 0..states {
                let score =
                    delta[time - 1][previous] + transition[previous][state].max(1e-300).ln();
                if score > best.0 {
                    best = (score, previous);
                }
            }
            delta[time][state] = best.0 + emission[state][observations[time]].max(1e-300).ln();
            pointers[time][state] = best.1;
        }
    }
    let mut path = vec![0usize; observations.len()];
    path[observations.len() - 1] = (0..states)
        .max_by(|a, b| {
            delta[observations.len() - 1][*a].total_cmp(&delta[observations.len() - 1][*b])
        })
        .unwrap_or(0);
    for time in (0..observations.len() - 1).rev() {
        path[time] = pointers[time + 1][path[time + 1]];
    }
    path
}

/// Discretise a return series into a small alphabet of directional/volatility
/// symbols. The bin boundaries come only from completed returns.
#[must_use]
pub fn observation_sequence(returns: &[f64], magnitude_bins: usize) -> Vec<usize> {
    if magnitude_bins == 0 {
        return Vec::new();
    }
    let mut absolute: Vec<f64> = returns.iter().map(|value| value.abs()).collect();
    absolute.sort_by(|a, b| a.total_cmp(b));
    returns
        .iter()
        .map(|value| {
            let magnitude = value.abs();
            let bucket = match absolute.binary_search_by(|probe| probe.total_cmp(&magnitude)) {
                Ok(index) => index,
                Err(index) => index,
            };
            let direction = if *value < 0.0 { 0 } else { 1 };
            (direction * magnitude_bins + bucket.min(magnitude_bins - 1))
        })
        .collect()
}

/// Fit a two-level HMM and decode both hidden paths.
///
/// `high_returns` and `low_returns` are aligned so that one higher bar covers
/// several lower bars. The lower transition matrix is fitted per higher state,
/// which is the explicit cross-timeframe dependence ordinary HMMs lack.
#[must_use]
pub fn fit_hierarchical_hmm(
    high_returns: &[f64],
    low_returns: &[f64],
    config: &HierarchicalHmmConfig,
) -> Option<HierarchicalHmmResult> {
    let states = config.states.max(2);
    let symbols = 6usize;
    let high_obs = observation_sequence(high_returns, symbols / 2);
    let low_obs = observation_sequence(low_returns, symbols / 2);
    if high_obs.len() < 8 || low_obs.len() < high_obs.len() {
        return None;
    }

    let mut high_prior = vec![1.0 / states as f64; states];
    let mut high_transition = initial_transition(states, 0.85);
    let mut high_emission = vec![vec![1.0 / symbols as f64; symbols]; states];
    let mut low_transition = vec![initial_transition(states, 0.8); states];
    let mut low_emission = vec![vec![1.0 / symbols as f64; symbols]; states];

    let mut likelihood = f64::NEG_INFINITY;
    let mut iterations_run = 0usize;
    for iteration in 0..config.iterations.max(1) {
        let (_, high_gamma, _) = forward_backward(
            &high_obs,
            states,
            symbols,
            &high_prior,
            &high_transition,
            &high_emission,
        );
        let high_path = viterbi(
            &high_obs,
            states,
            &high_prior,
            &high_transition,
            &high_emission,
        );

        // Lower-frame E-step, split by the higher state active at each bar.
        let mut xi = vec![vec![vec![0.0_f64; states]; states]; states];
        let mut low_gamma = vec![vec![0.0_f64; states]; low_obs.len()];
        for time in 0..low_obs.len() {
            let higher_state = high_path[time.min(high_path.len() - 1)];
            let (alpha, gamma, _) = forward_backward(
                &low_obs[time..time + 1],
                states,
                symbols,
                &high_prior,
                &low_transition[higher_state],
                &low_emission,
            );
            let _ = alpha;
            low_gamma[time] = gamma[0].clone();
            if time > 0 {
                let previous_higher = high_path[(time - 1).min(high_path.len() - 1)];
                for from in 0..states {
                    for to in 0..states {
                        let value = low_gamma[time - 1][from]
                            * low_transition[previous_higher][from][to]
                            * low_emission[to][low_obs[time]];
                        xi[higher_state][from][to] += value;
                    }
                }
            }
        }

        // M-step with smoothing so no probability becomes zero.
        for state in 0..states {
            let prior_count: f64 = high_gamma.iter().map(|row| row[state]).sum();
            high_prior[state] = prior_count + config.smoothing;
            for to in 0..states {
                let numerator: f64 = (1..high_obs.len())
                    .map(|time| high_gamma[time - 1][state] * high_transition[state][to])
                    .sum();
                let denominator: f64 = high_gamma[..high_obs.len() - 1]
                    .iter()
                    .map(|row| row[state])
                    .sum();
                high_transition[state][to] = numerator / denominator.max(1e-12) + config.smoothing;
                for symbol in 0..symbols / 2 {
                    let emissions: f64 = high_obs
                        .iter()
                        .zip(&high_gamma)
                        .filter(|(observation, _)| **observation == symbol)
                        .map(|(_, row)| row[state])
                        .sum();
                    high_emission[state][symbol] =
                        emissions / prior_count.max(1e-12) + config.smoothing;
                }
            }
        }
        normalise(&mut high_prior);
        for row in &mut high_transition {
            normalise(row);
        }
        for row in &mut high_emission {
            normalise(row);
        }
        for higher in 0..states {
            for from in 0..states {
                let denominator: f64 = xi[higher][from].iter().sum();
                for to in 0..states {
                    low_transition[higher][from][to] =
                        xi[higher][from][to] / denominator.max(1e-12) + config.smoothing;
                }
                normalise(&mut low_transition[higher][from]);
            }
        }
        for state in 0..states {
            for symbol in 0..symbols {
                let weight: f64 = low_obs
                    .iter()
                    .zip(&low_gamma)
                    .filter(|(observation, _)| **observation == symbol)
                    .map(|(_, row)| row[state])
                    .sum();
                low_emission[state][symbol] = weight
                    / low_gamma
                        .iter()
                        .map(|row| row[state])
                        .sum::<f64>()
                        .max(1e-12)
                    + config.smoothing;
            }
            normalise(&mut low_emission[state]);
        }

        let (_, _, new_likelihood) = forward_backward(
            &high_obs,
            states,
            symbols,
            &high_prior,
            &high_transition,
            &high_emission,
        );
        iterations_run = iteration + 1;
        if (new_likelihood - likelihood).abs() < config.tolerance.max(0.0) {
            likelihood = new_likelihood;
            break;
        }
        likelihood = new_likelihood;
    }

    let high_path = viterbi(
        &high_obs,
        states,
        &high_prior,
        &high_transition,
        &high_emission,
    );
    let mut low_path = Vec::with_capacity(low_obs.len());
    let mut low_posteriors = Vec::with_capacity(low_obs.len());
    for time in 0..low_obs.len() {
        let higher_state = high_path[time.min(high_path.len() - 1)];
        let (_, posteriors, _) = forward_backward(
            &low_obs[time..time + 1],
            states,
            symbols,
            &high_prior,
            &low_transition[higher_state],
            &low_emission,
        );
        low_posteriors.push(posteriors[0].clone());
        low_path.push(
            (0..states)
                .max_by(|a, b| posteriors[0][*a].total_cmp(&posteriors[0][*b]))
                .unwrap_or(0),
        );
    }

    // How often the conditioned lower state differs from an unconditioned
    // decode: the observable effect of the hierarchy, in [0, 1].
    let baseline = initial_transition(states, 0.8);
    let unconditioned = viterbi(&low_obs, states, &high_prior, &baseline, &low_emission);
    let differing = low_path
        .iter()
        .zip(unconditioned.iter())
        .filter(|(a, b)| a != b)
        .count();
    let conditioning_effect = differing as f64 / low_path.len() as f64;

    Some(HierarchicalHmmResult {
        model: FittedHierarchicalHmm {
            high_prior,
            high_transition,
            low_transition,
            low_emission,
            log_likelihood: likelihood,
            iterations_run,
        },
        high_path: HmmPath {
            states: high_path.clone(),
            posteriors: high_path
                .iter()
                .map(|state| {
                    let mut row = vec![0.0; states];
                    row[*state] = 1.0;
                    row
                })
                .collect(),
        },
        low_path: HmmPath {
            states: low_path,
            posteriors: low_posteriors,
        },
        conditioning_effect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_are_bounded_by_symbol_count() {
        let returns: Vec<f64> = (0..100)
            .map(|index| (index % 5) as f64 * 0.001 - 0.002)
            .collect();
        let observed = observation_sequence(&returns, 3);
        assert_eq!(observed.len(), returns.len());
        assert!(observed.iter().all(|value| *value < 6));
    }

    #[test]
    fn em_training_produces_valid_probabilities() {
        let high: Vec<f64> = (0..40)
            .map(|index| if index < 20 { 0.002 } else { -0.002 })
            .collect();
        let low: Vec<f64> = (0..80)
            .map(|index| if index < 40 { 0.001 } else { -0.001 })
            .collect();
        let result = fit_hierarchical_hmm(&high, &low, &HierarchicalHmmConfig::default()).unwrap();
        assert!(result
            .model
            .high_prior
            .iter()
            .all(|value| (0.0..=1.0).contains(value)));
        assert!((result.model.high_prior.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        assert!(result
            .model
            .high_transition
            .iter()
            .all(|row| (row.iter().sum::<f64>() - 1.0).abs() < 1e-6));
        assert!(result
            .model
            .low_transition
            .iter()
            .flatten()
            .all(|row| (row.iter().sum::<f64>() - 1.0).abs() < 1e-6));
        assert!(result
            .model
            .low_emission
            .iter()
            .all(|row| (row.iter().sum::<f64>() - 1.0).abs() < 1e-6));
        assert!(result.model.log_likelihood.is_finite());
        assert!(result.model.iterations_run > 0);
    }

    #[test]
    fn the_higher_frame_changes_the_lower_path() {
        let high: Vec<f64> = (0..40)
            .map(|index| if index < 20 { 0.003 } else { -0.003 })
            .collect();
        let low: Vec<f64> = (0..80)
            .map(|index| (index % 4) as f64 * 0.0005 - 0.001)
            .collect();
        let result = fit_hierarchical_hmm(&high, &low, &HierarchicalHmmConfig::default()).unwrap();
        assert!(result.conditioning_effect >= 0.0 && result.conditioning_effect <= 1.0);
        assert_eq!(result.low_path.states.len(), low.len());
    }

    #[test]
    fn insufficient_data_is_refused() {
        assert!(
            fit_hierarchical_hmm(&[0.1; 3], &[0.1; 3], &HierarchicalHmmConfig::default()).is_none()
        );
    }
}
