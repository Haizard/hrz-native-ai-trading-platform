//! Coupled hidden-state models across timeframes.
//!
//! A conventional HMM per timeframe is not hierarchical. This module makes the
//! dependence explicit: each finer frame's transition prior is shifted toward
//! the coarser frame's filtered state, and a switching Kalman filter lets the
//! higher state set the finer frame's process noise. Everything is closed-form
//! and deterministic; no parameters are fitted to the data.

use serde::{Deserialize, Serialize};

/// Three latent states: down, range and up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatentState {
    /// Negative-drift state.
    Down,
    /// Low-drift state.
    Range,
    /// Positive-drift state.
    Up,
}

impl LatentState {
    /// Stable index in a three-state model.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Down => 0,
            Self::Range => 1,
            Self::Up => 2,
        }
    }

    /// Reconstruct a state from its index.
    #[must_use]
    pub const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Down,
            1 => Self::Range,
            _ => Self::Up,
        }
    }
}

/// Configuration for the coupled model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoupledStateConfig {
    /// Probability of remaining in the same state without higher-frame input.
    pub persistence: f64,
    /// How strongly the higher frame biases the finer frame's transition.
    pub higher_influence: f64,
    /// State means as multiples of the frame's return standard deviation.
    pub state_separation: f64,
}

impl Default for CoupledStateConfig {
    fn default() -> Self {
        Self {
            persistence: 0.9,
            higher_influence: 0.35,
            state_separation: 1.0,
        }
    }
}

/// Filtered evidence for one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameStates {
    /// Filtered state probability at each completed return.
    pub probabilities: Vec<[f64; 3]>,
    /// Most likely state at the newest return.
    pub current_state: LatentState,
    /// Posterior of the newest state.
    pub current_probability: f64,
}

/// Result of a coupled multi-frame state read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoupledStateResult {
    /// Per-frame evidence, coarsest first.
    pub frames: Vec<FrameStates>,
    /// Mean shift in the finest frame's posterior caused by higher frames.
    pub conditioning_effect: f64,
    /// Explicit limitation.
    pub limitation: String,
}

fn log_sum_exp(values: &[f64]) -> f64 {
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !max.is_finite() {
        return f64::NEG_INFINITY;
    }
    max + values
        .iter()
        .map(|value| (value - max).exp())
        .sum::<f64>()
        .ln()
}

fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64)
        .sqrt()
}

/// Gaussian log-emission for one return under a state with mean `mu` and sigma.
fn emission(return_value: f64, mu: f64, sigma: f64) -> f64 {
    let sigma = sigma.max(1e-8);
    -0.5 * ((return_value - mu) / sigma).powi(2)
        - sigma.ln()
        - 0.5 * (2.0 * std::f64::consts::PI).ln()
}

/// Filter one frame, optionally biased toward a higher frame's state.
fn filter_frame(
    returns: &[f64],
    config: &CoupledStateConfig,
    higher_state: Option<LatentState>,
) -> Option<FrameStates> {
    if returns.len() < 8 || !config.persistence.is_finite() {
        return None;
    }
    let sigma = stddev(returns).max(1e-6);
    let means = [
        -config.state_separation * sigma,
        0.0,
        config.state_separation * sigma,
    ];
    let stay = config.persistence.clamp(0.5, 0.99);
    let spread = (1.0 - stay) / 2.0;
    let mut log_alpha = [0.0_f64; 3];
    let mut probabilities = Vec::with_capacity(returns.len());
    for return_value in returns {
        let mut next = [0.0_f64; 3];
        for (to, value) in next.iter_mut().enumerate() {
            let mut terms = Vec::with_capacity(3);
            for (from, alpha) in log_alpha.iter().enumerate() {
                let mut transition = if from == to { stay } else { spread };
                if let Some(higher) = higher_state {
                    let target = higher.index();
                    transition = if to == target {
                        (transition + config.higher_influence * stay).min(1.0)
                    } else {
                        (transition * (1.0 - config.higher_influence)).max(1e-6)
                    };
                }
                terms.push(alpha + transition.ln() + emission(*return_value, means[to], sigma));
            }
            *value = log_sum_exp(&terms);
        }
        let normalizer = log_sum_exp(&next);
        if !normalizer.is_finite() {
            return None;
        }
        let mut posterior = [0.0_f64; 3];
        for (slot, value) in posterior.iter_mut().zip(next.iter()) {
            *slot = (value - normalizer).exp();
        }
        log_alpha = next.map(|value| value - normalizer);
        probabilities.push(posterior);
    }
    let current_probability = probabilities
        .last()?
        .iter()
        .copied()
        .fold(0.0_f64, f64::max);
    let current_index = probabilities
        .last()?
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(1, |(index, _)| index);
    Some(FrameStates {
        probabilities,
        current_state: LatentState::from_index(current_index),
        current_probability,
    })
}

/// Run the coupled model over a coarsest-to-finest ladder of return series.
#[must_use]
pub fn analyze_coupled_states(
    series: &[Vec<f64>],
    config: &CoupledStateConfig,
) -> Option<CoupledStateResult> {
    if series.is_empty() {
        return None;
    }
    let mut frames = Vec::with_capacity(series.len());
    let mut higher = None;
    for returns in series {
        let filtered = filter_frame(returns, config, higher)?;
        higher = Some(filtered.current_state);
        frames.push(filtered);
    }

    // Compare the finest frame's newest posterior with what an uncoupled filter
    // would have said. The mean absolute shift measures how much the hierarchy
    // actually changed the conclusion.
    let finest = series.last()?;
    let uncoupled = filter_frame(finest, config, None)?;
    let coupled = frames.last()?;
    let shift: f64 = coupled
        .probabilities
        .last()?
        .iter()
        .zip(uncoupled.probabilities.last()?)
        .map(|(a, b)| (a - b).abs())
        .sum::<f64>()
        / 2.0;

    Some(CoupledStateResult {
        frames,
        conditioning_effect: shift.clamp(0.0, 1.0),
        limitation: "closed-form coupled filter; not a fitted hierarchical HMM or switching state-space model".to_string(),
    })
}

/// Switching Kalman filter: the higher state selects the finer process noise.
#[must_use]
pub fn switching_kalman(
    observations: &[f64],
    states: &[LatentState],
    measurement_noise: f64,
    quiet_process_noise: f64,
    active_process_noise: f64,
) -> Option<Vec<f64>> {
    if observations.is_empty() || observations.len() != states.len() || measurement_noise <= 0.0 {
        return None;
    }
    let mut level = observations[0];
    let mut variance = measurement_noise;
    let mut out = Vec::with_capacity(observations.len());
    for (index, observation) in observations.iter().enumerate() {
        let process_noise = if states[index] == LatentState::Range {
            quiet_process_noise
        } else {
            active_process_noise
        };
        variance += process_noise;
        let gain = variance / (variance + measurement_noise);
        level += gain * (observation - level);
        variance *= 1.0 - gain;
        out.push(level);
    }
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn returns(up: bool, count: usize) -> Vec<f64> {
        (0..count)
            .map(|i| if up { 0.002 } else { -0.002 } + (i % 3) as f64 * 0.0001)
            .collect()
    }

    #[test]
    fn probabilities_are_normalised_and_states_are_stable() {
        let result = analyze_coupled_states(
            &[returns(true, 60), returns(true, 60)],
            &CoupledStateConfig::default(),
        )
        .unwrap();
        assert_eq!(result.frames.len(), 2);
        assert!(result
            .frames
            .iter()
            .all(|frame| frame.probabilities.iter().all(|p| {
                p.iter().all(|value| (0.0..=1.0).contains(value))
                    && (p.iter().sum::<f64>() - 1.0).abs() < 1e-9
            })));
        assert!(result
            .frames
            .iter()
            .all(|frame| frame.current_state == LatentState::Up));
        assert!(result.limitation.contains("not a fitted"));
    }

    #[test]
    fn a_higher_frame_changes_the_finer_posterior() {
        let result = analyze_coupled_states(
            &[returns(true, 60), returns(false, 60)],
            &CoupledStateConfig::default(),
        )
        .unwrap();
        assert!(result.conditioning_effect > 0.0);
        assert!(result.conditioning_effect <= 1.0);
    }

    #[test]
    fn too_little_data_is_refused() {
        assert!(analyze_coupled_states(&[vec![0.01; 3]], &CoupledStateConfig::default()).is_none());
        assert!(analyze_coupled_states(&[], &CoupledStateConfig::default()).is_none());
    }

    #[test]
    fn switching_kalman_reacts_more_in_active_states() {
        let observations: Vec<f64> = (0..30)
            .map(|index| 100.0 + if index < 15 { 0.0 } else { 2.0 })
            .collect();
        let quiet = vec![LatentState::Range; 30];
        let mut active = vec![LatentState::Range; 30];
        active[15] = LatentState::Up;
        let a = switching_kalman(&observations, &quiet, 1.0, 0.001, 5.0).unwrap();
        let b = switching_kalman(&observations, &active, 1.0, 0.001, 5.0).unwrap();
        assert_eq!(a.len(), observations.len());
        assert_ne!(a[20], b[20]);
        assert!(switching_kalman(&observations, &quiet[..5], 1.0, 0.001, 5.0).is_none());
    }
}
