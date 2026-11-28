//! Hierarchical Markov intelligence across timeframes.
//!
//! `analytics-core` stays a leaf crate: the coupling lives here, where it can
//! depend on both the shared observation type and the seven Markov engines.
//! Each frame still runs its own engine; the hierarchy then conditions the
//! finest frame's regime probabilities on the coarser frames' states. This is
//! the deterministic counterpart of the roadmap's coupled/switching state-space
//! model — it does not claim to be a fitted hierarchical HMM.

pub mod hierarchical_hmm;

pub use hierarchical_hmm::{
    fit_hierarchical_hmm, observation_sequence, FittedHierarchicalHmm, HierarchicalHmmConfig,
    HierarchicalHmmResult, HmmPath,
};

use std::collections::BTreeMap;

use analytics_core::calculate_vwap;
use analytics_core::indicators::atr::atr;
use analytics_core::multiscale::MultiTimeframeObservation;
use analytics_core::types::{Candle, Timeframe};
use markov_intelligence::state::{MarketFeatures, Regime};
use markov_intelligence::{analyze, MarkovRequest, MarkovResult};
use serde::{Deserialize, Serialize};

/// Configuration shared by every frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HierarchicalMarkovConfig {
    /// Minimum completed candles before a frame is analysed.
    pub min_bars: usize,
    /// ATR window used for the volatility reference.
    pub atr_period: usize,
    /// Horizon passed to the continuous-time chain, in bars.
    pub horizon: f64,
}

impl Default for HierarchicalMarkovConfig {
    fn default() -> Self {
        Self {
            min_bars: 30,
            atr_period: 14,
            horizon: 6.0,
        }
    }
}

/// One frame's Markov evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMarkov {
    /// Frame resolution.
    pub timeframe: Timeframe,
    /// The seven-engine result.
    pub result: MarkovResult,
    /// Bullish probability minus bearish probability, in `[-1, 1]`.
    pub directional_bias: f64,
}

/// Result of a hierarchical Markov read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HierarchicalMarkovReport {
    /// Instrument analysed.
    pub instrument: String,
    /// Per-frame evidence, coarsest first.
    pub frames: Vec<FrameMarkov>,
    /// Probabilities after conditioning on higher frames.
    pub conditioned_probabilities: BTreeMap<String, f64>,
    /// Conditioned directional bias, in `[-1, 1]`.
    pub conditioned_bias: f64,
    /// Explicit limitation of the coupling.
    pub limitation: String,
}

fn features_for(
    candles: &[Candle],
    config: &HierarchicalMarkovConfig,
) -> Option<(MarketFeatures, Vec<f64>)> {
    if candles.len() < config.min_bars {
        return None;
    }
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let returns: Vec<f64> = closes
        .windows(2)
        .filter(|pair| pair[0] > 0.0)
        .map(|pair| pair[1] / pair[0] - 1.0)
        .collect();
    let atr_series = atr(candles, config.atr_period.min(candles.len()));
    let atr_now = atr_series
        .iter()
        .rev()
        .find_map(|v| v.filter(|v| v.is_finite() && *v > 0.0))?;
    let last = candles.last()?;
    let volume_sum: f64 = candles.iter().map(|c| c.volume.max(0.0)).sum();
    let mean_volume = volume_sum / candles.len() as f64;
    let vwap = calculate_vwap(candles)?;
    let features = MarketFeatures {
        return_1: returns.last().copied().unwrap_or(0.0),
        atr_pct: if last.close > 0.0 {
            atr_now / last.close
        } else {
            0.0
        },
        volume_ratio: if mean_volume > 0.0 {
            last.volume / mean_volume
        } else {
            1.0
        },
        momentum: if closes.len() > 10 {
            closes.last().copied().unwrap_or(0.0) / closes[closes.len() - 10].max(f64::EPSILON)
                - 1.0
        } else {
            0.0
        },
        delta: if last.volume > 0.0 {
            last.delta() / last.volume
        } else {
            0.0
        },
        distance_from_vwap: if vwap > 0.0 {
            last.close / vwap - 1.0
        } else {
            0.0
        },
        spread: 0.0,
    };
    if features.vector().iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some((features, returns))
}

fn classify(features: MarketFeatures, reference: f64) -> Regime {
    let atr_reference = reference.max(1e-9);
    if features.atr_pct > atr_reference * 1.5 {
        return Regime::HighVolatility;
    }
    if features.atr_pct < atr_reference * 0.5 {
        return Regime::LowVolatility;
    }
    let direction = features.momentum + features.delta + features.distance_from_vwap;
    if direction > 0.0 {
        Regime::BullTrend
    } else if direction < 0.0 {
        Regime::BearTrend
    } else {
        Regime::Sideways
    }
}

fn bias(probabilities: &BTreeMap<String, f64>) -> f64 {
    let bull = probabilities
        .get("BullTrend")
        .or_else(|| probabilities.get("bull_trend"))
        .copied()
        .unwrap_or(0.0);
    let bear = probabilities
        .get("BearTrend")
        .or_else(|| probabilities.get("bear_trend"))
        .copied()
        .unwrap_or(0.0);
    (bull - bear).clamp(-1.0, 1.0)
}

/// Run all seven Markov engines on each frame, then couple their regimes.
pub fn analyze_hierarchy(
    observation: &MultiTimeframeObservation,
    config: &HierarchicalMarkovConfig,
) -> Result<HierarchicalMarkovReport, String> {
    let mut frames = Vec::with_capacity(observation.frames.len());
    for frame in &observation.frames {
        let (features, returns) = features_for(frame.completed(), config).ok_or_else(|| {
            format!(
                "{} has too little completed data for Markov analysis",
                frame.timeframe
            )
        })?;
        let reference = features.atr_pct;
        let history = frame
            .completed()
            .windows(config.min_bars)
            .filter_map(|window| {
                features_for(window, config).map(|(features, _)| classify(features, reference))
            })
            .collect();
        let request = MarkovRequest {
            instrument: observation.instrument_id.clone(),
            timeframe: frame.timeframe.to_string(),
            features,
            returns,
            regime_history: history,
            atr_reference: reference.max(1e-9),
            horizon: config.horizon,
            gamma: 0.99,
        };
        let result = analyze(&request)?;
        frames.push(FrameMarkov {
            timeframe: frame.timeframe,
            directional_bias: bias(&result.regime_probabilities),
            result,
        });
    }
    if frames.is_empty() {
        return Err("no usable frames in the observation".to_string());
    }

    // Coarser frames contribute to the finest frame's prior. Weighting by
    // distance from the decision frame keeps the closest higher frame dominant;
    // every probability remains descriptive, never a calibrated trade odds.
    let finest = frames.last().unwrap();
    let mut conditioned = finest.result.regime_probabilities.clone();
    for frame in frames[..frames.len() - 1].iter().rev() {
        for (regime, probability) in &frame.result.regime_probabilities {
            let entry = conditioned.entry(regime.clone()).or_insert(0.0);
            *entry = 0.7 * *entry + 0.3 * probability;
        }
    }
    let total: f64 = conditioned.values().sum();
    if total.is_finite() && total > 0.0 {
        for value in conditioned.values_mut() {
            *value /= total;
        }
    }

    Ok(HierarchicalMarkovReport {
        instrument: observation.instrument_id.clone(),
        frames,
        conditioned_bias: bias(&conditioned),
        conditioned_probabilities: conditioned,
        limitation:
            "deterministic coupling of independent Markov engines; not a fitted hierarchical HMM"
                .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::multiscale::TimeframeObservation;
    use analytics_core::types::Candle;

    const MIN: i64 = 60_000_000_000;

    fn observation(up: bool) -> MultiTimeframeObservation {
        let frames = [Timeframe::H1, Timeframe::M15, Timeframe::M1]
            .into_iter()
            .map(|timeframe| {
                let count = (2000 * MIN / timeframe.nanos()).clamp(30, 80) as usize;
                let candles: Vec<Candle> = (0..count)
                    .map(|i| {
                        let close = if up {
                            100.0 + i as f64 * 0.5
                        } else {
                            160.0 - i as f64 * 0.5
                        };
                        Candle {
                            symbol: "BTCUSDT".to_string(),
                            timeframe,
                            open_time: i as i64 * timeframe.nanos(),
                            open: close - if up { 0.4 } else { -0.4 },
                            high: close + 0.5,
                            low: close - 0.5,
                            close,
                            volume: 10.0,
                            buy_volume: if up { 7.0 } else { 3.0 },
                            sell_volume: if up { 3.0 } else { 7.0 },
                        }
                    })
                    .collect();
                TimeframeObservation::from_candles(
                    timeframe,
                    "BTCUSDT",
                    &candles,
                    2000 * MIN,
                    "test",
                    1,
                )
                .unwrap()
            })
            .collect();
        MultiTimeframeObservation::new("BTCUSDT", 2000 * MIN, frames).unwrap()
    }

    #[test]
    fn every_frame_produces_markov_evidence() {
        let report =
            analyze_hierarchy(&observation(true), &HierarchicalMarkovConfig::default()).unwrap();
        assert_eq!(report.frames.len(), 3);
        assert!(report
            .frames
            .iter()
            .all(|frame| frame.timeframe != Timeframe::W1));
        assert!(report
            .conditioned_probabilities
            .values()
            .all(|value| value.is_finite() && *value >= 0.0));
        assert!(report.limitation.contains("not a fitted hierarchical HMM"));
    }

    #[test]
    fn a_persistent_rise_produces_a_positive_bias() {
        let report =
            analyze_hierarchy(&observation(true), &HierarchicalMarkovConfig::default()).unwrap();
        assert!(
            report.conditioned_bias > 0.0,
            "{:?}",
            report.conditioned_probabilities
        );
    }

    #[test]
    fn insufficient_data_is_rejected() {
        let thin = MultiTimeframeObservation::new(
            "BTCUSDT",
            10 * MIN,
            vec![TimeframeObservation::from_candles(
                Timeframe::M1,
                "BTCUSDT",
                &observation(true).frames.last().unwrap().completed()[..2],
                10 * MIN,
                "test",
                1,
            )
            .unwrap()],
        )
        .unwrap();
        assert!(analyze_hierarchy(&thin, &HierarchicalMarkovConfig::default()).is_err());
    }
}
