//! Deterministic OHLC representation for the NEURO-MARKET research direction.
//!
//! This is deliberately **not** a neural network and has no learned weights. It
//! provides the self-supervised-style primitives the roadmap asks for — masked
//! candle reconstruction, a compact latent signature and similarity — using
//! fixed, inspectable arithmetic. A researcher can use it to test whether a
//! representation carries information before committing to a trained model.

use serde::{Deserialize, Serialize};

use crate::multiscale::TimeframeObservation;
use crate::types::{Candle, Timeframe};

/// Fixed configuration for the encoder. No parameter is fitted to data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RepresentationConfig {
    /// Number of trailing completed candles in one representation window.
    pub window: usize,
    /// Minimum absolute net return treated as a directional move.
    pub direction_epsilon: f64,
}

impl Default for RepresentationConfig {
    fn default() -> Self {
        Self {
            window: 30,
            direction_epsilon: 0.002,
        }
    }
}

/// A compact, inspectable market representation at one timeframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketRepresentation {
    /// Frame this representation describes.
    pub timeframe: Timeframe,
    /// 0 = down, 1 = flat, 2 = up.
    pub direction_bucket: u8,
    /// Realized-volatility bucket, higher means noisier.
    pub volatility_bucket: u8,
    /// Relative-volume bucket: 0 = light, 1 = normal, 2 = heavy.
    pub volume_bucket: u8,
    /// Aggressor-delta bucket: 0 = selling, 1 = balanced, 2 = buying.
    pub delta_bucket: u8,
    /// The four buckets packed into one byte for cheap comparison.
    pub signature: u8,
    /// Confidence in `[0, 1]` from the masked-candle reconstruction.
    /// Not a probability of any market outcome.
    pub reconstruction_confidence: f64,
    /// Normalized reconstruction error on the most recent held-out bar.
    pub reconstruction_error: f64,
}

fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt()
}

/// Predict a held-out close by linear extrapolation from its three predecessors.
///
/// This is the masked-candle reconstruction task, solved without learned
/// weights: the model is a fixed local linear rule, so the error measures how
/// predictable this particular window is rather than how good a trained model
/// would be.
#[must_use]
pub fn masked_reconstruction_error(candles: &[Candle], index: usize) -> Option<f64> {
    if index < 3 || index >= candles.len() {
        return None;
    }
    let a = candles[index - 3].close;
    let b = candles[index - 2].close;
    let c = candles[index - 1].close;
    let predicted = c + (c - a) / 2.0;
    let base = [a, b, c]
        .into_iter()
        .fold(0.0_f64, |acc, value| acc + (value - c).abs())
        / 3.0;
    let scale = base.max(c.abs() * 1e-6).max(f64::EPSILON);
    let error = (candles[index].close - predicted).abs() / scale;
    error.is_finite().then_some(error)
}

/// Encode one completed-candle window into a compact signature.
pub fn encode(
    frame: &TimeframeObservation,
    config: &RepresentationConfig,
) -> Option<MarketRepresentation> {
    let candles = frame.completed();
    if candles.len() < config.window.max(8) {
        return None;
    }
    let window = &candles[candles.len() - config.window..];
    let mut returns = Vec::with_capacity(window.len().saturating_sub(1));
    for pair in window.windows(2) {
        if pair[0].close > 0.0 && pair[1].close.is_finite() {
            returns.push((pair[1].close / pair[0].close).ln());
        }
    }
    if returns.len() < 8 {
        return None;
    }
    let net: f64 = returns.iter().sum();
    let direction_bucket = if net < -config.direction_epsilon {
        0
    } else if net > config.direction_epsilon {
        2
    } else {
        1
    };
    let vol = stddev(&returns);
    let volatility_bucket = if vol < 0.001 {
        0
    } else if vol < 0.003 {
        1
    } else if vol < 0.01 {
        2
    } else {
        3
    };
    let mean_volume = window.iter().map(|c| c.volume.max(0.0)).sum::<f64>() / window.len() as f64;
    let last_volume = window.last()?.volume.max(0.0);
    let volume_ratio = if mean_volume > 0.0 {
        last_volume / mean_volume
    } else {
        1.0
    };
    let volume_bucket = if volume_ratio < 0.8 {
        0
    } else if volume_ratio > 1.2 {
        2
    } else {
        1
    };
    let volume_sum: f64 = window.iter().map(|c| c.volume.max(0.0)).sum();
    let delta_sum: f64 = window.iter().map(Candle::delta).sum();
    let delta_ratio = if volume_sum > 0.0 {
        delta_sum / volume_sum
    } else {
        0.0
    };
    let delta_bucket = if delta_ratio < -0.1 {
        0
    } else if delta_ratio > 0.1 {
        2
    } else {
        1
    };
    let reconstruction_error = masked_reconstruction_error(window, window.len() - 1)?;
    let reconstruction_confidence = (1.0 / (1.0 + reconstruction_error)).clamp(0.0, 1.0);
    let signature =
        direction_bucket | (volatility_bucket << 2) | (volume_bucket << 5) | (delta_bucket << 7);

    Some(MarketRepresentation {
        timeframe: frame.timeframe,
        direction_bucket,
        volatility_bucket,
        volume_bucket,
        delta_bucket,
        signature,
        reconstruction_confidence,
        reconstruction_error,
    })
}

/// Similarity of two packed representations, in `[0, 1]`.
///
/// One is exact identity. This is a discrete diagnostic, not a claim that the
/// two windows share an outcome distribution.
#[must_use]
pub const fn similarity(a: &MarketRepresentation, b: &MarketRepresentation) -> f64 {
    let differing = ((a.signature ^ b.signature).count_ones()) as f64;
    1.0 - differing / 8.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Candle;

    const MIN: i64 = 60_000_000_000;

    fn frame(kind: &str) -> TimeframeObservation {
        let candles: Vec<Candle> = (0..60)
            .map(|i| {
                let close = match kind {
                    "up" => 100.0 + i as f64 * 0.5,
                    "down" => 160.0 - i as f64 * 0.5,
                    "volatile" => 100.0 + (i % 4) as f64 * if i % 8 < 4 { 8.0 } else { -8.0 },
                    _ => 100.0,
                };
                let open = if kind == "up" {
                    close - 0.4
                } else if kind == "down" {
                    close + 0.4
                } else {
                    close
                };
                Candle {
                    symbol: "BTCUSDT".to_string(),
                    timeframe: Timeframe::M1,
                    open_time: i as i64 * MIN,
                    open,
                    high: open.max(close) + 0.5,
                    low: open.min(close) - 0.5,
                    close,
                    volume: 10.0,
                    buy_volume: if kind == "up" {
                        7.0
                    } else if kind == "down" {
                        3.0
                    } else {
                        5.0
                    },
                    sell_volume: if kind == "up" {
                        3.0
                    } else if kind == "down" {
                        7.0
                    } else {
                        5.0
                    },
                }
            })
            .collect();
        TimeframeObservation::from_candles(Timeframe::M1, "BTCUSDT", &candles, 60 * MIN, "test", 1)
            .unwrap()
    }

    #[test]
    fn direction_and_delta_buckets_follow_the_data() {
        let up = encode(&frame("up"), &RepresentationConfig::default()).unwrap();
        let down = encode(&frame("down"), &RepresentationConfig::default()).unwrap();
        let flat = encode(&frame("flat"), &RepresentationConfig::default()).unwrap();
        assert_eq!(up.direction_bucket, 2);
        assert_eq!(up.delta_bucket, 2);
        assert_eq!(down.direction_bucket, 0);
        assert_eq!(down.delta_bucket, 0);
        assert_eq!(flat.direction_bucket, 1);
    }

    #[test]
    fn volatile_windows_get_a_higher_volatility_bucket() {
        let config = RepresentationConfig::default();
        let smooth = encode(&frame("up"), &config).unwrap();
        let noisy = encode(&frame("volatile"), &config).unwrap();
        assert!(noisy.volatility_bucket > smooth.volatility_bucket);
    }

    #[test]
    fn masked_reconstruction_is_lower_for_a_smooth_series() {
        let smooth = frame("up");
        let mut noisy = frame("volatile");
        let last_index = noisy.completed().len() - 1;
        let last = &mut noisy.candles[last_index];
        last.close = last.open * 1.3;
        last.high = last.close;
        let smooth_error = masked_reconstruction_error(smooth.completed(), 59).unwrap();
        let noisy_error = masked_reconstruction_error(noisy.completed(), 59).unwrap();
        assert!(smooth_error < noisy_error);
    }

    #[test]
    fn signatures_compare_equal_only_for_the_same_shape() {
        let config = RepresentationConfig::default();
        let a = encode(&frame("up"), &config).unwrap();
        let b = encode(&frame("up"), &config).unwrap();
        let c = encode(&frame("down"), &config).unwrap();
        assert_eq!(similarity(&a, &b), 1.0);
        assert!(similarity(&a, &c) < 1.0);
        assert!(a.reconstruction_confidence >= 0.0 && a.reconstruction_confidence <= 1.0);
    }

    #[test]
    fn short_windows_are_refused() {
        let mut short = frame("up");
        short.candles.truncate(4);
        assert!(encode(&short, &RepresentationConfig::default()).is_none());
    }
}
