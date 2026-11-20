//! Markov-family intelligence for regime, transition, volatility, duration,
//! event-timing and strategy-selection evidence.
//!
//! This crate is deterministic and has no I/O. It never talks to an exchange,
//! an LLM, or a broker. Every engine is independently usable; [`analyze`] is a
//! convenience that returns one typed evidence record for the agent layer.

pub mod ctmc;
pub mod mdp;
pub mod regime;
pub mod state;
pub mod transition;
pub mod volatility;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::state::{MarketFeatures, Regime};

/// One analysis request. `returns` are simple returns in chronological order;
/// `regime_history` is the inferred or observed past-state sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkovRequest {
    pub instrument: String,
    pub timeframe: String,
    pub features: MarketFeatures,
    pub returns: Vec<f64>,
    pub regime_history: Vec<Regime>,
    pub atr_reference: f64,
    /// Calendar-time span used by the continuous-time chain, in bars.
    pub horizon: f64,
    pub gamma: f64,
}

/// Typed evidence produced for an agent or a deterministic decision layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkovResult {
    pub instrument: String,
    pub timeframe: String,
    pub current_regime: String,
    pub regime_probabilities: BTreeMap<String, f64>,
    pub transition_probabilities: BTreeMap<String, f64>,
    pub ctmc_probabilities: BTreeMap<String, f64>,
    pub expected_duration: Option<f64>,
    pub volatility_regime: String,
    pub transition_risk: f64,
    pub mdp_policy: Vec<String>,
    pub model_version: String,
}

/// The seven betas per regime bind to [`MarketFeatures::vector`] in order.
fn tvtp_betas() -> Vec<Vec<f64>> {
    vec![
        vec![0.2, -0.1, 0.3, 0.4, 0.3, 0.2, -0.1],     // bull
        vec![-0.2, -0.1, 0.3, -0.4, -0.3, -0.2, -0.1], // bear
        vec![0.0, -0.05, 0.0, 0.0, 0.0, 0.0, 0.1],     // sideways
        vec![0.0, 0.6, 0.4, 0.0, 0.2, 0.0, 0.5],       // high volatility
        vec![0.0, -0.6, -0.2, 0.0, -0.1, 0.0, -0.3],   // low volatility
    ]
}

fn state_scores(features: MarketFeatures) -> [f64; 5] {
    let direction = features.momentum + features.delta + features.distance_from_vwap;
    let volatility = features.atr_pct.max(1e-9);
    let reference = features.atr_pct.max(1e-9);
    let _ = reference;
    [
        direction.max(0.0) + 0.01,
        (-direction).max(0.0) + 0.01,
        1.0 / (1.0 + direction.abs()),
        volatility,
        1.0 / volatility,
    ]
}

fn normalized(scores: [f64; 5]) -> [f64; 5] {
    let total: f64 = scores.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return [0.2; 5];
    }
    let mut out = [0.0; 5];
    for (slot, score) in out.iter_mut().zip(scores.iter()) {
        *slot = score / total;
    }
    out
}

/// Combine all seven Markov engines into one evidence record.
pub fn analyze(request: &MarkovRequest) -> Result<MarkovResult, String> {
    let current = request.features.classify(request.atr_reference);

    // 1. Time-varying transition probabilities.
    let base = transition::fixed(Regime::ALL.len(), 0.8);
    let changed = transition::tvtp(&base, &request.features.vector(), &tvtp_betas())?;

    // 2. Regime probabilities from observable feature scores.
    let probabilities = normalized(state_scores(request.features));

    // 3. Regime-specific return dynamics (MS-AR) as transition-risk context.
    let ar_models = regime::ms_ar(&request.returns, &request.regime_history, 1);
    let ar_now = ar_models.get(&current).map_or(0.0, |model| {
        let last = request.returns.last().copied().unwrap_or(0.0);
        model.predict(&[last])
    });

    // 4. Hidden semi-Markov duration.
    let mut hsmm = regime::Hsmm::default();
    hsmm.observe(&request.regime_history);
    let expected_duration = hsmm.expected_duration(current);

    // 5./6. Volatility: GARCH, EGARCH and MS-GARCH evidence.
    let mut parameters = BTreeMap::new();
    parameters.insert(Regime::LowVolatility, (1e-8, 0.04, 0.9));
    parameters.insert(Regime::HighVolatility, (1e-4, 0.2, 0.75));
    for regime in [Regime::BullTrend, Regime::BearTrend, Regime::Sideways] {
        parameters.insert(regime, (1e-6, 0.08, 0.88));
    }
    let variance = volatility::ms_garch(&request.returns, &request.regime_history, &parameters);
    let _egarch = volatility::egarch(&request.returns, 0.0, 0.08, 0.9, -0.2);
    let volatility_regime = if variance.last().copied().unwrap_or(0.0)
        > variance.first().copied().unwrap_or(f64::MIN_POSITIVE) * 1.5
    {
        "expanding"
    } else {
        "stable"
    };

    // 7. Continuous-time chain for irregular event timing.
    let horizon = if request.horizon.is_finite() && request.horizon > 0.0 {
        request.horizon
    } else {
        1.0
    };
    let generator: transition::Matrix = changed
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(j, probability)| {
                    if j == row.len() / 2 && j < Regime::ALL.len() {
                        -(1.0 - probability) / horizon
                    } else {
                        probability / horizon
                    }
                })
                .collect()
        })
        .collect();
    let continuous = ctmc::exp_generator(&generator, horizon)?;

    // 8. Context-dependent higher-order transition estimate.
    let mut counts = BTreeMap::new();
    for pair in request.regime_history.windows(2) {
        *counts
            .entry(vec![pair[0].index(), pair[1].index()])
            .or_insert(0usize) += 1;
    }
    let context: Vec<usize> = request.regime_history.iter().map(|r| r.index()).collect();
    let context_counts: std::collections::HashMap<Vec<usize>, usize> = counts.into_iter().collect();
    let higher = transition::higher_order(&context_counts, &context, 2, Regime::ALL.len());

    // 9. Markov decision process: wait vs act. The policy is a proposal only.
    let identity: transition::Matrix = (0..Regime::ALL.len())
        .map(|i| {
            (0..Regime::ALL.len())
                .map(|j| if i == j { 1.0 } else { 0.0 })
                .collect()
        })
        .collect();
    let reward = ar_now + request.features.delta + request.features.momentum;
    let (_, policy) = mdp::value_iteration(
        &[identity, changed.clone()],
        &[
            vec![0.0; Regime::ALL.len()],
            vec![reward; Regime::ALL.len()],
        ],
        request.gamma.clamp(0.0, 0.999),
        1e-8,
        256,
    )?;

    let probabilities_by_name: BTreeMap<String, f64> = Regime::ALL
        .iter()
        .zip(probabilities.iter())
        .map(|(regime, value)| (regime.as_str().to_string(), *value))
        .collect();
    let transitions_by_name: BTreeMap<String, f64> = Regime::ALL
        .iter()
        .zip(changed[current.index()].iter())
        .map(|(regime, value)| (regime.as_str().to_string(), *value))
        .collect();
    let ctmc_by_name: BTreeMap<String, f64> = Regime::ALL
        .iter()
        .zip(continuous[current.index()].iter())
        .map(|(regime, value)| (regime.as_str().to_string(), *value))
        .collect();
    let _ = higher; // available independently through `transition::higher_order`.

    Ok(MarkovResult {
        instrument: request.instrument.clone(),
        timeframe: request.timeframe.clone(),
        current_regime: current.as_str().to_string(),
        regime_probabilities: probabilities_by_name,
        transition_probabilities: transitions_by_name,
        ctmc_probabilities: ctmc_by_name,
        expected_duration,
        volatility_regime: volatility_regime.to_string(),
        transition_risk: 1.0 - changed[current.index()][current.index()],
        mdp_policy: policy
            .into_iter()
            .map(|action| if action == 0 { "wait" } else { "act" }.to_string())
            .collect(),
        model_version: "markov-engine-v1".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> MarkovRequest {
        MarkovRequest {
            instrument: "BTCUSDT".into(),
            timeframe: "5m".into(),
            features: MarketFeatures {
                return_1: 0.002,
                atr_pct: 0.004,
                volume_ratio: 1.4,
                momentum: 0.01,
                delta: 120.0,
                distance_from_vwap: 15.0,
                spread: 0.5,
            },
            returns: (0..40).map(|i| i as f64 * 0.0001).collect(),
            regime_history: vec![Regime::Sideways; 8]
                .into_iter()
                .chain(vec![Regime::BullTrend; 6])
                .collect(),
            atr_reference: 0.003,
            horizon: 4.0,
            gamma: 0.9,
        }
    }

    #[test]
    fn analyze_returns_a_complete_typed_evidence_record() {
        let result = analyze(&request()).expect("the request is well formed");
        assert_eq!(result.current_regime, "bull_trend");
        assert_eq!(result.regime_probabilities.len(), 5);
        assert_eq!(result.transition_probabilities.len(), 5);
        assert_eq!(result.ctmc_probabilities.len(), 5);
        assert_eq!(result.expected_duration, Some(6.0));
        assert!(result.transition_risk >= 0.0 && result.transition_risk <= 1.0);
        assert_eq!(result.mdp_policy.len(), 5);
        assert!((result.regime_probabilities.values().sum::<f64>() - 1.0).abs() < 1e-9);
    }
}
