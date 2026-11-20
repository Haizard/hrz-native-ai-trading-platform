//! Markov decision processes for strategy selection.

use crate::transition::Matrix;

/// Value iteration over a finite MDP.
///
/// `transitions[action][state][next]` and `rewards[action][state]`. The policy
/// proposes an action; it never authorizes execution — the risk gate stays
/// independent.
pub fn value_iteration(
    transitions: &[Matrix],
    rewards: &[Vec<f64>],
    gamma: f64,
    epsilon: f64,
    max_iterations: usize,
) -> Result<(Vec<f64>, Vec<usize>), String> {
    if transitions.is_empty() || rewards.is_empty() {
        return Err("an MDP needs at least one action".into());
    }
    let actions = transitions.len();
    if rewards.len() != actions {
        return Err("rewards and transitions must have the same action count".into());
    }
    let states = transitions[0].len();
    if states == 0 {
        return Err("an MDP needs at least one state".into());
    }
    if transitions
        .iter()
        .any(|matrix| matrix.len() != states || matrix.iter().any(|row| row.len() != states))
        || rewards.iter().any(|row| row.len() != states)
    {
        return Err("MDP dimensions disagree".into());
    }

    let mut values = vec![0.0f64; states];
    for _ in 0..max_iterations {
        let mut next = vec![f64::NEG_INFINITY; states];
        for state in 0..states {
            for action in 0..actions {
                let expected: f64 = (0..states)
                    .map(|next_state| transitions[action][state][next_state] * values[next_state])
                    .sum();
                let candidate = rewards[action][state] + gamma * expected;
                if candidate > next[state] {
                    next[state] = candidate;
                }
            }
        }
        let delta = next
            .iter()
            .zip(values.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        values = next;
        if delta < epsilon {
            break;
        }
    }

    let mut policy = Vec::with_capacity(states);
    for state in 0..states {
        let mut best_action = 0;
        let mut best = f64::NEG_INFINITY;
        for action in 0..actions {
            let expected: f64 = (0..states)
                .map(|next_state| transitions[action][state][next_state] * values[next_state])
                .sum();
            let score = rewards[action][state] + gamma * expected;
            if score > best {
                best = score;
                best_action = action;
            }
        }
        policy.push(best_action);
    }
    Ok((values, policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_positive_reward_action_wins_in_every_state() {
        let identity = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let transitions = vec![identity.clone(), identity]; // wait / trade
        let rewards = vec![vec![0.0, 0.0], vec![1.0, 0.5]];
        let (values, policy) = value_iteration(&transitions, &rewards, 0.9, 1e-9, 100).unwrap();
        assert_eq!(policy, vec![1, 1]);
        assert!(values[0] > 9.0);
    }

    #[test]
    fn mismatched_dimensions_are_refused() {
        let transitions = vec![vec![vec![1.0, 0.0]]];
        let rewards = vec![vec![1.0]];
        assert!(value_iteration(&transitions, &rewards, 0.9, 1e-6, 10).is_err());
    }
}
