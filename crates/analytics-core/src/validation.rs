//! Chronological validation of hierarchical versus single-timeframe analysis.
//!
//! This is the research harness required before any multiscale discipline can
//! be promoted. It deliberately reports *descriptive* performance only:
//!
//! * outcomes are computed offline from future candles, after the decision,
//!   while every prediction is built from candles that closed before it;
//! * train and test periods are chronological, with an embargo gap so
//!   overlapping forward returns cannot leak across the boundary;
//! * a final untouched test segment is the only one scored;
//! * evidence strength is never treated as a calibrated probability, so the
//!   error metric is explicitly labelled uncalibrated.
//!
//! Nothing here decides that a discipline is profitable. A better hit rate on
//! one synthetic or historical sample is evidence to investigate, not a
//! trading edge.

use serde::{Deserialize, Serialize};

use crate::disciplines::analyze_hierarchy;
use crate::hierarchy::Direction;
use crate::multiscale::MultiTimeframeObservation;
use crate::types::{Candle, Timeframe};

/// Validation configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// Forward horizon for labels, counted in bars of the finest timeframe.
    pub horizon_bars: usize,
    /// Absolute return below which an outcome is labelled neutral.
    pub neutral_threshold: f64,
    /// Minimum completed candles a frame needs before it can be analysed.
    pub min_completed: usize,
    /// Share of decisions reserved for training before the embargo.
    pub train_fraction: f64,
    /// Number of finest-frame bars discarded across the train/test boundary.
    pub embargo_bars: usize,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            horizon_bars: 6,
            neutral_threshold: 0.002,
            min_completed: 30,
            train_fraction: 0.6,
            embargo_bars: 6,
        }
    }
}

/// A label computed from candles that occur after a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardOutcome {
    /// Decision instant, in unix nanoseconds.
    pub decision_time: i64,
    /// Return from the last close known at the decision to the horizon close.
    pub return_fraction: f64,
    /// Direction implied by the return and the neutral threshold.
    pub direction: Direction,
}

impl ForwardOutcome {
    /// Label one decision using only candles strictly after it.
    ///
    /// This function is for offline research: it reads the future on purpose.
    /// Predictions never call it.
    pub fn label(
        series: &[Candle],
        decision_time: i64,
        horizon_ns: i64,
        neutral_threshold: f64,
    ) -> Option<Self> {
        let entry = series
            .iter()
            .filter(|c| c.open_time.saturating_add(c.timeframe.nanos()) <= decision_time)
            .max_by_key(|c| c.open_time)?;
        let exit = series
            .iter()
            .filter(|c| {
                c.open_time > decision_time
                    && c.open_time <= decision_time.saturating_add(horizon_ns)
            })
            .max_by_key(|c| c.open_time)?;
        if entry.close <= 0.0 || !entry.close.is_finite() || !exit.close.is_finite() {
            return None;
        }
        let return_fraction = exit.close / entry.close - 1.0;
        let direction = if return_fraction > neutral_threshold {
            Direction::Bullish
        } else if return_fraction < -neutral_threshold {
            Direction::Bearish
        } else {
            Direction::Neutral
        };
        Some(Self {
            decision_time,
            return_fraction,
            direction,
        })
    }
}

/// Chronological split used by the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitKind {
    /// Before the embargo; used only to decide a variant, never scored here.
    Train,
    /// Inside the embargo window; never used.
    Embargo,
    /// Untouched evaluation segment; the only scored decisions.
    Test,
}

/// Performance of one hierarchy variant on the test segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantReport {
    /// Stable variant name, e.g. `single`, `full` or `ablation_M15`.
    pub name: String,
    /// Timeframes used, coarsest first.
    pub timeframes: Vec<Timeframe>,
    /// Number of scored test decisions.
    pub samples: usize,
    /// Number of directional predictions among them.
    pub directional_samples: usize,
    /// Correct directional predictions / directional predictions.
    pub hit_rate: Option<f64>,
    /// Predictions that were neutral or wrong against a directional outcome.
    pub false_signal_rate: f64,
    /// Mean absolute gap between evidence strength and outcome magnitude.
    ///
    /// The target is **not** a calibrated probability; this number only says
    /// whether strength tends to move with the size of the observed move.
    pub uncalibrated_strength_error: Option<f64>,
}

/// Full comparison report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Configuration used.
    pub config: ValidationConfig,
    /// Number of decisions in each split.
    pub split_counts: SplitCounts,
    /// Reports for each hierarchy variant.
    pub variants: Vec<VariantReport>,
    /// Statements about what this report does not establish.
    pub limitations: Vec<String>,
}

/// Decision counts per split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitCounts {
    /// Decisions in the training segment.
    pub train: usize,
    /// Decisions discarded by the embargo.
    pub embargo: usize,
    /// Decisions in the scored test segment.
    pub test: usize,
}

/// Which split a decision index belongs to.
#[must_use]
pub fn split_for(index: usize, total: usize, config: &ValidationConfig) -> SplitKind {
    if total == 0 {
        return SplitKind::Embargo;
    }
    let train_end = ((total as f64 * config.train_fraction.clamp(0.0, 1.0)) as usize).min(total);
    let test_start = train_end.saturating_add(config.embargo_bars).min(total);
    if index < train_end {
        SplitKind::Train
    } else if index < test_start {
        SplitKind::Embargo
    } else {
        SplitKind::Test
    }
}

fn count_splits(total: usize, config: &ValidationConfig) -> SplitCounts {
    let mut counts = SplitCounts {
        train: 0,
        embargo: 0,
        test: 0,
    };
    for index in 0..total {
        match split_for(index, total, config) {
            SplitKind::Train => counts.train += 1,
            SplitKind::Embargo => counts.embargo += 1,
            SplitKind::Test => counts.test += 1,
        }
    }
    counts
}

fn completed_subset(candles: &[Candle], timeframe: Timeframe, decision_time: i64) -> Vec<Candle> {
    candles
        .iter()
        .filter(|c| {
            c.timeframe == timeframe
                && c.open_time
                    .saturating_add(timeframe.nanos())
                    .le(&decision_time)
        })
        .cloned()
        .collect()
}

/// Variant ladder names and their timeframes, coarsest first.
fn variants_for(ladder: &[Timeframe]) -> Vec<(String, Vec<Timeframe>)> {
    let mut ordered = ladder.to_vec();
    ordered.sort_by(|a, b| b.cmp(a));
    ordered.dedup();
    if ordered.is_empty() {
        return Vec::new();
    }
    let finest = *ordered.last().unwrap();
    let mut variants = vec![("single".to_string(), vec![finest])];
    if ordered.len() >= 2 {
        variants.push((
            "two_frame".to_string(),
            vec![ordered[ordered.len() - 2], finest],
        ));
    }
    if ordered.len() >= 3 {
        variants.push((
            "three_frame".to_string(),
            vec![
                ordered[ordered.len() - 3],
                ordered[ordered.len() - 2],
                finest,
            ],
        ));
    }
    variants.push(("full".to_string(), ordered.clone()));
    for removed in &ordered {
        if ordered.len() > 1 {
            let kept: Vec<Timeframe> = ordered.iter().copied().filter(|tf| tf != removed).collect();
            variants.push((format!("ablation_{removed}"), kept));
        }
    }
    variants
}

fn evaluate_variant(
    name: String,
    timeframes: Vec<Timeframe>,
    series: &[(Timeframe, Vec<Candle>)],
    decisions: &[i64],
    outcomes: &[Option<ForwardOutcome>],
    split_counts: SplitCounts,
    discipline: &str,
    config: &ValidationConfig,
) -> VariantReport {
    let mut samples = 0usize;
    let mut directional = 0usize;
    let mut hits = 0usize;
    let mut false_signals = 0usize;
    let mut errors = Vec::new();
    let total = decisions.len();

    for (index, decision) in decisions.iter().enumerate() {
        if split_for(index, total, config) != SplitKind::Test {
            continue;
        }
        let Some(outcome) = &outcomes[index] else {
            continue;
        };
        let instrument_id = series
            .iter()
            .find_map(|(_, candles)| candles.first().map(|c| c.symbol.clone()))
            .unwrap_or_else(|| "validation".to_string());
        let mut frames = Vec::with_capacity(timeframes.len());
        for timeframe in &timeframes {
            let Some((_, candles)) = series.iter().find(|(tf, _)| tf == timeframe) else {
                continue;
            };
            let subset = completed_subset(candles, *timeframe, *decision);
            if subset.len() < config.min_completed {
                continue;
            }
            frames.push(
                crate::multiscale::TimeframeObservation::from_candles(
                    *timeframe,
                    &instrument_id,
                    &subset,
                    *decision,
                    "validation",
                    config.min_completed,
                )
                .ok()
                .filter(|frame| frame.availability.is_usable()),
            );
        }
        let frames: Vec<_> = frames.into_iter().flatten().collect();
        if frames.is_empty() {
            continue;
        }
        let observation = MultiTimeframeObservation::new(instrument_id, *decision, frames);
        let Ok(observation) = observation else {
            continue;
        };
        let Some(result) = analyze_hierarchy(&observation, discipline, "validation") else {
            continue;
        };
        let Some(prediction) = result.conditioned_lower.as_ref() else {
            continue;
        };
        samples += 1;
        if prediction.direction != Direction::Neutral {
            directional += 1;
            if prediction.direction == outcome.direction {
                hits += 1;
            } else {
                false_signals += 1;
            }
        } else if outcome.direction != Direction::Neutral {
            false_signals += 1;
        }
        let target =
            (outcome.return_fraction.abs() / (config.neutral_threshold * 3.0)).clamp(0.0, 1.0);
        let error = (prediction.evidence_strength - target).abs();
        if error.is_finite() {
            errors.push(error);
        }
    }

    let _ = split_counts; // retained in the report for provenance
    let hit_rate = if directional == 0 {
        None
    } else {
        Some(hits as f64 / directional as f64)
    };
    let false_signal_rate = if samples == 0 {
        0.0
    } else {
        false_signals as f64 / samples as f64
    };
    let uncalibrated_strength_error = if errors.is_empty() {
        None
    } else {
        Some(errors.iter().sum::<f64>() / errors.len() as f64)
    };

    VariantReport {
        name,
        timeframes,
        samples,
        directional_samples: directional,
        hit_rate,
        false_signal_rate,
        uncalibrated_strength_error,
    }
}

/// Compare hierarchy variants on an untouched chronological test segment.
///
/// `series` is keyed by timeframe and must include the finest ladder frame;
/// other missing frames are skipped by each variant. A report with zero test
/// samples is returned honestly rather than fabricated.
pub fn compare_hierarchies(
    series: &[(Timeframe, Vec<Candle>)],
    ladder: &[Timeframe],
    discipline: &str,
    config: &ValidationConfig,
) -> Result<ValidationReport, String> {
    if series.is_empty() || ladder.is_empty() {
        return Err("validation needs at least one series and one timeframe".to_string());
    }
    if config.horizon_bars == 0 || config.neutral_threshold <= 0.0 {
        return Err("validation needs a positive horizon and neutral threshold".to_string());
    }
    let finest = ladder.iter().min().copied().ok_or("empty ladder")?;
    let fine_candles = series
        .iter()
        .find(|(tf, _)| *tf == finest)
        .map(|(_, candles)| candles)
        .ok_or("the finest ladder timeframe has no series")?;
    if fine_candles.is_empty() {
        return Err("the finest ladder timeframe has no candles".to_string());
    }

    let horizon_ns = finest
        .nanos()
        .saturating_mul(config.horizon_bars as i64)
        .max(finest.nanos() as i64);
    let decisions: Vec<i64> = fine_candles
        .iter()
        .map(|c| c.open_time.saturating_add(finest.nanos()))
        .collect();
    let outcomes: Vec<Option<ForwardOutcome>> = decisions
        .iter()
        .map(|decision| {
            ForwardOutcome::label(
                fine_candles,
                *decision,
                horizon_ns,
                config.neutral_threshold,
            )
        })
        .collect();
    let split_counts = count_splits(decisions.len(), config);

    let variants = variants_for(ladder)
        .into_iter()
        .map(|(name, timeframes)| {
            evaluate_variant(
                name,
                timeframes,
                series,
                &decisions,
                &outcomes,
                split_counts,
                discipline,
                config,
            )
        })
        .collect();

    Ok(ValidationReport {
        config: config.clone(),
        split_counts,
        variants,
        limitations: vec![
            "only the untouched test segment is scored".to_string(),
            "evidence strength is not a calibrated probability".to_string(),
            "no trading costs or slippage are modelled".to_string(),
            "performance on one sample is not evidence of a persistent edge".to_string(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000_000_000;

    fn series(timeframe: Timeframe, count: usize) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.2 + (i % 7) as f64 * 0.05;
                Candle {
                    symbol: "BTCUSDT".to_string(),
                    timeframe,
                    open_time: i as i64 * timeframe.nanos(),
                    open: close,
                    high: close + 0.5,
                    low: close - 0.5,
                    close,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect()
    }

    #[test]
    fn splits_are_chronological_with_an_embargo() {
        let config = ValidationConfig {
            train_fraction: 0.5,
            embargo_bars: 3,
            ..ValidationConfig::default()
        };
        let kinds: Vec<SplitKind> = (0..20).map(|i| split_for(i, 20, &config)).collect();
        assert_eq!(kinds[..10], [SplitKind::Train; 10]);
        assert_eq!(kinds[10..13], [SplitKind::Embargo; 3]);
        assert_eq!(kinds[13..], [SplitKind::Test; 7]);
        let counts = count_splits(20, &config);
        assert_eq!(
            counts,
            SplitCounts {
                train: 10,
                embargo: 3,
                test: 7
            }
        );
    }

    #[test]
    fn labels_are_computed_only_from_future_candles() {
        let candles = series(Timeframe::M1, 20);
        let decision = 5 * MIN;
        let label = ForwardOutcome::label(&candles, decision, 4 * MIN, 0.001).unwrap();
        assert!(label.return_fraction > 0.0);
        assert_eq!(label.direction, Direction::Bullish);

        // A decision after the last candle cannot be labelled.
        assert!(ForwardOutcome::label(&candles, 100 * MIN, 4 * MIN, 0.001).is_none());
    }

    #[test]
    fn completed_subset_never_contains_a_future_or_forming_candle() {
        let candles = series(Timeframe::M1, 10);
        let subset = completed_subset(&candles, Timeframe::M1, 5 * MIN);
        assert_eq!(subset.len(), 5);
        assert!(subset
            .iter()
            .all(|c| c.open_time.saturating_add(MIN) <= 5 * MIN));
    }

    #[test]
    fn variants_include_baselines_and_ablations() {
        let variants = variants_for(&[Timeframe::H1, Timeframe::M15, Timeframe::M5]);
        let names: Vec<&str> = variants.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"single"));
        assert!(names.contains(&"two_frame"));
        assert!(names.contains(&"three_frame"));
        assert!(names.contains(&"full"));
        assert!(names.contains(&"ablation_15m"));
        assert!(variants
            .iter()
            .all(|(_, frames)| frames.windows(2).all(|w| w[0] > w[1])));
    }

    #[test]
    fn comparison_reports_only_the_test_segment() {
        let config = ValidationConfig {
            horizon_bars: 4,
            neutral_threshold: 0.001,
            min_completed: 20,
            train_fraction: 0.5,
            embargo_bars: 3,
        };
        let ladder = [Timeframe::M15, Timeframe::M1];
        let series = vec![
            (Timeframe::M15, series(Timeframe::M15, 100)),
            (Timeframe::M1, series(Timeframe::M1, 100)),
        ];
        let report = compare_hierarchies(&series, &ladder, "bayesian", &config).unwrap();

        assert!(report.split_counts.test > 0);
        assert!(report.variants.len() >= 3);
        assert!(report
            .variants
            .iter()
            .all(|v| v.samples <= report.split_counts.test));
        assert!(report.variants.iter().all(|v| {
            v.hit_rate.is_none_or(|rate| (0.0..=1.0).contains(&rate))
                && v.false_signal_rate >= 0.0
                && v.uncalibrated_strength_error.is_none_or(|e| e >= 0.0)
        }));
        assert!(report
            .limitations
            .iter()
            .any(|limit| limit.contains("probability")));
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        let config = ValidationConfig {
            horizon_bars: 0,
            ..ValidationConfig::default()
        };
        let series = vec![(Timeframe::M1, series(Timeframe::M1, 40))];
        assert!(compare_hierarchies(&series, &[Timeframe::M1], "bayesian", &config).is_err());
        assert!(compare_hierarchies(
            &[],
            &[Timeframe::M1],
            "bayesian",
            &ValidationConfig::default()
        )
        .is_err());
    }

    #[test]
    fn results_are_deterministic() {
        let config = ValidationConfig {
            horizon_bars: 4,
            neutral_threshold: 0.001,
            min_completed: 20,
            train_fraction: 0.5,
            embargo_bars: 3,
        };
        let series = vec![
            (Timeframe::M15, series(Timeframe::M15, 80)),
            (Timeframe::M1, series(Timeframe::M1, 80)),
        ];
        let first = compare_hierarchies(
            &series,
            &[Timeframe::M15, Timeframe::M1],
            "bayesian",
            &config,
        )
        .unwrap();
        let second = compare_hierarchies(
            &series,
            &[Timeframe::M15, Timeframe::M1],
            "bayesian",
            &config,
        )
        .unwrap();
        assert_eq!(first, second);
    }
}
