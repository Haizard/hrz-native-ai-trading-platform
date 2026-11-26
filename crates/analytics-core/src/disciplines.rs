//! Deterministic discipline adapters for hierarchical analysis.
//!
//! Each function answers one question from the completed candles of a single
//! frame and returns the shared [`TimeframeAnalysis`] shape. Nothing here
//! trains a model, reads a clock, touches the network, or produces a
//! probability: the numbers are evidence strengths, which the result types
//! deliberately keep separate from calibrated distributions.
//!
//! The implementations reuse the crate's existing mathematics — market
//! structure, Bayesian drift, GARCH, the two-state HMM, Shannon entropy, CUSUM
//! and the z-score — rather than maintaining parallel versions.

use crate::hierarchy::{Direction, MarketStateKind, TimeframeAnalysis, VolatilityState};
use crate::indicators::quant::{cusum_stat, shannon_entropy, zscore};
use crate::indicators::quant2::{bayesian_drift, garch, hmm_regime};
use crate::market_structure::{detect_market_structure, Trend};
use crate::multiscale::TimeframeObservation;

/// Minimum completed candles most adapters need before concluding anything.
pub const MIN_BARS: usize = 30;

fn closes(frame: &TimeframeObservation, minimum: usize) -> Option<Vec<f64>> {
    let candles = frame.completed();
    if candles.len() < minimum {
        return None;
    }
    Some(candles.iter().map(|c| c.close).collect())
}

fn last_finite(values: &[Option<f64>]) -> Option<f64> {
    values
        .iter()
        .rev()
        .find_map(|value| value.filter(|v| v.is_finite()))
}

fn bounded(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Structural analysis: trend, swing and break evidence from price structure.
pub fn analyze_structure(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let candles = frame.completed();
    if candles.len() < MIN_BARS {
        return None;
    }
    let structure = detect_market_structure(candles, Default::default());
    let (direction, state) = match structure.trend {
        Trend::Bullish => (Direction::Bullish, MarketStateKind::Bullish),
        Trend::Bearish => (Direction::Bearish, MarketStateKind::Bearish),
        Trend::Ranging => (Direction::Neutral, MarketStateKind::Range),
    };
    let events = structure.breaks.len() + structure.points.len();
    let strength = bounded(events as f64 / 20.0);
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            strength,
            frame.data_quality.score(),
        )
        .ok()?
        .with_state(state)
        .with_observation(format!(
            "{} swings, {} breaks, trend {:?}",
            structure.points.len(),
            structure.breaks.len(),
            structure.trend
        )),
    )
}

/// Hidden-state analysis: the two-state Gaussian HMM's volatile-regime
/// posterior, plus a Bayesian drift for direction.
///
/// The HMM in this crate models *volatility* states, so it sets
/// [`VolatilityState`] rather than claiming a directional hidden state.
pub fn analyze_hidden_state(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS + 2)?;
    let posterior = last_finite(&hmm_regime(&closes, MIN_BARS))?;
    let (means, _) = bayesian_drift(&closes, 0.02);
    let drift = last_finite(&means)?;
    let direction = if drift > 0.0 {
        Direction::Bullish
    } else if drift < 0.0 {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    let volatility = if posterior > 0.6 {
        VolatilityState::Expanding
    } else if posterior < 0.4 {
        VolatilityState::Contracting
    } else {
        VolatilityState::Stable
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            bounded((posterior - 0.5).abs() * 2.0),
            frame.data_quality.score(),
        )
        .ok()?
        .with_volatility(volatility)
        .with_observation(format!(
            "volatile-state posterior {posterior:.3}, Bayesian drift {drift:.6}"
        )),
    )
}

/// Bayesian drift analysis: conjugate-update posterior mean and uncertainty.
pub fn analyze_bayesian(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS)?;
    let (means, deviations) = bayesian_drift(&closes, 0.02);
    let mean = last_finite(&means)?;
    let sd = last_finite(&deviations)?;
    let signal = if sd > 0.0 { mean.abs() / sd } else { 0.0 };
    let direction = if signal < 0.5 || sd <= 0.0 {
        Direction::Neutral
    } else if mean > 0.0 {
        Direction::Bullish
    } else {
        Direction::Bearish
    };
    let state = match direction {
        Direction::Bullish => MarketStateKind::Bullish,
        Direction::Bearish => MarketStateKind::Bearish,
        Direction::Neutral => MarketStateKind::Unknown,
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            bounded(signal / 3.0),
            frame.data_quality.score(),
        )
        .ok()?
        .with_state(state)
        .with_observation(format!("posterior drift {mean:.6} with sd {sd:.6}")),
    )
}

/// Volatility analysis: GARCH(1,1) conditional sigma at the newest bar versus
/// its value in the middle of the window.
pub fn analyze_volatility(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS + 10)?;
    let series = garch(&closes, MIN_BARS, 0.08, 0.88);
    let current = last_finite(&series)?;
    let midpoint =
        series[MIN_BARS + (series.len() - MIN_BARS) / 2].filter(|v| v.is_finite() && *v > 0.0)?;
    let ratio = current / midpoint;
    let volatility = if ratio > 1.1 {
        VolatilityState::Expanding
    } else if ratio < 0.9 {
        VolatilityState::Contracting
    } else {
        VolatilityState::Stable
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            Direction::Neutral,
            bounded((ratio - 1.0).abs()),
            frame.data_quality.score(),
        )
        .ok()?
        .with_volatility(volatility)
        .with_observation(format!("GARCH sigma ratio {ratio:.3} against mid-window")),
    )
}

/// Information-theory analysis: Shannon entropy of the return distribution.
///
/// This is deliberately single-frame entropy. Whether a higher frame carries
/// incremental information about a lower frame is a cross-frame research
/// question; the hierarchical framework reports descriptive relationships,
/// not information transfer measured out of sample.
pub fn analyze_information(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS)?;
    let entropy = last_finite(&shannon_entropy(&closes, MIN_BARS, 6))?;
    let positives = closes
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] > w[0])
        .count();
    let negatives = closes
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] < w[0])
        .count();
    let total = positives + negatives;
    if total == 0 {
        return None;
    }
    let skew = (positives as f64 - negatives as f64) / total as f64;
    let direction = if skew > 0.1 {
        Direction::Bullish
    } else if skew < -0.1 {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            bounded(skew.abs() * (1.0 - bounded(entropy))),
            frame.data_quality.score(),
        )
        .ok()?
        .with_observation(format!(
            "return entropy {entropy:.3}; {positives}/{total} up bars (single frame only)"
        )),
    )
}

/// Change-point analysis: the signed normalized CUSUM excursion.
pub fn analyze_change_point(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS)?;
    // The continuous statistic, not the thresholded signal: an excursion below
    // the classic gate is still evidence, just not a confirmed change-point.
    let excursion = last_finite(&cusum_stat(&closes, MIN_BARS))?;
    let direction = if excursion > 0.0 {
        Direction::Bullish
    } else if excursion < 0.0 {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    let state = if excursion.abs() >= 4.0 {
        MarketStateKind::Transition
    } else {
        MarketStateKind::Unknown
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            bounded(excursion.abs() / 8.0),
            frame.data_quality.score(),
        )
        .ok()?
        .with_state(state)
        .with_observation(format!("CUSUM excursion {excursion:.3}")),
    )
}

/// Statistical z-score analysis: the newest close's distance from its mean.
pub fn analyze_statistical(frame: &TimeframeObservation) -> Option<TimeframeAnalysis> {
    let closes = closes(frame, MIN_BARS)?;
    let score = last_finite(&zscore(&closes, MIN_BARS))?;
    let direction = if score > 0.5 {
        Direction::Bullish
    } else if score < -0.5 {
        Direction::Bearish
    } else {
        Direction::Neutral
    };
    Some(
        TimeframeAnalysis::new(
            frame.timeframe,
            direction,
            bounded(score.abs() / 3.0),
            frame.data_quality.score(),
        )
        .ok()?
        .with_observation(format!("close z-score {score:.3} over {MIN_BARS} bars")),
    )
}

/// All deterministic adapters, in the order they are presented to callers.
#[must_use]
pub fn all_disciplines() -> &'static [(
    &'static str,
    fn(&TimeframeObservation) -> Option<TimeframeAnalysis>,
)] {
    &[
        ("structure", analyze_structure),
        ("hidden_state", analyze_hidden_state),
        ("bayesian", analyze_bayesian),
        ("volatility", analyze_volatility),
        ("information", analyze_information),
        ("change_point", analyze_change_point),
        ("statistical", analyze_statistical),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, Timeframe};

    const MIN: i64 = 60_000_000_000;

    fn frame_with(closes: &[f64]) -> TimeframeObservation {
        let candles: Vec<Candle> = closes
            .iter()
            .enumerate()
            .map(|(i, close)| Candle {
                symbol: "BTCUSDT".to_string(),
                timeframe: Timeframe::M1,
                open_time: i as i64 * MIN,
                open: *close,
                high: close + 0.5,
                low: close - 0.5,
                close: *close,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            })
            .collect();
        TimeframeObservation::from_candles(
            Timeframe::M1,
            "BTCUSDT",
            &candles,
            closes.len() as i64 * MIN,
            "test",
            1,
        )
        .unwrap()
    }

    fn rising() -> Vec<f64> {
        // A deterministic wobble keeps return variance non-zero: a perfectly
        // linear rise has constant returns, so no change-point is detectable.
        (0..80)
            .map(|i| 100.0 + i as f64 * 0.35 + (i % 5) as f64 * 0.05)
            .collect()
    }

    fn alternating() -> Vec<f64> {
        (0..80)
            .map(|i| 100.0 + if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect()
    }

    #[test]
    fn every_adapter_is_bounded_and_finite_on_realistic_data() {
        let frame = frame_with(&rising());
        for (name, adapter) in all_disciplines() {
            let result = adapter(&frame).unwrap_or_else(|| panic!("{name} returned no analysis"));
            assert!(
                result.evidence_strength >= 0.0 && result.evidence_strength <= 1.0,
                "{name}"
            );
            assert!(
                result.data_quality >= 0.0 && result.data_quality <= 1.0,
                "{name}"
            );
            assert!(!result.structural_observations.is_empty(), "{name}");
        }
    }

    #[test]
    fn insufficient_data_is_not_analysed() {
        let frame = frame_with(&vec![100.0; 5]);
        assert!(all_disciplines()
            .iter()
            .all(|(_, adapter)| adapter(&frame).is_none()));
    }

    #[test]
    fn forming_candles_are_invisible_to_adapters() {
        let candles = rising();
        let frame = frame_with(&candles);
        assert_eq!(frame.completed().len(), candles.len());
        assert!(frame.forming_candle.is_none());

        // A frame whose only bar is forming must yield no analysis.
        let forming = TimeframeObservation::from_candles(
            Timeframe::M1,
            "BTCUSDT",
            &frame.completed()[..1],
            MIN / 2,
            "test",
            1,
        )
        .unwrap();
        assert!(forming.completed().is_empty());
        assert!(all_disciplines()
            .iter()
            .all(|(_, adapter)| adapter(&forming).is_none()));
    }

    #[test]
    fn bayesian_drift_follows_a_persistent_rise() {
        let analysis = analyze_bayesian(&frame_with(&rising())).unwrap();
        assert_eq!(analysis.direction, Direction::Bullish);
        assert_eq!(analysis.estimated_state, Some(MarketStateKind::Bullish));
    }

    #[test]
    fn structural_analysis_reports_its_inputs() {
        let analysis = analyze_structure(&frame_with(&alternating())).unwrap();
        assert!(!analysis.structural_observations.is_empty());
        assert!(analysis.structural_observations[0].contains("swings"));
    }

    #[test]
    fn volatility_adapter_never_claims_a_direction() {
        let analysis = analyze_volatility(&frame_with(&rising())).unwrap();
        assert_eq!(analysis.direction, Direction::Neutral);
        assert!(analysis.volatility_state.is_some());
    }

    #[test]
    fn adapters_never_set_a_probability() {
        // Discipline adapters return only evidence strength; the separate
        // hierarchy result type owns probability distributions.
        let analysis = analyze_change_point(&frame_with(&rising())).unwrap();
        assert!(analysis.evidence_strength.is_finite());
        assert!(std::mem::size_of_val(&analysis) > 0);
    }
}
