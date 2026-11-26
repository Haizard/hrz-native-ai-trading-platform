//! Prediction outcome recording, drift detection and promotion decisions.
//!
//! A discipline cannot improve if its predictions are never settled against
//! what actually happened. This module records predictions at decision time,
//! settles them later with labels computed from future candles, and compares
//! recent performance against an earlier baseline using a two-proportion
//! z-statistic. It is deliberately conservative: it never promotes a model by
//! itself, and it never treats an evidence strength as a probability.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::hierarchy::Direction;
use crate::types::{Candle, Timeframe};
use crate::validation::ForwardOutcome;

/// A prediction made at a decision instant, before the outcome was known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictionRecord {
    /// Instrument predicted.
    pub instrument_id: String,
    /// Decision instant, in unix nanoseconds.
    pub decision_timestamp: i64,
    /// Timeframe the prediction describes.
    pub timeframe: Timeframe,
    /// Predicted direction.
    pub direction: Direction,
    /// Evidence strength at decision time; not a probability.
    pub evidence_strength: f64,
    /// Version of the discipline that produced it.
    pub model_version: String,
}

type RecordKey = (String, i64, Timeframe);

/// A prediction joined to its realised outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettledOutcome {
    /// The original prediction.
    pub prediction: PredictionRecord,
    /// Realised forward return.
    pub realized_return: f64,
    /// Direction implied by the realised return.
    pub realized_direction: Direction,
    /// Whether the directional prediction was correct.
    pub hit: bool,
}

/// In-memory ledger of predictions and their settlements.
///
/// The gateway owns persistence in a real deployment; this ledger is the pure
/// deterministic core that decides what a valid record and a valid settlement
/// are. It keeps no clock and reads no database.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutcomeLog {
    predictions: Vec<PredictionRecord>,
    settled: Vec<SettledOutcome>,
    settled_keys: HashSet<RecordKey>,
}

impl OutcomeLog {
    /// Create an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a prediction, rejecting duplicates and non-finite strengths.
    pub fn record(&mut self, prediction: PredictionRecord) -> Result<(), String> {
        if !prediction.evidence_strength.is_finite() {
            return Err("evidence strength must be finite".to_string());
        }
        let key = (
            prediction.instrument_id.clone(),
            prediction.decision_timestamp,
            prediction.timeframe,
        );
        if self
            .predictions
            .iter()
            .any(|existing| Self::key(existing) == key)
        {
            return Err(
                "duplicate prediction for the same instrument, time and timeframe".to_string(),
            );
        }
        self.predictions.push(prediction);
        Ok(())
    }

    fn key(prediction: &PredictionRecord) -> RecordKey {
        (
            prediction.instrument_id.clone(),
            prediction.decision_timestamp,
            prediction.timeframe,
        )
    }

    /// Number of predictions awaiting an outcome.
    #[must_use]
    pub fn unsettled_count(&self) -> usize {
        self.predictions
            .iter()
            .filter(|prediction| !self.settled_keys.contains(&Self::key(prediction)))
            .count()
    }

    /// Settled outcomes, ordered by decision time then insertion.
    #[must_use]
    pub fn settled(&self) -> &[SettledOutcome] {
        &self.settled
    }

    /// Join a prediction to an already-computed offline label.
    pub fn settle_with_outcome(
        &mut self,
        prediction_key: (String, i64, Timeframe),
        outcome: &ForwardOutcome,
    ) -> Result<(), String> {
        let Some(prediction) = self
            .predictions
            .iter()
            .find(|record| Self::key(record) == prediction_key)
            .cloned()
        else {
            return Err("no matching prediction to settle".to_string());
        };
        if prediction.decision_timestamp != outcome.decision_time {
            return Err("outcome belongs to a different decision instant".to_string());
        }
        if !self.settled_keys.insert(prediction_key) {
            return Err("prediction was already settled".to_string());
        }
        let hit =
            prediction.direction != Direction::Neutral && prediction.direction == outcome.direction;
        self.settled.push(SettledOutcome {
            prediction,
            realized_return: outcome.return_fraction,
            realized_direction: outcome.direction,
            hit,
        });
        Ok(())
    }

    /// Settle every unsettled prediction using future candles in `series`.
    ///
    /// Labels are computed offline from candles after each decision; the
    /// original prediction never reads them.
    pub fn settle_from_series(
        &mut self,
        series: &[Candle],
        horizon_ns: i64,
        neutral_threshold: f64,
    ) -> usize {
        let pending: Vec<PredictionRecord> = self
            .predictions
            .iter()
            .filter(|record| !self.settled_keys.contains(&Self::key(record)))
            .cloned()
            .collect();
        let mut settled = 0usize;
        for prediction in pending {
            let Some(outcome) = ForwardOutcome::label(
                series,
                prediction.decision_timestamp,
                horizon_ns,
                neutral_threshold,
            ) else {
                continue;
            };
            if self
                .settle_with_outcome(Self::key(&prediction), &outcome)
                .is_ok()
            {
                settled += 1;
            }
        }
        settled
    }
}

/// Drift detection settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DriftConfig {
    /// Minimum total settled samples before any comparison is meaningful.
    pub min_samples: usize,
    /// Number of most recent settled outcomes treated as the recent window.
    pub recent_window: usize,
    /// Absolute z-score beyond which a change is flagged.
    pub z_threshold: f64,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            min_samples: 40,
            recent_window: 20,
            z_threshold: 2.0,
        }
    }
}

/// Drift comparison between a recent window and the earlier baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftReport {
    /// Total settled samples used.
    pub samples: usize,
    /// Baseline hit rate, when a baseline exists.
    pub baseline_hit_rate: Option<f64>,
    /// Recent-window hit rate, when a window exists.
    pub recent_hit_rate: Option<f64>,
    /// Two-proportion z-statistic, when computable.
    pub z_score: Option<f64>,
    /// Whether the absolute z-score exceeded the configured threshold.
    pub drift_detected: bool,
}

/// Compare the most recent outcomes against the earlier baseline.
#[must_use]
pub fn detect_drift(log: &OutcomeLog, config: &DriftConfig) -> DriftReport {
    let settled = log.settled();
    let samples = settled.len();
    if samples < config.min_samples || config.recent_window == 0 {
        return DriftReport {
            samples,
            baseline_hit_rate: None,
            recent_hit_rate: None,
            z_score: None,
            drift_detected: false,
        };
    }
    let recent_size = config.recent_window.min(samples.saturating_sub(1));
    if recent_size == 0 {
        return DriftReport {
            samples,
            baseline_hit_rate: None,
            recent_hit_rate: None,
            z_score: None,
            drift_detected: false,
        };
    }
    let split = samples - recent_size;
    let baseline_hits = settled[..split]
        .iter()
        .filter(|outcome| outcome.hit)
        .count();
    let recent_hits = settled[split..]
        .iter()
        .filter(|outcome| outcome.hit)
        .count();
    let baseline_rate = baseline_hits as f64 / split as f64;
    let recent_rate = recent_hits as f64 / recent_size as f64;
    let pooled = (baseline_hits + recent_hits) as f64 / samples as f64;
    let variance = pooled * (1.0 - pooled) * (1.0 / split as f64 + 1.0 / recent_size as f64);
    let z_score = if variance > 0.0 {
        Some((recent_rate - baseline_rate) / variance.sqrt())
    } else {
        None
    };
    DriftReport {
        samples,
        baseline_hit_rate: Some(baseline_rate),
        recent_hit_rate: Some(recent_rate),
        z_score,
        drift_detected: z_score.is_some_and(|z| z.abs() > config.z_threshold),
    }
}

/// Promotion decision, requiring explicit operator authorisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionDecision {
    /// True only when the operator permitted promotion and evidence supports it.
    pub promote: bool,
    /// Whether the required sample count was reached.
    pub sufficient_samples: bool,
    /// Whether recent performance improved on the baseline.
    pub improved: bool,
    /// Whether a statistically large change was detected.
    pub drift_detected: bool,
}

/// Evaluate a candidate model against its settled outcomes.
///
/// Even a fully positive result is only a recommendation to the operator: a
/// genuinely untouched holdout period and a risk review remain mandatory.
#[must_use]
pub fn evaluate_promotion(
    log: &OutcomeLog,
    config: &DriftConfig,
    operator_authorized: bool,
) -> PromotionDecision {
    let drift = detect_drift(log, config);
    let sufficient_samples = drift.samples >= config.min_samples;
    let improved = matches!(
        (drift.baseline_hit_rate, drift.recent_hit_rate),
        (Some(baseline), Some(recent)) if recent > baseline
    );
    PromotionDecision {
        promote: operator_authorized && sufficient_samples && improved && !drift.drift_detected,
        sufficient_samples,
        improved,
        drift_detected: drift.drift_detected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, Timeframe};

    const MIN: i64 = 60_000_000_000;

    fn prediction(index: i64, direction: Direction) -> PredictionRecord {
        PredictionRecord {
            instrument_id: "BTCUSDT".to_string(),
            decision_timestamp: index * MIN,
            timeframe: Timeframe::M1,
            direction,
            evidence_strength: 0.7,
            model_version: "v1".to_string(),
        }
    }

    fn label(index: i64, direction: Direction) -> ForwardOutcome {
        ForwardOutcome {
            decision_time: index * MIN,
            return_fraction: match direction {
                Direction::Bullish => 0.01,
                Direction::Bearish => -0.01,
                Direction::Neutral => 0.0,
            },
            direction,
        }
    }

    fn candles() -> Vec<Candle> {
        (0..40)
            .map(|i| Candle {
                symbol: "BTCUSDT".to_string(),
                timeframe: Timeframe::M1,
                open_time: i * MIN,
                open: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                close: 100.5 + i as f64,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            })
            .collect()
    }

    #[test]
    fn a_prediction_is_recorded_once_and_settled_once() {
        let mut log = OutcomeLog::new();
        let record = prediction(5, Direction::Bullish);
        log.record(record.clone()).unwrap();
        assert_eq!(log.unsettled_count(), 1);
        assert_eq!(
            log.record(record.clone()).unwrap_err(),
            "duplicate prediction for the same instrument, time and timeframe"
        );
        log.settle_with_outcome(
            ("BTCUSDT".to_string(), 5 * MIN, Timeframe::M1),
            &label(5, Direction::Bullish),
        )
        .unwrap();
        assert_eq!(log.unsettled_count(), 0);
        assert!(log.settled()[0].hit);
        assert_eq!(
            log.settle_with_outcome(
                ("BTCUSDT".to_string(), 5 * MIN, Timeframe::M1),
                &label(5, Direction::Bullish),
            )
            .unwrap_err(),
            "prediction was already settled"
        );
    }

    #[test]
    fn settlement_rejects_a_mismatched_decision_instant() {
        let mut log = OutcomeLog::new();
        log.record(prediction(5, Direction::Bullish)).unwrap();
        let error = log
            .settle_with_outcome(
                ("BTCUSDT".to_string(), 5 * MIN, Timeframe::M1),
                &label(6, Direction::Bullish),
            )
            .unwrap_err();
        assert_eq!(error, "outcome belongs to a different decision instant");
    }

    #[test]
    fn settling_from_series_uses_future_candles_only() {
        let mut log = OutcomeLog::new();
        for index in 1..10 {
            log.record(prediction(index, Direction::Bullish)).unwrap();
        }
        let count = log.settle_from_series(&candles(), 2 * MIN, 0.001);
        assert!(count > 0);
        assert!(log.settled().iter().all(|outcome| outcome.hit));
    }

    #[test]
    fn drift_needs_enough_samples() {
        let mut log = OutcomeLog::new();
        for index in 0..10 {
            log.record(prediction(index, Direction::Bullish)).unwrap();
            log.settle_with_outcome(
                ("BTCUSDT".to_string(), index * MIN, Timeframe::M1),
                &label(index, Direction::Bullish),
            )
            .unwrap();
        }
        let report = detect_drift(&log, &DriftConfig::default());
        assert_eq!(report.samples, 10);
        assert!(report.z_score.is_none());
        assert!(!report.drift_detected);
    }

    #[test]
    fn drift_detects_a_degradation() {
        let mut log = OutcomeLog::new();
        for index in 0..60 {
            let predicted = Direction::Bullish;
            log.record(prediction(index, predicted)).unwrap();
            let realized = if index < 40 {
                Direction::Bullish
            } else {
                Direction::Bearish
            };
            log.settle_with_outcome(
                ("BTCUSDT".to_string(), index * MIN, Timeframe::M1),
                &label(index, realized),
            )
            .unwrap();
        }
        let config = DriftConfig {
            min_samples: 40,
            recent_window: 20,
            z_threshold: 2.0,
        };
        let report = detect_drift(&log, &config);
        assert_eq!(report.baseline_hit_rate, Some(1.0));
        assert_eq!(report.recent_hit_rate, Some(0.0));
        assert!(report.drift_detected);
    }

    #[test]
    fn promotion_requires_operator_authorisation() {
        let mut log = OutcomeLog::new();
        for index in 0..60 {
            log.record(prediction(index, Direction::Bullish)).unwrap();
            log.settle_with_outcome(
                ("BTCUSDT".to_string(), index * MIN, Timeframe::M1),
                &label(index, Direction::Bullish),
            )
            .unwrap();
        }
        let config = DriftConfig::default();
        let without_authorisation = evaluate_promotion(&log, &config, false);
        assert!(!without_authorisation.promote);
        assert!(without_authorisation.sufficient_samples);

        // Equal rates are not an improvement, so promotion stays refused even
        // with authorisation.
        let with_authorisation = evaluate_promotion(&log, &config, true);
        assert!(!with_authorisation.promote);
        assert!(!with_authorisation.improved);
    }
}
