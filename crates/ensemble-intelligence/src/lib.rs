//! Deterministic ensemble fusion for independent intelligence engines.
//!
//! This crate does not contain a model. It takes typed evidence from engines
//! such as Markov, SMC, footprint, VWAP or a future Kronos adapter, resolves
//! disagreement, and produces a decision **proposal**. Risk and execution stay
//! with the existing independent gate.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Direction an engine reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Bullish,
    Bearish,
    Neutral,
}

impl Direction {
    fn sign(self) -> f64 {
        match self {
            Direction::Bullish => 1.0,
            Direction::Bearish => -1.0,
            Direction::Neutral => 0.0,
        }
    }
}

/// The kind of evidence an engine contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCategory {
    Forecast,
    MarketStructure,
    OrderFlow,
    Volatility,
    Regime,
    Microstructure,
}

impl EvidenceCategory {
    fn default_weight(self) -> f64 {
        match self {
            EvidenceCategory::Forecast => 1.0,
            EvidenceCategory::MarketStructure => 1.2,
            EvidenceCategory::OrderFlow => 1.1,
            EvidenceCategory::Volatility => 0.8,
            EvidenceCategory::Regime => 1.0,
            EvidenceCategory::Microstructure => 0.9,
        }
    }
}

/// One engine's evidence about a market snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Engine id, e.g. `markov` or `smc`.
    pub source: String,
    /// What kind of statement this is.
    pub category: EvidenceCategory,
    /// Direction the engine reports.
    pub direction: Direction,
    /// Claimed strength, 0..1.
    pub strength: f64,
    /// Calibrated reliability, 0..1.
    pub reliability: f64,
    /// Whether the engine had usable data.
    pub valid: bool,
    /// Observation time, unix nanoseconds.
    pub observed_at: i64,
    /// Human-readable reason, kept for audit.
    pub note: String,
}

impl Evidence {
    fn contribution(&self) -> f64 {
        if !self.valid || !self.strength.is_finite() || !self.reliability.is_finite() {
            return 0.0;
        }
        self.direction.sign()
            * self.strength.clamp(0.0, 1.0)
            * self.reliability.clamp(0.0, 1.0)
            * self.category.default_weight()
    }
}

/// Conditions checked outside the ensemble.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RiskGate {
    pub data_ok: bool,
    pub risk_ok: bool,
    pub kill_switch: bool,
}

/// A decision proposal — never an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Buy,
    Sell,
    Wait,
    NoTrade,
}

/// Fused result with the reasons a human or an agent can audit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionProposal {
    pub decision: Decision,
    pub score: f64,
    pub confidence: f64,
    pub explanations: Vec<String>,
    pub disagreements: Vec<String>,
    pub used_sources: Vec<String>,
    pub ignored_sources: Vec<String>,
}

/// Reliability tracking per source, measured from settled outcomes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReliabilityRegistry {
    hits: HashMap<String, u64>,
    observations: HashMap<String, u64>,
}

impl ReliabilityRegistry {
    /// Record one settled outcome for a source.
    pub fn observe(&mut self, source: &str, hit: bool) {
        *self.observations.entry(source.to_string()).or_insert(0) += 1;
        if hit {
            *self.hits.entry(source.to_string()).or_insert(0) += 1;
        }
    }

    /// Shrunk reliability: one prior success and one prior failure keep a new
    /// source from being trusted or dismissed on a single observation.
    pub fn reliability(&self, source: &str) -> f64 {
        let hits = self.hits.get(source).copied().unwrap_or(0) as f64;
        let total = self.observations.get(source).copied().unwrap_or(0) as f64;
        ((hits + 1.0) / (total + 2.0)).clamp(0.0, 1.0)
    }
}

/// Fuse evidence into a proposal.
///
/// Confidence below `threshold` means WAIT: unclear opportunity, not a
/// prohibited trade. NO_TRADE is reserved for a specific risk or data reason.
pub fn fuse(
    evidence: &[Evidence],
    risk: RiskGate,
    threshold: f64,
) -> DecisionProposal {
    let mut used = Vec::new();
    let mut ignored = Vec::new();
    if !risk.data_ok {
        ignored.push("data_quality".to_string());
    }
    if !risk.risk_ok || risk.kill_switch {
        ignored.push("risk_gate".to_string());
    }
    if !risk.data_ok || !risk.risk_ok || risk.kill_switch {
        let mut reasons = Vec::new();
        if !risk.data_ok {
            reasons.push("data quality is not usable".to_string());
        }
        if risk.kill_switch {
            reasons.push("the kill-switch is active".to_string());
        } else if !risk.risk_ok {
            reasons.push("risk limits are not satisfied".to_string());
        }
        return DecisionProposal {
            decision: Decision::NoTrade,
            score: 0.0,
            confidence: 0.0,
            explanations: reasons,
            disagreements: Vec::new(),
            used_sources: used,
            ignored_sources: ignored,
        };
    }

    // Correlated engines are not independent confirmation: within one
    // category the strongest contribution counts fully, the rest count at
    // half weight.
    let mut by_category: BTreeMap<EvidenceCategory, Vec<&Evidence>> = BTreeMap::new();
    for item in evidence {
        if !item.valid {
            ignored.push(item.source.clone());
            continue;
        }
        by_category
            .entry(item.category)
            .or_default()
            .push(item);
        used.push(item.source.clone());
    }

    let mut score = 0.0;
    let mut bullish = 0.0;
    let mut bearish = 0.0;
    let mut disagreements = Vec::new();
    let mut explanations = Vec::new();
    for (category, mut items) in by_category {
        items.sort_by(|a, b| {
            b.contribution()
                .abs()
                .partial_cmp(&a.contribution().abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut category_score = 0.0;
        for (index, item) in items.iter().enumerate() {
            let contribution = item.contribution() * if index == 0 { 1.0 } else { 0.5 };
            category_score += contribution;
            if contribution > 0.0 {
                bullish += contribution;
            } else if contribution < 0.0 {
                bearish += contribution.abs();
            }
        }
        if category_score > 0.0 {
            explanations.push(format!("{category:?} supports a long bias"));
        } else if category_score < 0.0 {
            explanations.push(format!("{category:?} supports a short bias"));
        } else {
            explanations.push(format!("{category:?} is neutral"));
        }
        score += category_score;
    }

    if bullish > 0.0 && bearish > 0.0 {
        let weaker = bullish.min(bearish);
        let stronger = bullish.max(bearish);
        disagreements.push(format!(
            "engines disagree: bullish {bullish:.2} vs bearish {bearish:.2}"
        ));
        // Material disagreement damps the net score instead of being hidden.
        score *= 1.0 - (weaker / stronger).min(1.0) * 0.5;
    }

    let confidence = score.abs();
    let decision = if confidence < threshold.abs() {
        Decision::Wait
    } else if score > 0.0 {
        Decision::Buy
    } else {
        Decision::Sell
    };

    DecisionProposal {
        decision,
        score,
        confidence,
        explanations,
        disagreements,
        used_sources: used,
        ignored_sources: ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source: &str, category: EvidenceCategory, direction: Direction, strength: f64) -> Evidence {
        Evidence {
            source: source.into(),
            category,
            direction,
            strength,
            reliability: 0.8,
            valid: true,
            observed_at: 1_700_000_000_000_000_000,
            note: String::new(),
        }
    }

    const RISK: RiskGate = RiskGate {
        data_ok: true,
        risk_ok: true,
        kill_switch: false,
    };

    #[test]
    fn agreeing_engines_produce_a_directional_proposal() {
        let result = fuse(
            &[
                evidence("markov", EvidenceCategory::Regime, Direction::Bullish, 0.9),
                evidence("smc", EvidenceCategory::MarketStructure, Direction::Bullish, 0.8),
            ],
            RISK,
            0.2,
        );
        assert_eq!(result.decision, Decision::Buy);
        assert!(result.score > 0.0);
        assert!(result.disagreements.is_empty());
    }

    #[test]
    fn material_disagreement_returns_wait() {
        let result = fuse(
            &[
                evidence("markov", EvidenceCategory::Regime, Direction::Bullish, 0.5),
                evidence("footprint", EvidenceCategory::OrderFlow, Direction::Bearish, 0.5),
            ],
            RISK,
            0.2,
        );
        assert_eq!(result.decision, Decision::Wait);
        assert_eq!(result.disagreements.len(), 1);
    }

    #[test]
    fn a_risk_block_is_notrade_not_wait() {
        let blocked = RiskGate {
            risk_ok: false,
            ..RISK
        };
        let result = fuse(
            &[evidence("markov", EvidenceCategory::Regime, Direction::Bullish, 1.0)],
            blocked,
            0.2,
        );
        assert_eq!(result.decision, Decision::NoTrade);
        assert!(result.explanations.iter().any(|why| why.contains("risk limits")));
    }

    #[test]
    fn reliability_uses_a_bayesian_prior() {
        let mut registry = ReliabilityRegistry::default();
        assert!((registry.reliability("new") - 0.5).abs() < 1e-12);
        for _ in 0..8 {
            registry.observe("new", true);
        }
        assert!((registry.reliability("new") - 0.9).abs() < 1e-12);
    }

    #[test]
    fn invalid_evidence_is_ignored_not_counted() {
        let mut item = evidence("markov", EvidenceCategory::Regime, Direction::Bullish, 1.0);
        item.valid = false;
        let result = fuse(&[item], RISK, 0.2);
        assert_eq!(result.decision, Decision::Wait);
        assert_eq!(result.ignored_sources, vec!["markov".to_string()]);
    }
}
