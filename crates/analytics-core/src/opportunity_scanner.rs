//! Multi-stage opportunity scanning and trade-setup generation.
//!
//! The scanner is a funnel, not a trading authority: expensive analysis is
//! reserved for instruments that pass cheap checks first, and a setup is a
//! *proposal* for downstream validation and risk authorization. A ranking
//! score here is never a probability of success.

use serde::{Deserialize, Serialize};

use crate::disciplines::analyze_hierarchy;
use crate::hierarchy::{Direction, OpportunityCandidate};
use crate::indicators::atr::atr;
use crate::multiscale::MultiTimeframeObservation;
use crate::types::Timeframe;

/// Stages of the scan, in increasing cost order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreeningLevel {
    /// Cheap data-quality screen.
    DataQuality,
    /// Fast deterministic signal on the frame ladder.
    FastSignal,
    /// Full hierarchical analysis and cross-scale evidence.
    DeepAnalysis,
    /// Independent confirmation from agreeing higher frames.
    Confirmation,
    /// Chart-ready setup proposal.
    SetupReady,
}

/// Data-driven instrument configuration. Nothing here hardcodes a timeframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentProfile {
    /// Symbol this profile describes.
    pub symbol: String,
    /// Timeframes the instrument actually supports, coarsest first.
    pub timeframes: Vec<Timeframe>,
    /// Minimum acceptable data-quality score.
    pub min_data_quality: f64,
    /// Minimum evidence strength for a candidate.
    pub min_evidence_strength: f64,
}

impl InstrumentProfile {
    /// Create a profile with conservative defaults.
    #[must_use]
    pub fn new(symbol: impl Into<String>, timeframes: Vec<Timeframe>) -> Self {
        Self {
            symbol: symbol.into(),
            timeframes,
            min_data_quality: 0.7,
            min_evidence_strength: 0.5,
        }
    }
}

/// Outcome of scanning one instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanVerdict {
    /// Instrument scanned.
    pub symbol: String,
    /// Deepest stage reached.
    pub level_reached: ScreeningLevel,
    /// What the scanner recommends to the next layer.
    pub recommendation: ScanRecommendation,
    /// Ranking score in `[0, 1]`. Not a probability.
    pub ranking_score: f64,
    /// The candidate that passed, if any.
    pub candidate: Option<OpportunityCandidate>,
    /// Chart-ready proposal, when the setup stage was reached.
    pub setup: Option<SetupProposal>,
    /// Reasons recorded at each stage.
    pub reasons: Vec<String>,
}

/// Scanner recommendation, always subject to strategy rules and risk gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanRecommendation {
    /// Data or evidence was not sufficient.
    Skip,
    /// Keep watching, but do not propose a setup.
    Watch,
    /// A setup proposal is available for validation.
    Candidate,
}

/// A proposed trade setup, with no probability attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupProposal {
    /// Symbol.
    pub symbol: String,
    /// Timeframe where the opportunity was discovered.
    pub timeframe: Timeframe,
    /// Proposed direction.
    pub direction: Direction,
    /// Lower edge of the entry zone.
    pub entry_low: f64,
    /// Upper edge of the entry zone.
    pub entry_high: f64,
    /// Invalidation price.
    pub stop: f64,
    /// Reward targets at one and two times the initial risk.
    pub targets: Vec<f64>,
    /// Human-readable invalidation rule.
    pub invalidation: String,
    /// Evidence strings from the analysis.
    pub evidence: Vec<String>,
    /// Ranking score in `[0, 1]`. Not a probability.
    pub quality: f64,
}

fn build_setup(
    profile: &InstrumentProfile,
    observation: &MultiTimeframeObservation,
    candidate: &OpportunityCandidate,
    quality: f64,
) -> Option<SetupProposal> {
    let frame = observation.frame(candidate.timeframe)?;
    let candles = frame.completed();
    let last = candles.last()?;
    let period = 14.min(candles.len());
    let atr = atr(candles, period)
        .into_iter()
        .rev()
        .find_map(|v| v.filter(|v| v.is_finite() && *v > 0.0))?;
    let pad = (atr * 0.25).max(0.0);
    let (entry_low, entry_high, stop, targets, invalidation) = match candidate.direction {
        Direction::Bullish => {
            let entry_low = last.close - pad;
            let entry_high = last.close + pad;
            let risk = (last.low - pad - entry_low).abs().max(atr * 0.5);
            let stop = entry_low - risk;
            (
                entry_low,
                entry_high,
                stop,
                vec![entry_low + 2.0 * risk, entry_low + 3.0 * risk],
                "close below the proposed stop invalidates the setup".to_string(),
            )
        }
        Direction::Bearish => {
            let entry_low = last.close - pad;
            let entry_high = last.close + pad;
            let risk = (entry_high - (last.high + pad)).abs().max(atr * 0.5);
            let stop = entry_high + risk;
            (
                entry_low,
                entry_high,
                stop,
                vec![entry_high - 2.0 * risk, entry_high - 3.0 * risk],
                "close above the proposed stop invalidates the setup".to_string(),
            )
        }
        Direction::Neutral => return None,
    };

    let mut evidence = candidate.supporting_evidence.clone();
    evidence.extend(
        candidate
            .conflicting_evidence
            .iter()
            .map(|line| format!("conflict: {line}")),
    );
    evidence.push(format!(
        "ATR({period}) = {atr:.4} on {}",
        candidate.timeframe
    ));

    Some(SetupProposal {
        symbol: profile.symbol.clone(),
        timeframe: candidate.timeframe,
        direction: candidate.direction,
        entry_low,
        entry_high,
        stop,
        targets,
        invalidation,
        evidence,
        quality,
    })
}

/// Scan one instrument through the five-stage funnel.
///
/// The discipline is resolved by name from `analytics_core::disciplines`. A
/// failing stage returns the deepest level actually reached with the reason,
/// never a fabricated candidate.
#[must_use]
pub fn scan_instrument(
    profile: &InstrumentProfile,
    observation: &MultiTimeframeObservation,
    discipline: &str,
) -> ScanVerdict {
    let mut reasons = Vec::new();
    let finish = |level, recommendation, ranking_score, candidate, setup, reasons| ScanVerdict {
        symbol: profile.symbol.clone(),
        level_reached: level,
        recommendation,
        ranking_score,
        candidate,
        setup,
        reasons,
    };

    if observation.instrument_id != profile.symbol {
        reasons.push("observation belongs to a different instrument".to_string());
        return finish(
            ScreeningLevel::DataQuality,
            ScanRecommendation::Skip,
            0.0,
            None,
            None,
            reasons,
        );
    }
    let usable: Vec<_> = observation
        .frames
        .iter()
        .filter(|frame| frame.availability.is_usable())
        .collect();
    let quality = if usable.is_empty() {
        0.0
    } else {
        usable
            .iter()
            .map(|frame| frame.data_quality.score())
            .fold(1.0_f64, f64::min)
    };
    if usable.is_empty() || quality < profile.min_data_quality {
        reasons.push(format!(
            "data quality {quality:.2} is below {:.2}",
            profile.min_data_quality
        ));
        return finish(
            ScreeningLevel::DataQuality,
            ScanRecommendation::Skip,
            0.0,
            None,
            None,
            reasons,
        );
    }
    reasons.push(format!(
        "level 1 passed: {} usable frames, quality {quality:.2}",
        usable.len()
    ));

    let Some(result) = analyze_hierarchy(observation, discipline, "scanner") else {
        reasons.push(format!("unknown discipline `{discipline}`"));
        return finish(
            ScreeningLevel::FastSignal,
            ScanRecommendation::Skip,
            0.0,
            None,
            None,
            reasons,
        );
    };
    reasons.push(format!(
        "level 2 passed: {} frame conclusions",
        result.timeframe_results.len()
    ));

    let Some(candidate) = result.lower_timeframe_candidates.first() else {
        reasons.push("level 3: no lower-timeframe candidate from the hierarchy".to_string());
        return finish(
            ScreeningLevel::DeepAnalysis,
            ScanRecommendation::Watch,
            0.0,
            None,
            None,
            reasons,
        );
    };
    if candidate.evidence_strength < profile.min_evidence_strength {
        reasons.push(format!(
            "level 3: evidence {:.2} is below {:.2}",
            candidate.evidence_strength, profile.min_evidence_strength
        ));
        return finish(
            ScreeningLevel::DeepAnalysis,
            ScanRecommendation::Watch,
            candidate.evidence_strength,
            Some(candidate.clone()),
            None,
            reasons,
        );
    }
    reasons.push("level 3 passed: candidate produced with traceable evidence".to_string());

    if !candidate.conflicting_evidence.is_empty() {
        reasons
            .push("level 4: higher frames conflict; setup requires manual validation".to_string());
        return finish(
            ScreeningLevel::Confirmation,
            ScanRecommendation::Watch,
            candidate.evidence_strength,
            Some(candidate.clone()),
            None,
            reasons,
        );
    }
    reasons.push("level 4 passed: higher frames agree".to_string());

    let ranking = (candidate.evidence_strength * quality).clamp(0.0, 1.0);
    let Some(setup) = build_setup(profile, observation, candidate, ranking) else {
        reasons.push("level 5: no completed candle to anchor a setup".to_string());
        return finish(
            ScreeningLevel::SetupReady,
            ScanRecommendation::Watch,
            ranking,
            Some(candidate.clone()),
            None,
            reasons,
        );
    };
    reasons.push("level 5 passed: setup proposal ready for validation".to_string());
    finish(
        ScreeningLevel::SetupReady,
        ScanRecommendation::Candidate,
        ranking,
        Some(candidate.clone()),
        Some(setup),
        reasons,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiscale::TimeframeObservation as Frame;
    use crate::types::Candle;

    const MIN: i64 = 60_000_000_000;

    fn observation(symbol: &str, quality_ok: bool, bullish: bool) -> MultiTimeframeObservation {
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
                            symbol: symbol.to_string(),
                            timeframe,
                            open_time: i as i64 * timeframe.nanos(),
                            open,
                            high: open.max(close) + 0.2,
                            low: open.min(close) - 0.2,
                            close,
                            volume: if quality_ok { 10.0 } else { 0.0 },
                            buy_volume: if quality_ok { 6.0 } else { 0.0 },
                            sell_volume: if quality_ok { 4.0 } else { 0.0 },
                        }
                    })
                    .collect();
                Frame::from_candles(timeframe, symbol, &candles, decision, "test", 1).unwrap()
            })
            .collect();
        MultiTimeframeObservation::new(symbol, decision, frames).unwrap()
    }

    fn profile() -> InstrumentProfile {
        InstrumentProfile::new(
            "BTCUSDT",
            vec![Timeframe::M1, Timeframe::M15, Timeframe::H1],
        )
    }

    #[test]
    fn a_clean_aligned_observation_reaches_a_setup() {
        let verdict = scan_instrument(&profile(), &observation("BTCUSDT", true, true), "bayesian");
        assert_eq!(verdict.level_reached, ScreeningLevel::SetupReady);
        assert_eq!(verdict.recommendation, ScanRecommendation::Candidate);
        let setup = verdict.setup.unwrap();
        assert!(setup.entry_low <= setup.entry_high);
        assert_eq!(setup.targets.len(), 2);
        assert!(!setup.evidence.is_empty());
        assert!(verdict.ranking_score >= 0.0 && verdict.ranking_score <= 1.0);
    }

    #[test]
    fn a_wrong_instrument_is_skipped_at_the_first_stage() {
        let verdict = scan_instrument(&profile(), &observation("ETHUSDT", true, true), "bayesian");
        assert_eq!(verdict.level_reached, ScreeningLevel::DataQuality);
        assert_eq!(verdict.recommendation, ScanRecommendation::Skip);
    }

    #[test]
    fn an_unknown_discipline_stops_after_the_data_screen() {
        let verdict = scan_instrument(&profile(), &observation("BTCUSDT", true, true), "unknown");
        assert_eq!(verdict.level_reached, ScreeningLevel::FastSignal);
        assert_eq!(verdict.recommendation, ScanRecommendation::Skip);
    }

    #[test]
    fn conflicting_frames_never_produce_a_setup() {
        // One higher frame leaning the other way is a real conflict.
        let mut observation = observation("BTCUSDT", true, true);
        if let Some(frame) = observation
            .frames
            .iter_mut()
            .find(|frame| frame.timeframe == Timeframe::H1)
        {
            for (index, candle) in frame.candles.iter_mut().enumerate() {
                let open = 200.0 - index as f64 * 0.5;
                candle.open = open;
                candle.close = open - 0.5;
                candle.high = open + 0.2;
                candle.low = open - 0.7;
            }
        }
        let verdict = scan_instrument(&profile(), &observation, "statistical");
        assert!(verdict.setup.is_none());
        assert!(matches!(
            verdict.recommendation,
            ScanRecommendation::Watch | ScanRecommendation::Skip
        ));
    }

    #[test]
    fn the_scanner_never_emits_a_probability() {
        let verdict = scan_instrument(&profile(), &observation("BTCUSDT", true, true), "bayesian");
        assert!(verdict
            .candidate
            .as_ref()
            .unwrap()
            .probability_distribution
            .is_none());
        let serialized = serde_json::to_string(&verdict).unwrap();
        assert!(serialized.contains("\"probability_distribution\":null"));
    }
}
