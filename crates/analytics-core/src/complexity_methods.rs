//! Rough volatility, multifractal scaling, recurrence quantification and
//! cross-market network transmission.
//!
//! Every estimator is closed-form and deterministic. They measure structure in
//! supplied series; none is a fitted model or a trading signal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::research_methods::transfer_entropy;
use crate::types::Candle;

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let average = mean(values);
    (values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64)
        .sqrt()
}

fn log_returns(closes: &[f64]) -> Vec<f64> {
    closes
        .windows(2)
        .filter(|pair| pair[0] > 0.0 && pair[1].is_finite())
        .map(|pair| (pair[1] / pair[0]).ln())
        .collect()
}

/// Rough-volatility evidence from a deterministic fractional kernel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoughVolatilityReport {
    /// Estimated Hurst exponent of absolute returns.
    pub hurst: f64,
    /// Kernel-weighted log volatility at the newest bar.
    pub rough_sigma: f64,
    /// Short-window realized volatility.
    pub realized_sigma: f64,
    /// Ratio of rough to realized volatility.
    pub rough_ratio: f64,
}

/// Estimate rough volatility using a fixed power-law kernel on squared returns.
#[must_use]
pub fn rough_volatility(returns: &[f64], hurst: f64) -> Option<RoughVolatilityReport> {
    if returns.len() < 40 || !hurst.is_finite() || !(0.0..1.0).contains(&hurst) {
        return None;
    }
    let exponent = hurst - 0.5;
    let mut weighted = 0.0;
    let mut weights = 0.0;
    for (lag, value) in returns.iter().rev().enumerate() {
        let weight = (lag as f64 + 1.0).powf(exponent);
        weighted += weight * value * value;
        weights += weight;
    }
    if weights <= 0.0 {
        return None;
    }
    let rough_sigma = (weighted / weights).sqrt();
    let short = &returns[returns.len().saturating_sub(20)..];
    let realized_sigma = stddev(short);
    Some(RoughVolatilityReport {
        hurst,
        rough_sigma,
        realized_sigma,
        rough_ratio: rough_sigma / realized_sigma.max(1e-12),
    })
}

/// Multifractal evidence from generalized Hurst exponents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultifractalReport {
    /// Generalized Hurst exponents keyed by moment q.
    pub exponents: BTreeMap<String, f64>,
    /// Width of the singularity spectrum, `h(q_min) - h(q_max)`.
    pub spectrum_width: f64,
    /// Explicit limitation.
    pub limitation: String,
}

/// Multifractal DFA-style scaling over aggregation windows.
///
/// For each moment q this regresses log fluctuation against log window. The
/// spread across q is the multifractal signature; a constant h(q) means a
/// monofractal series.
#[must_use]
pub fn multifractal_exponents(
    values: &[f64],
    moments: &[f64],
    min_window: usize,
    max_window: usize,
) -> Option<MultifractalReport> {
    if values.len() < max_window.saturating_mul(2)
        || min_window < 2
        || max_window <= min_window
        || moments.is_empty()
    {
        return None;
    }
    let mut exponents = BTreeMap::new();
    for q in moments {
        let mut points = Vec::new();
        let mut window = min_window;
        while window <= max_window {
            let count = values.len() / window;
            let mut fluctuations = Vec::with_capacity(count);
            for chunk in values.chunks_exact(window) {
                let average = mean(chunk);
                let variance = chunk
                    .iter()
                    .map(|value| (value - average).powi(2))
                    .sum::<f64>()
                    / window as f64;
                fluctuations.push(variance.sqrt());
            }
            if fluctuations.len() >= 2 {
                let fluctuation = fluctuations.iter().map(|value| value.powf(*q)).sum::<f64>()
                    / fluctuations.len() as f64;
                if fluctuation > 0.0 && fluctuation.is_finite() {
                    points.push(((window as f64).ln(), fluctuation.ln() / *q));
                }
            }
            window *= 2;
        }
        if points.len() < 2 {
            return None;
        }
        let n = points.len() as f64;
        let sx: f64 = points.iter().map(|point| point.0).sum();
        let sy: f64 = points.iter().map(|point| point.1).sum();
        let sxx: f64 = points.iter().map(|point| point.0 * point.0).sum();
        let sxy: f64 = points.iter().map(|point| point.0 * point.1).sum();
        let denominator = n * sxx - sx * sx;
        if denominator.abs() < f64::EPSILON {
            return None;
        }
        exponents.insert(format!("q={q}"), (n * sxy - sx * sy) / denominator);
    }
    let low = moments.iter().copied().fold(f64::INFINITY, f64::min);
    let high = moments.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = (exponents[&format!("q={low}")] - exponents[&format!("q={high}")]).abs();
    Some(MultifractalReport {
        exponents,
        spectrum_width: width,
        limitation: "scaling estimate on a finite window; no significance test".to_string(),
    })
}

/// Recurrence quantification metrics of a delay embedding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecurrenceReport {
    /// Recurrence rate.
    pub recurrence_rate: f64,
    /// Determinism: share of recurrent pairs on diagonal lines.
    pub determinism: f64,
    /// Laminarity: share of recurrent pairs on vertical lines.
    pub laminarity: f64,
    /// Mean diagonal line length.
    pub mean_diagonal_length: f64,
}

/// Full recurrence quantification at one threshold.
#[must_use]
pub fn recurrence_quantification(
    values: &[f64],
    embedding: usize,
    delay: usize,
    threshold: f64,
) -> Option<RecurrenceReport> {
    if embedding == 0 || delay == 0 || threshold <= 0.0 {
        return None;
    }
    let count = values.len().saturating_sub((embedding - 1) * delay);
    if count < 20 {
        return None;
    }
    let states: Vec<Vec<f64>> = (0..count)
        .map(|start| {
            (0..embedding)
                .map(|lag| values[start + lag * delay])
                .collect()
        })
        .collect();
    let mut matrix = vec![vec![false; count]; count];
    let mut recurrent = 0usize;
    let mut pairs = 0usize;
    for i in 0..count {
        for j in i + 1..count {
            let distance = states[i]
                .iter()
                .zip(&states[j])
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            pairs += 1;
            if distance <= threshold {
                matrix[i][j] = true;
                matrix[j][i] = true;
                recurrent += 1;
            }
        }
    }
    if pairs == 0 {
        return None;
    }
    let mut diagonal_points = 0usize;
    let mut diagonal_lines = 0usize;
    let mut diagonal_lengths = Vec::new();
    for offset in 1..count {
        let mut length = 0usize;
        for i in 0..count - offset {
            if matrix[i][i + offset] {
                length += 1;
                diagonal_points += 1;
            } else if length > 0 {
                diagonal_lines += 1;
                diagonal_lengths.push(length);
                length = 0;
            }
        }
        if length > 0 {
            diagonal_lines += 1;
            diagonal_lengths.push(length);
        }
    }
    let mut vertical_points = 0usize;
    let mut vertical_lines = 0usize;
    for column in 0..count {
        let mut length = 0usize;
        for row in 0..count {
            if matrix[row][column] {
                length += 1;
                vertical_points += 1;
            } else if length > 0 {
                vertical_lines += 1;
                length = 0;
            }
        }
        if length > 0 {
            vertical_lines += 1;
        }
    }
    let mean_diagonal_length = if diagonal_lines == 0 {
        0.0
    } else {
        diagonal_lengths.iter().sum::<usize>() as f64 / diagonal_lines as f64
    };
    Some(RecurrenceReport {
        recurrence_rate: recurrent as f64 / pairs as f64,
        determinism: diagonal_points as f64 / (recurrent * 2) as f64,
        laminarity: vertical_points as f64 / (recurrent * 2) as f64,
        mean_diagonal_length,
    })
}

/// Directed information-transmission edge between two instruments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransmissionEdge {
    /// Source symbol.
    pub from: String,
    /// Target symbol.
    pub to: String,
    /// Transfer entropy in bits.
    pub transfer_entropy: f64,
    /// Sample count behind the estimate.
    pub samples: usize,
}

/// Build a directed transfer-entropy network from aligned instrument series.
#[must_use]
pub fn market_network(series: &[(&str, &[Candle])], bins: usize) -> Vec<TransmissionEdge> {
    let mut edges = Vec::new();
    for (from_index, (from_symbol, from_candles)) in series.iter().enumerate() {
        for (to_index, (to_symbol, to_candles)) in series.iter().enumerate() {
            if from_index == to_index {
                continue;
            }
            let count = from_candles.len().min(to_candles.len()).saturating_sub(1);
            if count < 30 {
                continue;
            }
            let from_returns = log_returns(
                &from_candles[..=count]
                    .iter()
                    .map(|c| c.close)
                    .collect::<Vec<_>>(),
            );
            let to_returns = log_returns(
                &to_candles[..=count]
                    .iter()
                    .map(|c| c.close)
                    .collect::<Vec<_>>(),
            );
            let n = from_returns.len().min(to_returns.len());
            if let Some(te) = transfer_entropy(&from_returns[..n], &to_returns[..n], bins) {
                edges.push(TransmissionEdge {
                    from: (*from_symbol).to_string(),
                    to: (*to_symbol).to_string(),
                    transfer_entropy: te,
                    samples: n,
                });
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn series(up: bool, count: usize) -> Vec<f64> {
        (0..count)
            .map(|i| {
                (if up {
                    100.0 + i as f64 * 0.5
                } else {
                    200.0 - i as f64 * 0.5
                }) + (i % 7) as f64 * 0.1
            })
            .collect()
    }

    fn candles(up: bool, count: usize) -> Vec<Candle> {
        series(up, count)
            .into_iter()
            .enumerate()
            .map(|(i, close)| Candle {
                symbol: "TEST".to_string(),
                timeframe: Timeframe::M1,
                open_time: i as i64 * 60_000_000_000,
                open: close - 0.1,
                high: close + 0.3,
                low: close - 0.3,
                close,
                volume: 10.0,
                buy_volume: if up { 7.0 } else { 3.0 },
                sell_volume: if up { 3.0 } else { 7.0 },
            })
            .collect()
    }

    #[test]
    fn rough_volatility_is_finite_and_bounded_ratio() {
        let returns = log_returns(&series(true, 100));
        let report = rough_volatility(&returns, 0.4).unwrap();
        assert!(report.rough_sigma >= 0.0 && report.rough_sigma.is_finite());
        assert!(report.rough_ratio >= 0.0 && report.rough_ratio.is_finite());
        assert!(rough_volatility(&returns, 1.2).is_none());
    }

    #[test]
    fn multifractal_exponents_are_finite() {
        let report = multifractal_exponents(&series(true, 256), &[1.0, 2.0, 3.0], 2, 16).unwrap();
        assert_eq!(report.exponents.len(), 3);
        assert!(report.exponents.values().all(|value| value.is_finite()));
        assert!(report.spectrum_width >= 0.0);
    }

    #[test]
    fn recurrence_metrics_are_bounded() {
        let report = recurrence_quantification(&series(true, 80), 2, 1, 1.0).unwrap();
        assert!((0.0..=1.0).contains(&report.recurrence_rate));
        assert!((0.0..=1.0).contains(&report.determinism));
        assert!((0.0..=1.0).contains(&report.laminarity));
        assert!(report.mean_diagonal_length >= 0.0);
    }

    #[test]
    fn market_network_has_one_edge_per_directed_pair() {
        let up = candles(true, 80);
        let down = candles(false, 80);
        let edges = market_network(&[("UP", &up), ("DOWN", &down)], 4);
        assert_eq!(edges.len(), 2);
        assert!(edges
            .iter()
            .all(|edge| edge.transfer_entropy >= 0.0 && edge.samples > 0));
    }
}
