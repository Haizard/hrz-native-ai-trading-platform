//! Strategy Intelligence Framework: per-chart policy and unified opportunities.
//!
//! A strategy can discover an opportunity independently of Core Analytics. This
//! module keeps those conclusions separate and applies an explicit, per-chart
//! policy to decide how higher-timeframe context participates. It never merges
//! one source's answer into the other's and never executes anything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hierarchy::{Direction, OpportunityCandidate};
use crate::multiscale::TimeframeObservation;
use crate::opportunity_scanner::SetupProposal;
use crate::types::Timeframe;

/// How a chart lets higher timeframes participate in a lower-timeframe setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrategyPolicy {
    /// Higher frames provide context only.
    Contextual,
    /// The lower setup must agree with the selected higher direction.
    DirectionalAlignment,
    /// Explicitly allow trading against the higher direction.
    Countertrend,
    /// Require confirmed conditions at multiple levels.
    HierarchicalConfirmation,
}

/// Per-chart intelligence configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartIntelligenceConfig {
    /// Instrument this configuration belongs to.
    pub symbol: String,
    /// Decision timeframe of the chart.
    pub timeframe: Timeframe,
    /// Selected top-down policy.
    pub policy: StrategyPolicy,
    /// Disciplines this chart trusts.
    pub enabled_disciplines: Vec<String>,
    /// Minimum evidence strength accepted from any source.
    pub min_evidence_strength: f64,
}

impl ChartIntelligenceConfig {
    /// Conservative defaults: contextual only, no automatic promotion.
    #[must_use]
    pub fn new(symbol: impl Into<String>, timeframe: Timeframe) -> Self {
        Self {
            symbol: symbol.into(),
            timeframe,
            policy: StrategyPolicy::Contextual,
            enabled_disciplines: Vec::new(),
            min_evidence_strength: 0.5,
        }
    }
}

/// Result of applying a policy to one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    /// Candidate passes and may be shown downstream.
    Accept,
    /// Candidate conflicts with the selected policy.
    Reject,
    /// Candidate needs confirmation at another timeframe.
    RequireConfirmation,
}

/// Apply a chart's top-down policy to a candidate.
#[must_use]
pub fn evaluate_policy(
    config: &ChartIntelligenceConfig,
    higher_direction: Direction,
    candidate_direction: Direction,
    higher_confirmed: bool,
    evidence_strength: f64,
) -> PolicyDecision {
    if candidate_direction == Direction::Neutral || evidence_strength < config.min_evidence_strength
    {
        return PolicyDecision::Reject;
    }
    match config.policy {
        StrategyPolicy::Contextual => PolicyDecision::Accept,
        StrategyPolicy::DirectionalAlignment => {
            if higher_direction != Direction::Neutral && higher_direction == candidate_direction {
                PolicyDecision::Accept
            } else {
                PolicyDecision::Reject
            }
        }
        StrategyPolicy::Countertrend => {
            if higher_direction != Direction::Neutral
                && higher_direction == candidate_direction.opposite()
            {
                PolicyDecision::Accept
            } else {
                PolicyDecision::RequireConfirmation
            }
        }
        StrategyPolicy::HierarchicalConfirmation => {
            if higher_confirmed && higher_direction == candidate_direction {
                PolicyDecision::Accept
            } else {
                PolicyDecision::RequireConfirmation
            }
        }
    }
}

/// One opportunity with its originating source preserved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnifiedOpportunity {
    /// Instrument.
    pub symbol: String,
    /// Timeframe.
    pub timeframe: Timeframe,
    /// Direction.
    pub direction: Direction,
    /// Who proposed it: `strategy`, `core_analytics`, or `both`.
    pub source: String,
    /// Evidence strength from the proposing source; not a probability.
    pub evidence_strength: f64,
    /// Policy outcome.
    pub decision: PolicyDecision,
    /// Setup proposal, when one exists.
    pub setup: Option<SetupProposal>,
}

/// Combine independent strategy and Core Analytics candidates without letting
/// one silently overwrite the other.
#[must_use]
pub fn unify_opportunities(
    config: &ChartIntelligenceConfig,
    higher_direction: Direction,
    higher_confirmed: bool,
    strategy_candidates: &[OpportunityCandidate],
    core_candidates: &[OpportunityCandidate],
    setups: &[SetupProposal],
) -> Vec<UnifiedOpportunity> {
    let mut out = Vec::new();
    let mut push = |candidate: &OpportunityCandidate, source: &str| {
        let decision = evaluate_policy(
            config,
            higher_direction,
            candidate.direction,
            higher_confirmed,
            candidate.evidence_strength,
        );
        let setup = setups
            .iter()
            .find(|setup| {
                setup.timeframe == candidate.timeframe && setup.direction == candidate.direction
            })
            .cloned();
        out.push(UnifiedOpportunity {
            symbol: config.symbol.clone(),
            timeframe: candidate.timeframe,
            direction: candidate.direction,
            source: source.to_string(),
            evidence_strength: candidate.evidence_strength,
            decision,
            setup,
        });
    };
    for candidate in strategy_candidates {
        push(candidate, "strategy");
    }
    for candidate in core_candidates {
        let mirrored = strategy_candidates.iter().any(|other| {
            other.timeframe == candidate.timeframe && other.direction == candidate.direction
        });
        push(candidate, if mirrored { "both" } else { "core_analytics" });
    }
    out
}

/// A discovered strategy pattern with its observed support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredStrategy {
    /// Stable key: timeframe and direction.
    pub key: String,
    /// Number of observations supporting the pattern.
    pub observations: usize,
    /// Mean evidence strength among them.
    pub mean_strength: f64,
    /// Explicit limitation.
    pub limitation: String,
}

/// Discover recurring opportunity patterns without fitting a model.
#[must_use]
pub fn discover_strategies(
    opportunities: &[UnifiedOpportunity],
    min_observations: usize,
) -> Vec<DiscoveredStrategy> {
    let mut groups: BTreeMap<(Timeframe, Direction), (usize, f64)> = BTreeMap::new();
    for opportunity in opportunities {
        let entry = groups
            .entry((opportunity.timeframe, opportunity.direction))
            .or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += opportunity.evidence_strength;
    }
    groups
        .into_iter()
        .filter(|(_, (count, _))| *count >= min_observations)
        .map(
            |((timeframe, direction), (count, total))| DiscoveredStrategy {
                key: format!("{timeframe}-{direction:?}"),
                observations: count,
                mean_strength: total / count as f64,
                limitation: "frequency of a historical pattern, not validated edge".to_string(),
            },
        )
        .collect()
}

/// Summarise a completed frame's role for per-chart dashboards.
#[must_use]
pub fn frame_role(frame: &TimeframeObservation) -> (Timeframe, usize) {
    (frame.timeframe, frame.completed().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        timeframe: Timeframe,
        direction: Direction,
        strength: f64,
    ) -> OpportunityCandidate {
        OpportunityCandidate {
            timeframe,
            direction,
            evidence_strength: strength,
            probability_distribution: None,
            expected_horizon_ns: None,
            supporting_evidence: Vec::new(),
            conflicting_evidence: Vec::new(),
        }
    }

    #[test]
    fn policies_differ_on_alignment_and_countertrend() {
        let mut config = ChartIntelligenceConfig::new("BTCUSDT", Timeframe::M5);
        config.policy = StrategyPolicy::DirectionalAlignment;
        assert_eq!(
            evaluate_policy(&config, Direction::Bullish, Direction::Bullish, true, 0.8),
            PolicyDecision::Accept
        );
        assert_eq!(
            evaluate_policy(&config, Direction::Bullish, Direction::Bearish, true, 0.8),
            PolicyDecision::Reject
        );
        config.policy = StrategyPolicy::Countertrend;
        assert_eq!(
            evaluate_policy(&config, Direction::Bullish, Direction::Bearish, true, 0.8),
            PolicyDecision::Accept
        );
    }

    #[test]
    fn confirmation_policy_requires_a_confirmed_higher_frame() {
        let mut config = ChartIntelligenceConfig::new("BTCUSDT", Timeframe::M5);
        config.policy = StrategyPolicy::HierarchicalConfirmation;
        assert_eq!(
            evaluate_policy(&config, Direction::Bullish, Direction::Bullish, false, 0.9),
            PolicyDecision::RequireConfirmation
        );
        assert_eq!(
            evaluate_policy(&config, Direction::Bullish, Direction::Bullish, true, 0.9),
            PolicyDecision::Accept
        );
    }

    #[test]
    fn unification_preserves_both_sources() {
        let config = ChartIntelligenceConfig::new("BTCUSDT", Timeframe::M5);
        let strategy = vec![candidate(Timeframe::M5, Direction::Bullish, 0.8)];
        let core = vec![
            candidate(Timeframe::M5, Direction::Bullish, 0.7),
            candidate(Timeframe::H1, Direction::Bearish, 0.6),
        ];
        let unified = unify_opportunities(&config, Direction::Bullish, true, &strategy, &core, &[]);
        assert_eq!(unified.len(), 3);
        assert!(unified.iter().any(|item| item.source == "both"));
        assert!(unified.iter().any(|item| item.source == "core_analytics"));
        assert!(unified.iter().any(|item| item.source == "strategy"));
    }

    #[test]
    fn discovery_needs_minimum_support() {
        let config = ChartIntelligenceConfig::new("BTCUSDT", Timeframe::M5);
        let core = vec![candidate(Timeframe::M5, Direction::Bullish, 0.7)];
        let unified = unify_opportunities(&config, Direction::Bullish, true, &[], &core, &[]);
        assert!(discover_strategies(&unified, 2).is_empty());
        let found = discover_strategies(&unified, 1);
        assert_eq!(found.len(), 1);
        assert!(found[0].limitation.contains("not validated"));
    }

    #[test]
    fn weak_evidence_is_rejected_under_every_policy() {
        for policy in [
            StrategyPolicy::Contextual,
            StrategyPolicy::DirectionalAlignment,
            StrategyPolicy::Countertrend,
            StrategyPolicy::HierarchicalConfirmation,
        ] {
            let mut config = ChartIntelligenceConfig::new("BTCUSDT", Timeframe::M5);
            config.policy = policy;
            assert_eq!(
                evaluate_policy(&config, Direction::Bullish, Direction::Bullish, true, 0.1),
                PolicyDecision::Reject
            );
        }
    }
}
