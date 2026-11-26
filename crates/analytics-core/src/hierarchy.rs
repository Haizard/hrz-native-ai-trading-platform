//! Top-down hierarchical analysis over a [`MultiTimeframeObservation`].
//!
//! A discipline still does its own mathematics: this module does not fuse or
//! replace it. What it adds is the shared structure every discipline needs —
//! analyse coarsest to finest, compare what each frame concluded, and surface
//! lower-timeframe candidates together with the evidence that supports or
//! conflicts with them.
//!
//! Two honesty rules are enforced in the types themselves:
//!
//! * A discipline's evidence strength is **not** a probability. It is reported
//!   separately from [`DirectionDistribution`], which can only be built by a
//!   caller with calibrated empirical frequencies.
//! * Relationships measured here are descriptive. Statistical significance
//!   requires out-of-sample evaluation and is `None` until that is supplied.

use serde::{Deserialize, Serialize};

use crate::multiscale::{MultiTimeframeObservation, TimeframeObservation};
use crate::types::Timeframe;

/// Identifier of the discipline that produced a hierarchical result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisciplineId(String);

impl DisciplineId {
    /// Create an identifier from a stable discipline name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The discipline's stable name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DisciplineId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Directional conclusion of one discipline at one timeframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// Evidence leans upward.
    Bullish,
    /// Evidence leans downward.
    Bearish,
    /// No directional lean, or the disciplines disagree.
    Neutral,
}

impl Direction {
    /// The opposite direction, with neutral unchanged.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Bullish => Self::Bearish,
            Self::Bearish => Self::Bullish,
            Self::Neutral => Self::Neutral,
        }
    }

    /// Whether two directional conclusions agree.
    #[must_use]
    pub const fn agrees_with(self, other: Self) -> bool {
        !matches!(self, Self::Neutral) && self as u8 == other as u8
    }
}

/// A regime label estimated by a discipline at one timeframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketStateKind {
    /// A persistent upward state.
    Bullish,
    /// A persistent downward state.
    Bearish,
    /// A sideways state.
    Range,
    /// A state in transition, often the precursor to a lower-timeframe setup.
    Transition,
    /// The discipline could not estimate a state.
    Unknown,
}

/// Volatility condition estimated by a discipline at one timeframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolatilityState {
    /// Realised volatility is expanding.
    Expanding,
    /// Realised volatility is contracting.
    Contracting,
    /// No clear change.
    Stable,
}

/// Whether a directional distribution came from calibrated outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalibrationStatus {
    /// Frequencies were measured against historical outcomes out of sample.
    Empirical,
    /// Provided by a caller without an empirical calibration guarantee.
    Unverified,
}

/// A directional probability distribution.
///
/// Only construct this from measured outcomes. A score of 85 from a discipline
/// is not an 85% probability of success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectionDistribution {
    /// Calibrated probability of an upward outcome.
    pub bullish: f64,
    /// Calibrated probability of a downward outcome.
    pub bearish: f64,
    /// Calibrated probability of no directional outcome.
    pub neutral: f64,
    /// Provenance of the numbers.
    pub calibration: CalibrationStatus,
}

impl DirectionDistribution {
    /// Build a distribution, rejecting values that cannot be probabilities.
    pub fn new(
        bullish: f64,
        bearish: f64,
        neutral: f64,
        calibration: CalibrationStatus,
    ) -> Result<Self, String> {
        let values = [bullish, bearish, neutral];
        if values
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
        {
            return Err("probabilities must be finite and within [0, 1]".to_string());
        }
        let sum: f64 = values.iter().sum();
        if (sum - 1.0).abs() > 1e-9 {
            return Err(format!("probabilities must sum to 1, got {sum}"));
        }
        Ok(Self {
            bullish,
            bearish,
            neutral,
            calibration,
        })
    }

    /// The most likely direction.
    #[must_use]
    pub fn argmax(&self) -> Direction {
        if self.bullish >= self.bearish && self.bullish >= self.neutral {
            Direction::Bullish
        } else if self.bearish >= self.neutral {
            Direction::Bearish
        } else {
            Direction::Neutral
        }
    }
}

/// One discipline's structured finding at one timeframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticalEvidence {
    /// Discipline that produced the finding.
    pub discipline: DisciplineId,
    /// Timeframe the finding describes.
    pub timeframe: Timeframe,
    /// Human-readable observation, preserved for chart and agent display.
    pub observation: String,
    /// Evidence strength in `[0, 1]`. This is **not** a probability.
    pub strength: f64,
    /// Direction the finding leans toward.
    pub direction: Direction,
}

/// What one discipline concluded at one timeframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeframeAnalysis {
    /// Timeframe analysed.
    pub timeframe: Timeframe,
    /// Estimated market state, if the discipline estimates one.
    pub estimated_state: Option<MarketStateKind>,
    /// Volatility condition, if the discipline estimates one.
    pub volatility_state: Option<VolatilityState>,
    /// Directional conclusion.
    pub direction: Direction,
    /// Evidence strength in `[0, 1]`, never a probability.
    pub evidence_strength: f64,
    /// Structural observations worth displaying downstream.
    pub structural_observations: Vec<String>,
    /// Quality of this frame's data, in `[0, 1]`.
    pub data_quality: f64,
}

impl TimeframeAnalysis {
    /// Build a timeframe analysis, clamping strength and rejecting non-finite values.
    pub fn new(
        timeframe: Timeframe,
        direction: Direction,
        evidence_strength: f64,
        data_quality: f64,
    ) -> Result<Self, String> {
        if !evidence_strength.is_finite() || !data_quality.is_finite() {
            return Err("evidence strength and data quality must be finite".to_string());
        }
        Ok(Self {
            timeframe,
            estimated_state: None,
            volatility_state: None,
            direction,
            evidence_strength: evidence_strength.clamp(0.0, 1.0),
            structural_observations: Vec::new(),
            data_quality: data_quality.clamp(0.0, 1.0),
        })
    }

    /// Attach a market state.
    #[must_use]
    pub fn with_state(mut self, state: MarketStateKind) -> Self {
        self.estimated_state = Some(state);
        self
    }

    /// Attach a volatility condition.
    #[must_use]
    pub fn with_volatility(mut self, state: VolatilityState) -> Self {
        self.volatility_state = Some(state);
        self
    }

    /// Attach a structural observation.
    #[must_use]
    pub fn with_observation(mut self, observation: impl Into<String>) -> Self {
        self.structural_observations.push(observation.into());
        self
    }
}

/// A descriptive relationship between two frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossTimeframeRelationship {
    /// Coarser frame.
    pub higher_timeframe: Timeframe,
    /// Finer frame.
    pub lower_timeframe: Timeframe,
    /// Kind of relationship observed.
    pub relationship_type: RelationshipKind,
    /// Descriptive strength in `[0, 1]`; never a probability.
    pub measured_strength: f64,
    /// Out-of-sample significance, supplied only by a research evaluation.
    pub statistical_significance: Option<f64>,
}

/// Kinds of cross-timeframe relationship this framework can describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipKind {
    /// Both frames lean the same way.
    Alignment,
    /// The frames lean in opposite directions.
    Conflict,
    /// A higher-timeframe state conditions a lower-timeframe transition.
    StateDependency,
    /// Volatility conditions co-move across scales.
    VolatilityTransmission,
}

/// A candidate opportunity on a smaller timeframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpportunityCandidate {
    /// Timeframe where the opportunity was discovered.
    pub timeframe: Timeframe,
    /// Direction of the candidate.
    pub direction: Direction,
    /// Evidence strength in `[0, 1]`; never a probability.
    pub evidence_strength: f64,
    /// Set only by a caller holding calibrated outcome frequencies.
    pub probability_distribution: Option<DirectionDistribution>,
    /// Expected horizon, in nanoseconds, when the discipline supplies one.
    pub expected_horizon_ns: Option<i64>,
    /// Evidence from frames that support the candidate.
    pub supporting_evidence: Vec<String>,
    /// Evidence from frames that conflict with the candidate.
    pub conflicting_evidence: Vec<String>,
}

/// Uncertainty and limitations attached to every hierarchical result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UncertaintyReport {
    /// Number of frames that produced usable analysis.
    pub usable_frames: usize,
    /// Number of frames supplied to the analyzer.
    pub requested_frames: usize,
    /// Worst data-quality score among the analysed frames.
    pub worst_data_quality: f64,
    /// Share of adjacent frame pairs that conflicted, in `[0, 1]`.
    pub discipline_disagreement: f64,
    /// False: this framework does not calibrate probabilities.
    pub probability_calibrated: bool,
    /// Explicit statements of what this result does not establish.
    pub limitations: Vec<String>,
}

/// Standard result returned by every hierarchical discipline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiscaleAnalysisResult {
    /// Instrument analysed.
    pub instrument_id: String,
    /// Decision instant, in unix nanoseconds.
    pub analysis_timestamp: i64,
    /// Discipline that produced the result.
    pub discipline_id: DisciplineId,
    /// Version of the discipline's model or configuration.
    pub model_version: String,
    /// Per-frame conclusions, ordered coarsest first.
    pub timeframe_results: Vec<TimeframeAnalysis>,
    /// Descriptive relationships between frames.
    pub cross_timeframe_relationships: Vec<CrossTimeframeRelationship>,
    /// Lower-timeframe candidates, if any.
    pub lower_timeframe_candidates: Vec<OpportunityCandidate>,
    /// Uncertainty and limitations.
    pub uncertainty: UncertaintyReport,
    /// Structured evidence, one entry per frame conclusion.
    pub evidence: Vec<AnalyticalEvidence>,
}

/// Runs one discipline's per-frame analysis across a timeframe hierarchy.
///
/// The analyzer never calls a model itself: `analyze_frame` is supplied by the
/// discipline and receives only completed candles (the forming candle is not
/// reachable through the frame's `candles`). Returning `None` skips a frame —
/// for example when the discipline has too little data to conclude anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HierarchicalAnalyzer {
    /// Discipline identity recorded in results.
    pub discipline_id: DisciplineId,
    /// Model or configuration version recorded in results.
    pub model_version: String,
    /// Minimum agreement required before a lower-timeframe candidate is emitted.
    pub min_agreement: f64,
    /// Minimum evidence strength for a candidate.
    pub min_candidate_strength: f64,
}

impl HierarchicalAnalyzer {
    /// Create an analyzer with conservative defaults.
    #[must_use]
    pub fn new(discipline_id: impl Into<DisciplineId>, model_version: impl Into<String>) -> Self {
        Self {
            discipline_id: discipline_id.into(),
            model_version: model_version.into(),
            min_agreement: 0.6,
            min_candidate_strength: 0.5,
        }
    }

    /// Set the minimum higher-timeframe agreement for a candidate.
    #[must_use]
    pub const fn with_min_agreement(mut self, value: f64) -> Self {
        self.min_agreement = value;
        self
    }

    /// Set the minimum evidence strength for a candidate.
    #[must_use]
    pub const fn with_min_candidate_strength(mut self, value: f64) -> Self {
        self.min_candidate_strength = value;
        self
    }

    /// Analyse a hierarchy from coarsest to finest.
    pub fn analyze<F>(
        &self,
        observation: &MultiTimeframeObservation,
        mut analyze_frame: F,
    ) -> MultiscaleAnalysisResult
    where
        F: FnMut(&TimeframeObservation) -> Option<TimeframeAnalysis>,
    {
        let mut results = Vec::new();
        for frame in &observation.frames {
            if let Some(analysis) = analyze_frame(frame) {
                results.push(analysis);
            }
        }

        let mut relationships = Vec::new();
        let mut conflicts = 0usize;
        for pair in results.windows(2) {
            let higher = &pair[0];
            let lower = &pair[1];
            if higher.direction.agrees_with(lower.direction) {
                relationships.push(CrossTimeframeRelationship {
                    higher_timeframe: higher.timeframe,
                    lower_timeframe: lower.timeframe,
                    relationship_type: RelationshipKind::Alignment,
                    measured_strength: higher.evidence_strength.min(lower.evidence_strength),
                    statistical_significance: None,
                });
            } else if higher.direction != Direction::Neutral
                && lower.direction != Direction::Neutral
            {
                conflicts += 1;
                relationships.push(CrossTimeframeRelationship {
                    higher_timeframe: higher.timeframe,
                    lower_timeframe: lower.timeframe,
                    relationship_type: RelationshipKind::Conflict,
                    measured_strength: higher.evidence_strength.min(lower.evidence_strength),
                    statistical_significance: None,
                });
            }
            if matches!(higher.estimated_state, Some(state) if state != MarketStateKind::Unknown)
                && matches!(lower.estimated_state, Some(MarketStateKind::Transition))
            {
                relationships.push(CrossTimeframeRelationship {
                    higher_timeframe: higher.timeframe,
                    lower_timeframe: lower.timeframe,
                    relationship_type: RelationshipKind::StateDependency,
                    measured_strength: higher.evidence_strength,
                    statistical_significance: None,
                });
            }
            if let (Some(a), Some(b)) = (higher.volatility_state, lower.volatility_state) {
                if a == b && a != VolatilityState::Stable {
                    relationships.push(CrossTimeframeRelationship {
                        higher_timeframe: higher.timeframe,
                        lower_timeframe: lower.timeframe,
                        relationship_type: RelationshipKind::VolatilityTransmission,
                        measured_strength: higher.evidence_strength.min(lower.evidence_strength),
                        statistical_significance: None,
                    });
                }
            }
        }

        let evidence: Vec<AnalyticalEvidence> = results
            .iter()
            .map(|analysis| AnalyticalEvidence {
                discipline: self.discipline_id.clone(),
                timeframe: analysis.timeframe,
                observation: analysis.structural_observations.join("; "),
                strength: analysis.evidence_strength,
                direction: analysis.direction,
            })
            .collect();

        let mut candidates = Vec::new();
        if let Some(lowest) = results.last() {
            if lowest.direction != Direction::Neutral
                && lowest.evidence_strength >= self.min_candidate_strength
            {
                let higher: Vec<&TimeframeAnalysis> = results[..results.len() - 1]
                    .iter()
                    .filter(|frame| frame.direction.agrees_with(lowest.direction))
                    .collect();
                let agreement = if results.len() > 1 {
                    higher.len() as f64 / (results.len() - 1) as f64
                } else {
                    0.0
                };
                if agreement >= self.min_agreement {
                    let supporting = higher
                        .iter()
                        .map(|frame| format!("{}: {}", frame.timeframe, frame.direction_name()))
                        .collect();
                    let conflicting = results[..results.len() - 1]
                        .iter()
                        .filter(|frame| frame.direction == lowest.direction.opposite())
                        .map(|frame| format!("{}: {}", frame.timeframe, frame.direction_name()))
                        .collect();
                    candidates.push(OpportunityCandidate {
                        timeframe: lowest.timeframe,
                        direction: lowest.direction,
                        evidence_strength: lowest.evidence_strength,
                        probability_distribution: None,
                        expected_horizon_ns: None,
                        supporting_evidence: supporting,
                        conflicting_evidence: conflicting,
                    });
                }
            }
        }

        let pair_count = results.len().saturating_sub(1);
        let disagreement = if pair_count == 0 {
            0.0
        } else {
            conflicts as f64 / pair_count as f64
        };
        let worst_data_quality = results
            .iter()
            .map(|analysis| analysis.data_quality)
            .fold(1.0_f64, f64::min);
        let limitations = vec![
            "evidence strength is not a calibrated probability".to_string(),
            "relationships are descriptive; statistical significance was not evaluated".to_string(),
            "candidates require strategy rules and risk authorization before execution".to_string(),
        ];

        MultiscaleAnalysisResult {
            instrument_id: observation.instrument_id.clone(),
            analysis_timestamp: observation.decision_timestamp,
            discipline_id: self.discipline_id.clone(),
            model_version: self.model_version.clone(),
            timeframe_results: results,
            cross_timeframe_relationships: relationships,
            lower_timeframe_candidates: candidates,
            uncertainty: UncertaintyReport {
                usable_frames: evidence.len(),
                requested_frames: observation.frames.len(),
                worst_data_quality,
                discipline_disagreement: disagreement,
                probability_calibrated: false,
                limitations,
            },
            evidence,
        }
    }
}

impl TimeframeAnalysis {
    /// Stable display name for a directional conclusion.
    #[must_use]
    pub fn direction_name(&self) -> &'static str {
        match self.direction {
            Direction::Bullish => "bullish",
            Direction::Bearish => "bearish",
            Direction::Neutral => "neutral",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiscale::MultiTimeframeObservation;
    use crate::types::{Candle, Timeframe};

    const MIN: i64 = 60_000_000_000;

    fn observation() -> MultiTimeframeObservation {
        MultiTimeframeObservation::new(
            "BTCUSDT",
            100 * MIN,
            [Timeframe::H1, Timeframe::M15, Timeframe::M5]
                .into_iter()
                .map(|timeframe| {
                    let count = (100 * MIN / timeframe.nanos()).clamp(1, 30) as usize;
                    let candles: Vec<Candle> = (0..count)
                        .map(|i| candle("BTCUSDT", timeframe, i as i64 * timeframe.nanos(), 100.0))
                        .collect();
                    crate::multiscale::TimeframeObservation::from_candles(
                        timeframe,
                        "BTCUSDT",
                        &candles,
                        100 * MIN,
                        "test",
                        1,
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap()
    }

    fn candle(symbol: &str, timeframe: Timeframe, open_time: i64, close: f64) -> Candle {
        Candle {
            symbol: symbol.to_string(),
            timeframe,
            open_time,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
            buy_volume: 7.0,
            sell_volume: 3.0,
        }
    }

    fn analysis(timeframe: Timeframe, direction: Direction, strength: f64) -> TimeframeAnalysis {
        TimeframeAnalysis::new(timeframe, direction, strength, 1.0).unwrap()
    }

    #[test]
    fn direction_agreement_and_opposition() {
        assert!(Direction::Bullish.agrees_with(Direction::Bullish));
        assert!(!Direction::Bullish.agrees_with(Direction::Bearish));
        assert!(!Direction::Neutral.agrees_with(Direction::Neutral));
        assert_eq!(Direction::Bullish.opposite(), Direction::Bearish);
    }

    #[test]
    fn aligned_frames_produce_a_lower_timeframe_candidate() {
        let result = HierarchicalAnalyzer::new("test", "v1").analyze(&observation(), |frame| {
            Some(analysis(frame.timeframe, Direction::Bullish, 0.8))
        });

        assert_eq!(result.timeframe_results.len(), 3);
        assert_eq!(result.lower_timeframe_candidates.len(), 1);
        let candidate = &result.lower_timeframe_candidates[0];
        assert_eq!(candidate.timeframe, Timeframe::M5);
        assert_eq!(candidate.direction, Direction::Bullish);
        assert_eq!(candidate.probability_distribution, None);
        assert_eq!(candidate.supporting_evidence.len(), 2);
        assert!(result
            .cross_timeframe_relationships
            .iter()
            .all(|r| r.relationship_type == RelationshipKind::Alignment));
    }

    #[test]
    fn conflicting_higher_frames_block_a_candidate() {
        let result = HierarchicalAnalyzer::new("test", "v1").analyze(&observation(), |frame| {
            let direction = if frame.timeframe == Timeframe::M15 {
                Direction::Bearish
            } else {
                Direction::Bullish
            };
            Some(analysis(frame.timeframe, direction, 0.8))
        });

        assert!(result.lower_timeframe_candidates.is_empty());
        assert!(result
            .cross_timeframe_relationships
            .iter()
            .any(|r| r.relationship_type == RelationshipKind::Conflict));
        assert!(result.uncertainty.discipline_disagreement > 0.0);
    }

    #[test]
    fn state_dependency_and_volatility_transmission_are_descriptive() {
        let result = HierarchicalAnalyzer::new("test", "v1").analyze(&observation(), |frame| {
            let mut value = analysis(frame.timeframe, Direction::Bullish, 0.7);
            if frame.timeframe == Timeframe::H1 {
                value = value
                    .with_state(MarketStateKind::Bullish)
                    .with_volatility(VolatilityState::Expanding);
            }
            if frame.timeframe == Timeframe::M15 {
                value = value
                    .with_state(MarketStateKind::Transition)
                    .with_volatility(VolatilityState::Expanding);
            }
            Some(value)
        });

        let kinds: Vec<RelationshipKind> = result
            .cross_timeframe_relationships
            .iter()
            .map(|r| r.relationship_type)
            .collect();
        assert!(kinds.contains(&RelationshipKind::StateDependency));
        assert!(kinds.contains(&RelationshipKind::VolatilityTransmission));
        assert!(result
            .cross_timeframe_relationships
            .iter()
            .all(|r| r.statistical_significance.is_none()));
    }

    #[test]
    fn unusable_frames_are_skipped_and_reported() {
        let result = HierarchicalAnalyzer::new("test", "v1").analyze(&observation(), |frame| {
            if frame.timeframe == Timeframe::M15 {
                None
            } else {
                Some(analysis(frame.timeframe, Direction::Bullish, 0.9))
            }
        });

        assert_eq!(result.uncertainty.usable_frames, 2);
        assert_eq!(result.uncertainty.requested_frames, 3);
        assert_eq!(result.timeframe_results.len(), 2);
        assert!(!result.uncertainty.probability_calibrated);
        assert!(result
            .uncertainty
            .limitations
            .iter()
            .any(|l| l.contains("probability")));
    }

    #[test]
    fn probabilities_are_validated_and_never_invented() {
        assert!(DirectionDistribution::new(0.5, 0.3, 0.2, CalibrationStatus::Empirical).is_ok());
        assert!(DirectionDistribution::new(0.5, 0.5, 0.5, CalibrationStatus::Empirical).is_err());
        assert!(DirectionDistribution::new(-0.1, 0.6, 0.5, CalibrationStatus::Unverified).is_err());
        let distribution =
            DirectionDistribution::new(0.2, 0.5, 0.3, CalibrationStatus::Unverified).unwrap();
        assert_eq!(distribution.argmax(), Direction::Bearish);
    }

    #[test]
    fn evidence_strength_is_clamped_not_treated_as_probability() {
        let value = TimeframeAnalysis::new(Timeframe::M5, Direction::Bullish, 85.0, -1.0).unwrap();
        assert_eq!(value.evidence_strength, 1.0);
        assert_eq!(value.data_quality, 0.0);
    }
}
