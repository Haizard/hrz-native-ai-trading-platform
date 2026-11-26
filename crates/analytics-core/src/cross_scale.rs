//! Cross-timeframe dependence and multiscale market reconstruction.
//!
//! Running one model per timeframe is not hierarchical. This module measures
//! whether a coarser frame's bar direction carries information about a finer
//! frame's bar direction, using only discrete categorical mutual information:
//! no learned weights, no randomness, no assumptions about normality.
//!
//! The measurement is deliberately descriptive. Finite samples bias mutual
//! information upward, `samples` is always reported beside it, and
//! significance is never manufactured — out-of-sample research must supply it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::hierarchy::Direction;
use crate::multiscale::{MultiTimeframeObservation, TimeframeObservation};
use crate::types::{Candle, Timeframe};

/// Discrete direction of one bar, used as the categorical variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BarState {
    /// The bar closed above its open.
    Bullish,
    /// The bar closed below its open.
    Bearish,
    /// The bar closed at its open.
    Neutral,
}

impl BarState {
    /// Classify a candle by its own open and close.
    #[must_use]
    pub const fn of(candle: &Candle) -> Self {
        if candle.close > candle.open {
            Self::Bullish
        } else if candle.close < candle.open {
            Self::Bearish
        } else {
            Self::Neutral
        }
    }

    /// Direction equivalent used by the standard result types.
    #[must_use]
    pub const fn direction(self) -> Direction {
        match self {
            Self::Bullish => Direction::Bullish,
            Self::Bearish => Direction::Bearish,
            Self::Neutral => Direction::Neutral,
        }
    }
}

/// Measured dependence between two frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossScaleDependency {
    /// Coarser frame.
    pub higher_timeframe: Timeframe,
    /// Finer frame.
    pub lower_timeframe: Timeframe,
    /// Mutual information in bits.
    pub mutual_information_bits: f64,
    /// Conditional entropy of the lower state given the higher state, in bits.
    pub conditional_entropy_bits: f64,
    /// Mutual information divided by the smaller marginal entropy.
    pub normalized_dependence: f64,
    /// Number of aligned lower bars behind the estimate.
    pub samples: usize,
    /// Always `None`: significance requires out-of-sample evaluation.
    pub statistical_significance: Option<f64>,
}

/// One frame's reconstructed condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconstructedFrame {
    /// Frame resolution.
    pub timeframe: Timeframe,
    /// Direction from the frame's own completed bars.
    pub state: BarState,
    /// Share of completed bars leaning that way, in `[0, 1]`.
    pub coherence: f64,
    /// Data quality of the frame, in `[0, 1]`.
    pub data_quality: f64,
}

/// A reconstructed multiscale condition with its measured dependencies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiscaleReconstruction {
    /// Instrument analysed.
    pub instrument_id: String,
    /// Decision instant, in unix nanoseconds.
    pub decision_timestamp: i64,
    /// Per-frame conditions, coarsest first.
    pub frames: Vec<ReconstructedFrame>,
    /// Pairwise categorical dependencies.
    pub dependencies: Vec<CrossScaleDependency>,
    /// Majority direction across frames; neutral on a tie.
    pub consensus: Direction,
    /// Frames leaning with the consensus.
    pub supporting_frames: Vec<Timeframe>,
    /// Frames leaning against the consensus.
    pub conflicting_frames: Vec<Timeframe>,
}

fn entropy(counts: &HashMap<BarState, usize>, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    counts
        .values()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

/// Measure bar-state dependence between two frames of one observation.
///
/// Each lower bar is mapped to the higher bucket that contains its open time.
/// The higher bar's open/close direction and the lower bar's direction form a
/// 3x3 contingency table. `None` means there was no usable overlap.
pub fn measure_dependency(
    observation: &MultiTimeframeObservation,
    higher_timeframe: Timeframe,
    lower_timeframe: Timeframe,
) -> Option<CrossScaleDependency> {
    if higher_timeframe <= lower_timeframe {
        return None;
    }
    let higher = observation.frame(higher_timeframe)?;
    let lower = observation.frame(lower_timeframe)?;
    let higher_buckets: HashMap<i64, BarState> = higher
        .completed()
        .iter()
        .map(|candle| {
            (
                higher_timeframe.bucket_of(candle.open_time),
                BarState::of(candle),
            )
        })
        .collect();
    if higher_buckets.is_empty() || lower.completed().is_empty() {
        return None;
    }

    let mut joint: HashMap<(BarState, BarState), usize> = HashMap::new();
    let mut higher_counts: HashMap<BarState, usize> = HashMap::new();
    let mut lower_counts: HashMap<BarState, usize> = HashMap::new();
    let mut samples = 0usize;
    for candle in lower.completed() {
        let bucket = higher_timeframe.bucket_of(candle.open_time);
        // Ladders rarely cover identical spans: a lower bar whose higher
        // bucket was not supplied is simply not a paired observation. Skipping
        // it is honest; aborting the whole measurement would discard every
        // aligned bar because of one missing bucket.
        let Some(high_state) = higher_buckets.get(&bucket) else {
            continue;
        };
        let high_state = *high_state;
        let low_state = BarState::of(candle);
        *joint.entry((high_state, low_state)).or_default() += 1;
        *higher_counts.entry(high_state).or_default() += 1;
        *lower_counts.entry(low_state).or_default() += 1;
        samples += 1;
    }
    if samples == 0 {
        return None;
    }

    let mut mutual_information = 0.0;
    for ((high_state, low_state), count) in &joint {
        let p_xy = *count as f64 / samples as f64;
        let p_x = higher_counts[high_state] as f64 / samples as f64;
        let p_y = lower_counts[low_state] as f64 / samples as f64;
        mutual_information += p_xy * (p_xy / (p_x * p_y)).log2();
    }
    let mutual_information = mutual_information.max(0.0);
    let entropy_high = entropy(&higher_counts, samples);
    let entropy_low = entropy(&lower_counts, samples);
    let denominator = entropy_high.min(entropy_low);
    let normalized_dependence = if denominator > 0.0 {
        (mutual_information / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Some(CrossScaleDependency {
        higher_timeframe,
        lower_timeframe,
        mutual_information_bits: mutual_information,
        conditional_entropy_bits: (entropy_low - mutual_information).max(0.0),
        normalized_dependence,
        samples,
        statistical_significance: None,
    })
}

fn reconstruct_frame(frame: &TimeframeObservation) -> ReconstructedFrame {
    let candles = frame.completed();
    let (bullish, bearish) = candles.iter().fold((0usize, 0usize), |(up, down), candle| {
        match BarState::of(candle) {
            BarState::Bullish => (up + 1, down),
            BarState::Bearish => (up, down + 1),
            BarState::Neutral => (up, down),
        }
    });
    let directional = bullish + bearish;
    let state = if bullish > bearish {
        BarState::Bullish
    } else if bearish > bullish {
        BarState::Bearish
    } else {
        BarState::Neutral
    };
    let coherence = if directional == 0 {
        0.0
    } else {
        (bullish.max(bearish) as f64) / directional as f64
    };
    ReconstructedFrame {
        timeframe: frame.timeframe,
        state,
        coherence,
        data_quality: frame.data_quality.score(),
    }
}

/// Reconstruct each frame's condition and measure every pair of frames.
pub fn reconstruct(observation: &MultiTimeframeObservation) -> MultiscaleReconstruction {
    let frames: Vec<ReconstructedFrame> =
        observation.frames.iter().map(reconstruct_frame).collect();

    let mut dependencies = Vec::new();
    for (higher_index, higher) in observation.frames.iter().enumerate() {
        for lower in observation.frames.iter().skip(higher_index + 1) {
            if let Some(dependency) =
                measure_dependency(observation, higher.timeframe, lower.timeframe)
            {
                dependencies.push(dependency);
            }
        }
    }

    let bullish = frames
        .iter()
        .filter(|f| f.state == BarState::Bullish)
        .count();
    let bearish = frames
        .iter()
        .filter(|f| f.state == BarState::Bearish)
        .count();
    let consensus = if bullish > bearish {
        Direction::Bullish
    } else if bearish > bullish {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    let (supporting_frames, conflicting_frames) = frames
        .iter()
        .filter(|frame| frame.state != BarState::Neutral)
        .fold(
            (Vec::new(), Vec::new()),
            |(mut support, mut conflict), frame| {
                if frame.state.direction() == consensus {
                    support.push(frame.timeframe);
                } else {
                    conflict.push(frame.timeframe);
                }
                (support, conflict)
            },
        );

    MultiscaleReconstruction {
        instrument_id: observation.instrument_id.clone(),
        decision_timestamp: observation.decision_timestamp,
        frames,
        dependencies,
        consensus,
        supporting_frames,
        conflicting_frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiscale::TimeframeObservation as Frame;

    const MIN: i64 = 60_000_000_000;

    fn observation(bullish: bool, count: usize) -> MultiTimeframeObservation {
        let decision = count as i64 * MIN;
        let frames = [Timeframe::H1, Timeframe::M15, Timeframe::M1]
            .into_iter()
            .map(|timeframe| {
                let available = (decision / timeframe.nanos()).clamp(1, count as i64) as usize;
                let candles: Vec<Candle> = (0..available)
                    .map(|i| {
                        let open = 100.0 + i as f64 * 0.1;
                        let close = if bullish { open + 0.5 } else { open - 0.5 };
                        Candle {
                            symbol: "BTCUSDT".to_string(),
                            timeframe,
                            open_time: i as i64 * timeframe.nanos(),
                            open,
                            high: open.max(close) + 0.1,
                            low: open.min(close) - 0.1,
                            close,
                            volume: 10.0,
                            buy_volume: 6.0,
                            sell_volume: 4.0,
                        }
                    })
                    .collect();
                Frame::from_candles(timeframe, "BTCUSDT", &candles, decision, "test", 1).unwrap()
            })
            .collect();
        MultiTimeframeObservation::new("BTCUSDT", decision, frames).unwrap()
    }

    #[test]
    fn all_frames_leaning_the_same_way_give_a_clean_consensus() {
        let result = reconstruct(&observation(true, 120));
        assert_eq!(result.consensus, Direction::Bullish);
        assert_eq!(result.supporting_frames.len(), 3);
        assert!(result.conflicting_frames.is_empty());
        assert!(result.frames.iter().all(|f| f.state == BarState::Bullish));
    }

    #[test]
    fn mixed_frames_report_conflict() {
        let mut observation = observation(true, 120);
        // Flip the finest frame's bars downward while leaving the higher ones up.
        let frame = observation.frames.last_mut().unwrap();
        for candle in &mut frame.candles {
            let open = candle.open;
            candle.close = open - 0.5;
            candle.high = open + 0.1;
            candle.low = open - 0.6;
        }
        let result = reconstruct(&observation);
        assert_eq!(result.consensus, Direction::Bullish);
        assert_eq!(result.conflicting_frames, vec![Timeframe::M1]);
    }

    #[test]
    fn dependencies_are_bounded_and_report_sample_counts() {
        let result = reconstruct(&observation(true, 120));
        assert_eq!(result.dependencies.len(), 3);
        assert!(result.dependencies.iter().all(|dependency| {
            dependency.mutual_information_bits >= 0.0
                && dependency.mutual_information_bits <= 2.0
                && dependency.normalized_dependence >= 0.0
                && dependency.normalized_dependence <= 1.0
                && dependency.conditional_entropy_bits >= 0.0
                && dependency.samples > 0
                && dependency.statistical_significance.is_none()
        }));
    }

    #[test]
    fn equal_or_reversed_frames_have_no_dependency() {
        let observation = observation(true, 60);
        assert!(measure_dependency(&observation, Timeframe::M1, Timeframe::M15).is_none());
        assert!(measure_dependency(&observation, Timeframe::M1, Timeframe::M1).is_none());
    }

    #[test]
    fn reconstruction_is_deterministic() {
        assert_eq!(
            reconstruct(&observation(false, 90)),
            reconstruct(&observation(false, 90))
        );
    }
}
