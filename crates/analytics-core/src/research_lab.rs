//! Experimental market reverse-engineering laboratory.
//!
//! The six engines here compose the deterministic infrastructure already in
//! `analytics-core`. The lab does not invent a participant identity, a trained
//! model or a calibrated probability: each engine reports what its inputs
//! actually support, and the validation engine says plainly that it needs
//! historical outcomes.
//!
//! This is a research surface for generating hypotheses. It cannot place a
//! trade, and none of its findings bypass strategy rules or the risk gate.

use serde::{Deserialize, Serialize};

use crate::cross_scale::reconstruct;
use crate::disciplines::{analyze_hidden_state, analyze_structure};
use crate::forecast::{bootstrap_forecast, ForecastSpec};
use crate::hierarchy::Direction;
use crate::multiscale::MultiTimeframeObservation;
use crate::types::{Candle, Timeframe};

/// The six engines, in the order the research programme defines them.
pub const ENGINE_NAMES: [&str; 6] = [
    "structural_discovery",
    "participant_behaviour",
    "market_state",
    "information_discovery",
    "scenario_simulation",
    "prediction_validation",
];

/// Configuration for one lab run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LabConfig {
    /// Minimum completed bars an engine needs before concluding anything.
    pub min_bars: usize,
    /// Forecast horizon in finest-frame bars.
    pub forecast_steps: usize,
    /// Number of bootstrap paths in the scenario cone.
    pub forecast_paths: usize,
    /// Caller-supplied seed, so a run can be reproduced exactly.
    pub seed: u64,
}

impl Default for LabConfig {
    fn default() -> Self {
        Self {
            min_bars: 30,
            forecast_steps: 12,
            forecast_paths: 200,
            seed: 0x5EED_1234_5678_9ABC,
        }
    }
}

/// One engine's finding. `evidence_strength` is not a probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabFinding {
    /// Engine that produced the finding.
    pub engine: String,
    /// Frame it describes, when applicable.
    pub timeframe: Option<Timeframe>,
    /// Directional lean. `Neutral` for non-evidence engines.
    pub direction: Direction,
    /// Evidence strength in `[0, 1]`.
    pub evidence_strength: f64,
    /// Human-readable observation for research and chart display.
    pub observation: String,
}

/// A seeded bootstrap scenario cone, summarised for display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    /// Frame the scenarios were simulated from.
    pub timeframe: Timeframe,
    /// Last known close the paths start from.
    pub start: f64,
    /// Median price at the horizon.
    pub median_horizon: f64,
    /// Fifth-percentile price at the horizon.
    pub lower_05: f64,
    /// Ninety-fifth-percentile price at the horizon.
    pub upper_95: f64,
    /// Simulated path count actually used.
    pub paths: usize,
    /// Horizon in bars.
    pub steps: usize,
}

/// Result of running all six engines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabReport {
    /// Instrument analysed.
    pub instrument_id: String,
    /// Decision instant, in unix nanoseconds.
    pub decision_timestamp: i64,
    /// One finding per engine.
    pub findings: Vec<LabFinding>,
    /// Scenario cone, when the bootstrap engine had enough returns.
    pub scenarios: Option<ScenarioSummary>,
    /// Majority direction of the evidence engines; neutral on a tie.
    pub consensus: Direction,
    /// Statements about what this run does not establish.
    pub limitations: Vec<String>,
}

fn bounded(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Heuristic aggressor dominance from completed candles.
///
/// This does **not** identify a participant class: it only reports the
/// buy/sell volume split and candle bodies visible in the data. Calling it
/// "institutional flow" would be a claim the data cannot support.
fn participant_finding(candles: &[Candle], timeframe: Timeframe) -> LabFinding {
    let (buy, sell) = candles.iter().fold((0.0_f64, 0.0_f64), |(b, s), candle| {
        (
            b + candle.buy_volume.max(0.0),
            s + candle.sell_volume.max(0.0),
        )
    });
    let total = buy + sell;
    let dominance = if total > 0.0 {
        (buy - sell) / total
    } else {
        0.0
    };
    let direction = if dominance > 0.05 {
        Direction::Bullish
    } else if dominance < -0.05 {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    LabFinding {
        engine: "participant_behaviour".to_string(),
        timeframe: Some(timeframe),
        direction,
        evidence_strength: bounded(dominance.abs()),
        observation: format!(
            "aggressor split buy {buy:.1} / sell {sell:.1} over {} bars; heuristic, not participant identity",
            candles.len()
        ),
    }
}

fn structural_finding(
    observation: &MultiTimeframeObservation,
    min_bars: usize,
) -> Option<LabFinding> {
    let frame = observation.lowest()?;
    if frame.completed().len() < min_bars {
        return None;
    }
    let analysis = analyze_structure(frame)?;
    Some(LabFinding {
        engine: "structural_discovery".to_string(),
        timeframe: Some(frame.timeframe),
        direction: analysis.direction,
        evidence_strength: analysis.evidence_strength,
        observation: analysis.structural_observations.join("; "),
    })
}

fn market_state_finding(
    observation: &MultiTimeframeObservation,
    min_bars: usize,
) -> Option<LabFinding> {
    let frame = observation.lowest()?;
    if frame.completed().len() < min_bars {
        return None;
    }
    let analysis = analyze_hidden_state(frame)?;
    Some(LabFinding {
        engine: "market_state".to_string(),
        timeframe: Some(frame.timeframe),
        direction: analysis.direction,
        evidence_strength: analysis.evidence_strength,
        observation: analysis.structural_observations.join("; "),
    })
}

fn information_finding(observation: &MultiTimeframeObservation) -> Option<LabFinding> {
    let reconstruction = reconstruct(observation);
    let strongest = reconstruction
        .dependencies
        .iter()
        .max_by(|a, b| a.normalized_dependence.total_cmp(&b.normalized_dependence))?;
    Some(LabFinding {
        engine: "information_discovery".to_string(),
        timeframe: Some(strongest.lower_timeframe),
        direction: reconstruction.consensus,
        evidence_strength: bounded(strongest.normalized_dependence),
        observation: format!(
            "{} -> {} dependence {:.3} bits over {} aligned bars ({})",
            strongest.higher_timeframe,
            strongest.lower_timeframe,
            strongest.mutual_information_bits,
            strongest.samples,
            if strongest.statistical_significance.is_none() {
                "significance not evaluated"
            } else {
                "significance supplied externally"
            }
        ),
    })
}

fn scenario_finding(
    observation: &MultiTimeframeObservation,
    config: &LabConfig,
) -> (Option<LabFinding>, Option<ScenarioSummary>) {
    let Some(frame) = observation.lowest() else {
        return (None, None);
    };
    let closes: Vec<f64> = frame
        .completed()
        .iter()
        .map(|candle| candle.close)
        .collect();
    let spec = ForecastSpec {
        steps: config.forecast_steps,
        paths: config.forecast_paths,
        seed: config.seed,
        vol_scale: 1.0,
    };
    let Some(forecast) = bootstrap_forecast(&closes, &spec) else {
        return (None, None);
    };
    let Some(last) = forecast.steps.last() else {
        return (None, None);
    };
    let summary = ScenarioSummary {
        timeframe: frame.timeframe,
        start: forecast.start,
        median_horizon: last[2],
        lower_05: last[0],
        upper_95: last[4],
        paths: forecast.paths,
        steps: forecast.steps.len(),
    };
    let finding = LabFinding {
        engine: "scenario_simulation".to_string(),
        timeframe: Some(frame.timeframe),
        direction: Direction::Neutral,
        evidence_strength: 0.0,
        observation: format!(
            "bootstrap cone {:.2}..{:.2} around median {:.2} over {} bars (scenario, not evidence)",
            summary.lower_05, summary.upper_95, summary.median_horizon, summary.steps
        ),
    };
    (Some(finding), Some(summary))
}

fn validation_finding() -> LabFinding {
    LabFinding {
        engine: "prediction_validation".to_string(),
        timeframe: None,
        direction: Direction::Neutral,
        evidence_strength: 0.0,
        observation:
            "requires historical outcomes: use validation::compare_hierarchies on a labelled series"
                .to_string(),
    }
}

/// Run all six engines over one multiscale observation.
///
/// Engines that cannot conclude from the supplied data are omitted rather than
/// given a fabricated result. The validation engine always reports its
/// requirement instead of scoring an unlabelled observation.
#[must_use]
pub fn run_lab(observation: &MultiTimeframeObservation, config: &LabConfig) -> LabReport {
    let mut findings = Vec::with_capacity(ENGINE_NAMES.len());
    if let Some(finding) = structural_finding(observation, config.min_bars) {
        findings.push(finding);
    }
    if let Some(frame) = observation.lowest() {
        findings.push(participant_finding(frame.completed(), frame.timeframe));
    }
    if let Some(finding) = market_state_finding(observation, config.min_bars) {
        findings.push(finding);
    }
    if let Some(finding) = information_finding(observation) {
        findings.push(finding);
    }
    let (scenario, scenarios) = scenario_finding(observation, config);
    if let Some(finding) = scenario {
        findings.push(finding);
    }
    findings.push(validation_finding());

    let bullish = findings
        .iter()
        .filter(|finding| finding.direction == Direction::Bullish)
        .count();
    let bearish = findings
        .iter()
        .filter(|finding| finding.direction == Direction::Bearish)
        .count();
    let consensus = if bullish > bearish {
        Direction::Bullish
    } else if bearish > bullish {
        Direction::Bearish
    } else {
        Direction::Neutral
    };

    LabReport {
        instrument_id: observation.instrument_id.clone(),
        decision_timestamp: observation.decision_timestamp,
        findings,
        scenarios,
        consensus,
        limitations: vec![
            "engine findings are hypotheses, not calibrated probabilities".to_string(),
            "participant behaviour is a volume heuristic, not participant identification"
                .to_string(),
            "scenario cones are resampled history, not forecasts of a fitted model".to_string(),
            "no engine in this report was validated against out-of-sample outcomes".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiscale::TimeframeObservation as Frame;
    use crate::types::Candle;

    const MIN: i64 = 60_000_000_000;

    fn observation(bullish: bool) -> MultiTimeframeObservation {
        let decision = 2000 * MIN;
        let frames = [Timeframe::H1, Timeframe::M15, Timeframe::M1]
            .into_iter()
            .map(|timeframe| {
                let count = (decision / timeframe.nanos()).clamp(1, 80) as usize;
                let candles: Vec<Candle> = (0..count)
                    .map(|i| {
                        let open = 100.0 + i as f64 * 0.3 + (i % 5) as f64 * 0.05;
                        let close = if bullish { open + 0.5 } else { open - 0.5 };
                        Candle {
                            symbol: "BTCUSDT".to_string(),
                            timeframe,
                            open_time: i as i64 * timeframe.nanos(),
                            open,
                            high: open.max(close) + 0.2,
                            low: open.min(close) - 0.2,
                            close,
                            volume: 10.0,
                            buy_volume: if bullish { 7.0 } else { 3.0 },
                            sell_volume: if bullish { 3.0 } else { 7.0 },
                        }
                    })
                    .collect();
                Frame::from_candles(timeframe, "BTCUSDT", &candles, decision, "test", 1).unwrap()
            })
            .collect();
        MultiTimeframeObservation::new("BTCUSDT", decision, frames).unwrap()
    }

    #[test]
    fn all_six_engines_report_something_on_a_usable_observation() {
        let report = run_lab(&observation(true), &LabConfig::default());
        let engines: Vec<&str> = report.findings.iter().map(|f| f.engine.as_str()).collect();
        for name in ENGINE_NAMES {
            assert!(engines.contains(&name), "missing engine {name}");
        }
        assert_eq!(report.findings.len(), 6);
        assert!(report.scenarios.is_some());
    }

    #[test]
    fn non_evidence_engines_do_not_claim_direction_or_strength() {
        let report = run_lab(&observation(true), &LabConfig::default());
        for finding in report
            .findings
            .iter()
            .filter(|f| f.engine == "scenario_simulation" || f.engine == "prediction_validation")
        {
            assert_eq!(finding.direction, Direction::Neutral);
            assert_eq!(finding.evidence_strength, 0.0);
        }
    }

    #[test]
    fn evidence_is_bounded_and_consensus_follows_the_data() {
        let bullish = run_lab(&observation(true), &LabConfig::default());
        let bearish = run_lab(&observation(false), &LabConfig::default());
        assert_eq!(bullish.consensus, Direction::Bullish);
        assert_eq!(bearish.consensus, Direction::Bearish);
        assert!(bullish
            .findings
            .iter()
            .all(|f| { f.evidence_strength >= 0.0 && f.evidence_strength <= 1.0 }));
    }

    #[test]
    fn scenarios_have_ordered_quantiles() {
        let report = run_lab(&observation(true), &LabConfig::default());
        let cone = report.scenarios.unwrap();
        assert!(cone.lower_05 <= cone.median_horizon);
        assert!(cone.median_horizon <= cone.upper_95);
        assert!(cone.start.is_finite() && cone.paths > 0 && cone.steps > 0);
    }

    #[test]
    fn insufficient_data_is_reported_not_fabricated() {
        let tiny = MultiTimeframeObservation::new(
            "BTCUSDT",
            MIN * 40,
            vec![Frame::from_candles(
                Timeframe::M1,
                "BTCUSDT",
                &observation(true).lowest().unwrap().completed()[..2],
                MIN * 40,
                "test",
                1,
            )
            .unwrap()],
        )
        .unwrap();
        let report = run_lab(&tiny, &LabConfig::default());
        assert!(report
            .findings
            .iter()
            .all(|f| f.engine != "structural_discovery"));
        assert!(report.findings.iter().all(|f| f.engine != "market_state"));
        assert!(report
            .findings
            .iter()
            .any(|f| f.engine == "prediction_validation"));
    }

    #[test]
    fn runs_are_reproducible() {
        let config = LabConfig::default();
        assert_eq!(
            run_lab(&observation(true), &config),
            run_lab(&observation(true), &config)
        );
    }
}
