//! Walk-forward validation of discovered strategy rules.
//!
//! Frequency counting alone is not discovery: a recurring pattern may be noise.
//! This module evaluates candidate rules on a chronologically separated test
//! segment and requires stability across folds before calling a rule promising.
//! Nothing here is promoted automatically.

use serde::{Deserialize, Serialize};

/// A candidate rule described by timeframe and direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRule {
    /// Rule name, e.g. the originating concept.
    pub name: String,
    /// Timeframe the rule fires on.
    pub timeframe: String,
    /// Direction the rule predicts.
    pub direction: String,
}

/// One observed firing of a rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleObservation {
    /// Rule this observation belongs to.
    pub rule: CandidateRule,
    /// Decision instant, unix nanoseconds.
    pub decision_time: i64,
    /// Evidence strength at the decision.
    pub evidence_strength: f64,
    /// Realized forward return, filled offline after the horizon matured.
    pub realized_return: Option<f64>,
    /// Whether the directional call was correct.
    pub hit: Option<bool>,
}

/// Validation outcome for one rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleValidation {
    /// Rule validated.
    pub rule: CandidateRule,
    /// Observations in the test segment.
    pub test_observations: usize,
    /// Directional test observations.
    pub directional_observations: usize,
    /// Hit rate, withheld below the sample gate.
    pub hit_rate: Option<f64>,
    /// Mean realized return of directional calls, with costs charged.
    pub mean_net_return: Option<f64>,
    /// Number of folds where the rule was positive.
    pub positive_folds: usize,
    /// Total folds.
    pub folds: usize,
    /// Whether the rule is stable enough to investigate further.
    pub promising: bool,
    /// Explicit limitation.
    pub limitation: String,
}

/// Validation settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Minimum directional test observations for a hit rate.
    pub min_test_observations: usize,
    /// Round-trip cost in basis points.
    pub round_trip_cost_bps: f64,
    /// Minimum share of positive folds.
    pub min_positive_folds: f64,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            min_test_observations: 30,
            round_trip_cost_bps: 10.0,
            min_positive_folds: 0.6,
        }
    }
}

fn rule_key(rule: &CandidateRule) -> (&str, &str, &str) {
    (&rule.name, &rule.timeframe, &rule.direction)
}

/// Validate discovered rules on a later, chronologically separate segment.
#[must_use]
pub fn validate_discovered_rules(
    observations: &[RuleObservation],
    split_time_ns: i64,
    folds: usize,
    config: &DiscoveryConfig,
) -> Vec<RuleValidation> {
    if observations.is_empty() || folds == 0 {
        return Vec::new();
    }
    let mut groups: std::collections::BTreeMap<(String, String, String), Vec<&RuleObservation>> =
        std::collections::BTreeMap::new();
    for observation in observations {
        groups
            .entry((
                observation.rule.name.clone(),
                observation.rule.timeframe.clone(),
                observation.rule.direction.clone(),
            ))
            .or_default()
            .push(observation);
    }

    groups
        .into_iter()
        .map(|((name, timeframe, direction), mut values)| {
            values.sort_by_key(|value| value.decision_time);
            let split = values.partition_point(|value| value.decision_time < split_time_ns);
            let test = &values[split..];
            let directional: Vec<&RuleObservation> = test
                .iter()
                .copied()
                .filter(|value| value.hit.is_some() && value.realized_return.is_some())
                .collect();
            let hits = directional
                .iter()
                .filter(|value| value.hit == Some(true))
                .count();
            let hit_rate = if directional.len() < config.min_test_observations {
                None
            } else {
                Some(hits as f64 / directional.len() as f64)
            };
            let cost = config.round_trip_cost_bps / 10_000.0;
            let mean_net_return = if directional.is_empty() {
                None
            } else {
                let total: f64 = directional
                    .iter()
                    .filter_map(|value| value.realized_return)
                    .map(|return_value| return_value - cost)
                    .sum();
                Some(total / directional.len() as f64)
            };
            let fold_size = (directional.len() / folds).max(1);
            let mut positive_folds = 0usize;
            for chunk in directional.chunks(fold_size) {
                let net: f64 = chunk
                    .iter()
                    .filter_map(|value| value.realized_return)
                    .map(|return_value| return_value - cost)
                    .sum();
                if net > 0.0 {
                    positive_folds += 1;
                }
            }
            let observed_folds = positive_folds.min(folds);
            let promising = hit_rate.is_some_and(|rate| rate > 0.5)
                && mean_net_return.is_some_and(|net| net > 0.0)
                && observed_folds as f64 / folds as f64 >= config.min_positive_folds;
            RuleValidation {
                rule: CandidateRule {
                    name,
                    timeframe,
                    direction,
                },
                test_observations: test.len(),
                directional_observations: directional.len(),
                hit_rate,
                mean_net_return,
                positive_folds,
                folds,
                promising,
                limitation: "research validation only; no promotion or execution authority"
                    .to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str) -> CandidateRule {
        CandidateRule {
            name: name.to_string(),
            timeframe: "5m".to_string(),
            direction: "Bullish".to_string(),
        }
    }

    fn observations(good: bool) -> Vec<RuleObservation> {
        (0..120)
            .map(|index| {
                let hit = if good { index % 3 != 0 } else { index % 3 == 0 };
                RuleObservation {
                    rule: rule(if good { "good" } else { "bad" }),
                    decision_time: index * 60_000_000_000,
                    evidence_strength: 0.8,
                    realized_return: Some(if hit { 0.02 } else { -0.01 }),
                    hit: Some(hit),
                }
            })
            .collect()
    }

    #[test]
    fn a_stable_rule_is_promising_and_a_bad_one_is_not() {
        let mut values = observations(true);
        values.extend(observations(false));
        let config = DiscoveryConfig {
            min_test_observations: 20,
            round_trip_cost_bps: 5.0,
            min_positive_folds: 0.6,
        };
        let results = validate_discovered_rules(&values, 60 * 60_000_000_000, 4, &config);
        assert_eq!(results.len(), 2);
        let good = results
            .iter()
            .find(|value| value.rule.name == "good")
            .unwrap();
        let bad = results
            .iter()
            .find(|value| value.rule.name == "bad")
            .unwrap();
        assert!(good.promising, "{good:?}");
        assert!(!bad.promising, "{bad:?}");
        assert!(good.hit_rate.unwrap() > 0.5);
        assert!(good.mean_net_return.unwrap() > 0.0);
        assert!(good.limitation.contains("no promotion"));
    }

    #[test]
    fn a_small_test_segment_withholds_the_hit_rate() {
        let config = DiscoveryConfig::default();
        let results = validate_discovered_rules(&observations(true)[..10], 0, 2, &config);
        assert!(results[0].hit_rate.is_none());
        assert!(!results[0].promising);
    }

    #[test]
    fn empty_input_is_not_fabricated() {
        assert!(validate_discovered_rules(&[], 0, 1, &DiscoveryConfig::default()).is_empty());
    }
}
