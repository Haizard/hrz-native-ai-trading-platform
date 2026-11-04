//! Multi-timeframe indicator-family readings for the chart's heatmap.
//!
//! This is a confluence summary, not a trading system. Directional families
//! contribute bounded scores; volatility describes risk regime and is never
//! counted as bullish or bearish by itself. Volume can be backed by executed
//! candle volume or explicitly supplied quote-activity counts.

use crate::indicators::{atr, bollinger, ema, macd, quant, quant2, rsi, trend};
use crate::regime::structure_lean;
use crate::types::{Candle, Side, Timeframe};
use serde::Serialize;

/// One explainable metric shown under a family reading.
#[derive(Debug, Clone, Serialize)]
pub struct HeatmapMetric {
    /// Human-readable metric name.
    pub name: String,
    /// Formatted reading.
    pub value: String,
}

/// One indicator family on one timeframe.
#[derive(Debug, Clone, Serialize)]
pub struct FamilyReading {
    /// Stable family identifier.
    pub id: String,
    /// Display title.
    pub name: String,
    /// Directional score in -100..=100. Volatility is intentionally `None`.
    pub score: Option<f64>,
    /// Compact state such as `bullish`, `range`, `squeeze`, or `unavailable`.
    pub state: String,
    /// Evidence quality, reduced for short history or proxy inputs.
    pub confidence: f64,
    /// Diagnostic readings that explain the score.
    pub metrics: Vec<HeatmapMetric>,
    /// Where the family inputs came from.
    pub basis: String,
    /// Why the reading is limited or unavailable.
    pub note: Option<String>,
}

/// All five families for one timeframe.
#[derive(Debug, Clone, Serialize)]
pub struct FrameReading {
    /// Timeframe label.
    pub timeframe: String,
    /// Latest candle timestamp (nanoseconds).
    pub observed_at_ns: Option<i64>,
    /// Number of input bars.
    pub bars: usize,
    /// Family readings in the chart's stable display order.
    pub families: Vec<FamilyReading>,
}

/// Aggregate directional confluence across frames and families.
#[derive(Debug, Clone, Serialize)]
pub struct HeatmapConsensus {
    /// Weighted directional reading in -100..=100.
    pub score: f64,
    /// Human-readable state.
    pub signal: String,
    /// 0..=100 agreement/confidence estimate, not probability of profit.
    pub confidence: f64,
    /// Share of directional readings aligned with the consensus.
    pub alignment_pct: f64,
    /// Number of directional readings included.
    pub readings: usize,
    /// Short explanation of the score's interpretation.
    pub note: String,
}

/// Full multi-timeframe heatmap response.
#[derive(Debug, Clone, Serialize)]
pub struct IndicatorHeatmap {
    /// Instrument.
    pub symbol: String,
    /// The chart timeframe used as the first rung.
    pub base_timeframe: String,
    /// Candle readings, one per requested timeframe.
    pub frames: Vec<FrameReading>,
    /// Directional confluence, excluding the non-directional volatility family.
    pub consensus: HeatmapConsensus,
}

/// Compute five indicator families over the same closed-candle data supplied by
/// the API. `quote_activity` is present only for feeds whose venue exposes
/// quote observations without traded size; it is never called traded volume.
#[must_use]
pub fn calculate(
    symbol: &str,
    base_timeframe: Timeframe,
    inputs: &[(Timeframe, Vec<Candle>, Option<Vec<f64>>)],
) -> IndicatorHeatmap {
    let frames: Vec<FrameReading> = inputs
        .iter()
        .map(|(timeframe, candles, quote_activity)| FrameReading {
            timeframe: timeframe.to_string(),
            observed_at_ns: candles.last().map(|c| c.open_time),
            bars: candles.len(),
            families: calculate_frame(candles, quote_activity.as_deref()),
        })
        .collect();
    let directional: Vec<(f64, f64)> = frames
        .iter()
        .flat_map(|frame| frame.families.iter())
        .filter(|family| family.id != "volatility")
        .filter_map(|family| family.score.map(|score| (score, family.confidence)))
        .collect();
    let score = weighted_mean(&directional);
    let alignment = if directional.is_empty() || score.abs() < 12.0 {
        0.0
    } else {
        let sign = score.signum();
        100.0
            * directional
                .iter()
                .filter(|(value, _)| value.signum() == sign)
                .count() as f64
            / directional.len() as f64
    };
    let evidence_quality = weighted_mean(
        &directional
            .iter()
            .map(|(_, confidence)| (*confidence, 1.0))
            .collect::<Vec<_>>(),
    );
    let confidence = (alignment * 0.65 + evidence_quality * 0.35).clamp(0.0, 100.0);
    let signal = if directional.is_empty() {
        "warming up"
    } else if score >= 60.0 {
        "strong bullish confluence"
    } else if score >= 25.0 {
        "bullish confluence"
    } else if score <= -60.0 {
        "strong bearish confluence"
    } else if score <= -25.0 {
        "bearish confluence"
    } else {
        "mixed / neutral"
    };
    IndicatorHeatmap {
        symbol: symbol.into(),
        base_timeframe: base_timeframe.to_string(),
        frames,
        consensus: HeatmapConsensus {
            score,
            signal: signal.into(),
            confidence,
            alignment_pct: alignment,
            readings: directional.len(),
            note: "Confluence is a comparison of indicator readings, not a probability of profit or an execution instruction. Check price location, invalidation, and the data basis before acting.".into(),
        },
    }
}

fn weighted_mean(values: &[(f64, f64)]) -> f64 {
    let (sum, weight) = values
        .iter()
        .fold((0.0, 0.0), |(sum, weight), (value, confidence)| {
            (sum + value * confidence, weight + confidence)
        });
    if weight > 0.0 { sum / weight } else { 0.0 }
}

fn reading(
    id: &str,
    name: &str,
    score: Option<f64>,
    state: &str,
    confidence: f64,
    basis: &str,
    metrics: Vec<(&str, String)>,
    note: Option<String>,
) -> FamilyReading {
    FamilyReading {
        id: id.into(),
        name: name.into(),
        score: score.map(|value| value.clamp(-100.0, 100.0)),
        state: state.into(),
        confidence: confidence.clamp(0.0, 100.0),
        metrics: metrics
            .into_iter()
            .map(|(name, value)| HeatmapMetric {
                name: name.into(),
                value,
            })
            .collect(),
        basis: basis.into(),
        note,
    }
}

fn latest<T: Copy>(values: &[Option<T>]) -> Option<T> {
    values.iter().rev().find_map(|value| *value)
}
fn pct(value: f64) -> String {
    format!("{:.2}%", value * 100.0)
}
fn signed(value: f64) -> String {
    format!("{value:+.2}")
}

fn calculate_frame(candles: &[Candle], quote_activity: Option<&[f64]>) -> Vec<FamilyReading> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let enough = candles.len() >= 60;
    let evidence_confidence = (52.0 + candles.len().min(300) as f64 / 300.0 * 48.0).min(100.0);
    let (fast, medium, slow) = (
        latest(&ema(&closes, 20)),
        latest(&ema(&closes, 50)),
        latest(&ema(&closes, 200)),
    );

    // Trend combines three-scale EMA location, Wilder ADX/+DI/-DI and confirmed
    // swing structure, avoiding a direction call from one moving average alone.
    let (plus_di, minus_di, adx_line) = trend::adx(candles, 14);
    let adx_value = latest(&adx_line);
    let di_score = match (latest(&plus_di), latest(&minus_di)) {
        (Some(plus), Some(minus)) if plus + minus > 0.0 => 100.0 * (plus - minus) / (plus + minus),
        _ => 0.0,
    };
    let ema_score = match (fast, medium, slow, candles.last()) {
        (Some(fast), Some(medium), Some(slow), Some(last)) => {
            let ordered = if fast > medium && medium > slow {
                1.0
            } else if fast < medium && medium < slow {
                -1.0
            } else {
                0.0
            };
            let location = (last.close / medium - 1.0).clamp(-0.02, 0.02) / 0.02;
            55.0 * ordered + 45.0 * location
        }
        _ => 0.0,
    };
    let structure_score = match structure_lean(candles) {
        Some(Side::Buy) => 100.0,
        Some(Side::Sell) => -100.0,
        None => 0.0,
    };
    let trend_strength = (adx_value.unwrap_or(0.0) / 40.0).clamp(0.0, 1.0);
    let trend_ready = fast.is_some() && medium.is_some() && slow.is_some() && adx_value.is_some();
    let trend_score = 0.45 * ema_score + 0.35 * di_score + 0.20 * structure_score * trend_strength;
    let trend_state = state(trend_score, 22.0);
    let trend_metrics = vec![
        (
            "EMA 20 / 50 / 200",
            match (fast, medium, slow) {
                (Some(a), Some(b), Some(c)) => format!(
                    "{} / {} / {}",
                    relation(a, b),
                    relation(b, c),
                    relation(candles.last().map_or(c, |x| x.close), c)
                ),
                _ => "warming up".into(),
            },
        ),
        (
            "ADX / DI pressure",
            format!(
                "{} · {}",
                adx_value.map_or_else(|| "—".into(), |v| format!("{v:.1}")),
                signed(di_score)
            ),
        ),
        (
            "Swing structure",
            match structure_score {
                v if v > 0.0 => "higher highs/lows".into(),
                v if v < 0.0 => "lower highs/lows".into(),
                _ => "mixed / forming".into(),
            },
        ),
    ];

    // Momentum follows RSI-family consensus (2..24), MACD histogram and a
    // normalized ROC read. Fast RSI outliers are balanced by the family median.
    let rsi_values: Vec<f64> = crate::rsi_pca::RSI_PERIODS
        .iter()
        .filter_map(|period| latest(&rsi(&closes, *period)))
        .collect();
    let rsi_mid = median(&rsi_values);
    let rsi_breadth = if rsi_values.is_empty() {
        0.0
    } else {
        100.0 * rsi_values.iter().filter(|v| **v >= 50.0).count() as f64 / rsi_values.len() as f64
            - 50.0
    };
    let macd_hist = latest(&macd(&closes, 12, 26, 9)).map_or(0.0, |value| value.histogram);
    let macd_scale = candles.last().map_or(0.0, |last| last.close.abs() * 0.002);
    let macd_score = if macd_scale > 0.0 {
        (macd_hist / macd_scale * 50.0).clamp(-100.0, 100.0)
    } else {
        0.0
    };
    let roc = closes
        .last()
        .zip(closes.get(closes.len().saturating_sub(13)))
        .and_then(|(last, then)| (*then != 0.0).then_some(last / then - 1.0))
        .unwrap_or(0.0);
    let momentum_score = 0.45 * (rsi_mid.unwrap_or(50.0) - 50.0) * 2.0
        + 0.35 * macd_score
        + 0.20 * (roc / 0.02 * 100.0).clamp(-100.0, 100.0);
    let momentum_state = state(momentum_score, 20.0);
    let momentum_metrics = vec![
        (
            "RSI family median (2..24)",
            rsi_mid.map_or_else(
                || "warming up".into(),
                |v| format!("{v:.1} · {}% above 50", rsi_breadth + 50.0),
            ),
        ),
        ("MACD histogram", signed(macd_hist)),
        ("12-bar return", pct(roc)),
    ];

    // Volatility is a risk state: ATR% is compared with its own rolling history,
    // Bollinger width detects compression/expansion, and GARCH/HMM corroborate
    // whether elevated movement is clustering rather than treating it as bias.
    let atr_pct = latest(&atr::atr_percent(candles, 14));
    let atr_history: Vec<f64> = atr::atr_percent(candles, 14)
        .into_iter()
        .flatten()
        .collect();
    let atr_median = median(&atr_history);
    let bands = bollinger(&closes, 20, 2.0);
    let bandwidth = latest(&bands).map(|band| band.bandwidth);
    let bandwidth_prev = bands
        .iter()
        .rev()
        .nth(5)
        .and_then(|v| *v)
        .map(|band| band.bandwidth);
    let garch = latest(&quant2::garch(&closes, 30, 0.10, 0.85));
    let hmm_line = quant2::hmm_regime(&closes, 60);
    let hmm = latest(&hmm_line);
    let volatility_ratio = atr_pct
        .zip(atr_median)
        .map(|(now, median)| if median > 0.0 { now / median } else { 1.0 });
    let volatility_state = match (volatility_ratio, bandwidth.zip(bandwidth_prev)) {
        (Some(ratio), Some((now, before))) if ratio < 0.75 && now < before => "compression",
        (Some(ratio), Some((now, before))) if ratio > 1.35 && now > before => "expanding",
        (Some(ratio), _) if ratio > 1.35 => "high range",
        (Some(_), _) => "normal",
        _ => "warming up",
    };
    let volatility_note = if volatility_state == "compression" {
        Some(
            "Squeeze conditions can precede a range expansion; direction still needs confirmation."
                .into(),
        )
    } else if volatility_state == "expanding" {
        Some(
            "Range is expanding. Consider wider noise bands and reduce size to keep risk stable."
                .into(),
        )
    } else {
        None
    };
    let volatility_metrics = vec![
        (
            "ATR 14 / price",
            atr_pct.map_or_else(|| "warming up".into(), pct),
        ),
        (
            "ATR vs own median",
            volatility_ratio.map_or_else(|| "—".into(), |v| format!("{v:.2}×")),
        ),
        ("Bollinger width", bandwidth.map_or_else(|| "—".into(), pct)),
        (
            "GARCH σ / HMM volatile",
            format!(
                "{} / {}",
                garch.map_or_else(|| "—".into(), pct),
                hmm.map_or_else(|| "—".into(), |v| format!("{:.0}%", v * 100.0))
            ),
        ),
    ];

    // Volume combines normalized Chaikin-style signed activity, OBV slope and
    // money-flow index. A quote count is deliberately tagged as an activity
    // proxy; it cannot establish buyer/seller aggressor volume.
    let quote_basis = quote_activity.is_some();
    let activity: Vec<f64> = quote_activity
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| candles.iter().map(|c| c.volume).collect());
    let total_activity: f64 = activity.iter().rev().take(30).sum();
    let usable = activity.iter().rev().take(30).filter(|v| **v > 0.0).count();
    let flow = candles
        .iter()
        .zip(&activity)
        .rev()
        .take(20)
        .filter_map(|(c, volume)| {
            let range = c.high - c.low;
            (range > 0.0 && volume.is_finite())
                .then_some(((2.0 * c.close - c.high - c.low) / range) * volume)
        })
        .sum::<f64>();
    let cmf = if total_activity > 0.0 {
        flow / total_activity
    } else {
        0.0
    };
    let obv_slope = candles
        .iter()
        .zip(&activity)
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| {
            let (newer, newer_volume) = pair[0];
            let (older, _older_volume) = pair[1];
            if newer.close > older.close {
                *newer_volume
            } else if newer.close < older.close {
                -*newer_volume
            } else {
                0.0
            }
        })
        .sum::<f64>();
    let obv_score = if total_activity > 0.0 {
        (obv_slope / total_activity * 500.0).clamp(-100.0, 100.0)
    } else {
        0.0
    };
    let mfi = money_flow_index(candles, &activity, 14).unwrap_or(50.0);
    let volume_score =
        0.45 * (cmf * 100.0).clamp(-100.0, 100.0) + 0.30 * obv_score + 0.25 * (mfi - 50.0) * 2.0;
    let has_volume = total_activity > 0.0 && usable >= 8;
    let volume_state = if !has_volume {
        "unavailable"
    } else {
        state(volume_score, 20.0)
    };
    let volume_basis = if quote_basis {
        "Deriv quote-tick activity proxy · not traded volume"
    } else if has_volume {
        "reported candle volume"
    } else {
        "no usable volume data"
    };
    let volume_note = if quote_basis {
        Some("Chaikin/OBV-style inputs use quote counts and close location as a participation proxy. They are not signed trades or true money flow.".into())
    } else if !has_volume {
        Some("This instrument/timeframe has no usable volume observations; no directional volume score is included.".into())
    } else {
        None
    };
    let volume_metrics = vec![
        (
            if quote_basis {
                "Quote activity"
            } else {
                "Volume"
            },
            format!("{} obs", total_activity.round()),
        ),
        ("Flow pressure", signed(cmf * 100.0)),
        ("OBV slope", signed(obv_score)),
        ("Money Flow Index", format!("{mfi:.1}")),
    ];

    // Statistical family blends posterior drift significance, a normalized
    // AR(2) forecast and distance-from-mean z-score. The HMM volatile posterior
    // is exposed as a regime quality diagnostic, not a directional vote.
    let (drift, uncertainty) = quant2::bayesian_drift(&closes, 0.001);
    let drift_z = latest(&drift)
        .zip(latest(&uncertainty))
        .map_or(0.0, |(m, s)| if s > 0.0 { m / s } else { 0.0 })
        .clamp(-3.0, 3.0);
    let forecast = latest(&quant2::ar2_forecast(&closes, 30)).unwrap_or(0.0);
    let rets = log_returns(&closes);
    let ret_sd = stddev(&rets);
    let forecast_z = if ret_sd > 0.0 { forecast / ret_sd } else { 0.0 };
    let price_z = latest(&quant::zscore(&closes, 50))
        .unwrap_or(0.0)
        .clamp(-3.0, 3.0);
    let statistical_score = 55.0 * (drift_z / 3.0).clamp(-1.0, 1.0) * 100.0 / 100.0
        + 30.0 * (forecast_z / 2.0).clamp(-1.0, 1.0) * 100.0 / 100.0
        + 15.0 * (price_z / 3.0).clamp(-1.0, 1.0) * 100.0 / 100.0;
    let entropy = quant::shannon_entropy(&closes, 30, 8);
    let entropy_value = latest(&entropy).unwrap_or(0.0);
    let hmm_probability = hmm.unwrap_or(0.0);
    let stat_metrics = vec![
        ("Bayesian drift z", signed(drift_z)),
        ("AR(2) forecast z", signed(forecast_z)),
        ("Price z-score (50)", signed(price_z)),
        (
            "Return entropy / HMM volatile",
            format!("{entropy_value:.2} / {:.0}%", hmm_probability * 100.0),
        ),
    ];
    let note = if !enough {
        Some("Long-lookback readings are warming up; confidence is reduced until enough candles arrive.".into())
    } else {
        None
    };
    vec![
        reading(
            "volume",
            "Volume",
            has_volume.then_some(volume_score),
            volume_state,
            if quote_basis {
                evidence_confidence * 0.65
            } else {
                evidence_confidence
            },
            volume_basis,
            volume_metrics,
            volume_note,
        ),
        reading(
            "trend",
            "Trend",
            trend_ready.then_some(trend_score),
            if trend_ready {
                trend_state
            } else {
                "warming up"
            },
            evidence_confidence,
            "OHLC candles · EMA / ADX-DI / swing structure",
            trend_metrics,
            note.clone(),
        ),
        reading(
            "momentum",
            "Momentum",
            (candles.len() >= 30).then_some(momentum_score),
            if candles.len() >= 30 {
                momentum_state
            } else {
                "warming up"
            },
            if candles.len() >= 30 {
                evidence_confidence
            } else {
                50.0
            },
            "OHLC candles · RSI family / MACD / ROC",
            momentum_metrics,
            note.clone(),
        ),
        reading(
            "volatility",
            "Volatility",
            None,
            volatility_state,
            evidence_confidence,
            "OHLC candles · ATR / Bollinger / GARCH / HMM",
            volatility_metrics,
            volatility_note,
        ),
        reading(
            "statistical",
            "Statistical",
            (candles.len() >= 50).then_some(statistical_score),
            if statistical_score >= 20.0 {
                "positive drift"
            } else if statistical_score <= -20.0 {
                "negative drift"
            } else {
                "mixed / mean reverting"
            },
            if candles.len() >= 50 {
                evidence_confidence
            } else {
                50.0
            },
            "OHLC candles · Bayesian drift / AR(2) / z-score / entropy",
            stat_metrics,
            note,
        ),
    ]
}

fn state(score: f64, threshold: f64) -> &'static str {
    if score >= threshold {
        "bullish"
    } else if score <= -threshold {
        "bearish"
    } else {
        "mixed / range"
    }
}

fn relation(left: f64, right: f64) -> &'static str {
    if left > right {
        "↑"
    } else if left < right {
        "↓"
    } else {
        "="
    }
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
}

fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

fn log_returns(closes: &[f64]) -> Vec<f64> {
    closes
        .windows(2)
        .filter_map(|w| (w[0] > 0.0 && w[1] > 0.0).then_some((w[1] / w[0]).ln()))
        .collect()
}

fn money_flow_index(candles: &[Candle], activity: &[f64], period: usize) -> Option<f64> {
    if candles.len() <= period || candles.len() != activity.len() {
        return None;
    }
    let mut positive = 0.0;
    let mut negative = 0.0;
    let start = candles.len() - period;
    for index in start..candles.len() {
        let typical = (candles[index].high + candles[index].low + candles[index].close) / 3.0;
        let prior =
            (candles[index - 1].high + candles[index - 1].low + candles[index - 1].close) / 3.0;
        let flow = typical * activity[index].max(0.0);
        if typical > prior {
            positive += flow;
        } else if typical < prior {
            negative += flow;
        }
    }
    if positive == 0.0 && negative == 0.0 {
        Some(50.0)
    } else if negative == 0.0 {
        Some(100.0)
    } else {
        Some(100.0 - 100.0 / (1.0 + positive / negative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(drift: f64, volume: f64) -> Vec<Candle> {
        (0..260)
            .map(|i| {
                let close = 100.0 + i as f64 * drift + (i as f64 * 0.31).sin() * 0.45;
                Candle {
                    symbol: "TEST".into(),
                    timeframe: Timeframe::M5,
                    open_time: i * Timeframe::M5.nanos(),
                    open: close - drift,
                    high: close + 0.8,
                    low: close - 0.8,
                    close,
                    volume,
                    buy_volume: 0.0,
                    sell_volume: 0.0,
                }
            })
            .collect()
    }

    #[test]
    fn heatmap_reports_five_families_and_a_directional_consensus() {
        let heatmap = calculate(
            "TEST",
            Timeframe::M5,
            &[(Timeframe::M5, candles(0.2, 100.0), None)],
        );
        assert_eq!(heatmap.frames[0].families.len(), 5);
        assert_eq!(heatmap.frames[0].families[0].id, "volume");
        assert!(heatmap.consensus.readings > 0);
        assert!(
            heatmap.frames[0]
                .families
                .iter()
                .find(|f| f.id == "volatility")
                .unwrap()
                .score
                .is_none()
        );
    }

    #[test]
    fn quote_tick_activity_is_explicitly_a_proxy_and_gets_reduced_confidence() {
        let heatmap = calculate(
            "DERIV:V75",
            Timeframe::M1,
            &[(Timeframe::M1, candles(0.1, 0.0), Some(vec![25.0; 260]))],
        );
        let volume = heatmap.frames[0]
            .families
            .iter()
            .find(|f| f.id == "volume")
            .unwrap();
        assert!(volume.basis.contains("not traded volume"));
        assert!(volume.confidence < 84.0);
        assert!(volume.note.as_deref().unwrap().contains("quote counts"));
    }

    #[test]
    fn missing_volume_is_not_silently_interpreted_as_bearish() {
        let heatmap = calculate(
            "DERIV:V75",
            Timeframe::M1,
            &[(Timeframe::M1, candles(0.1, 0.0), None)],
        );
        let volume = heatmap.frames[0]
            .families
            .iter()
            .find(|f| f.id == "volume")
            .unwrap();
        assert_eq!(volume.state, "unavailable");
        assert!(volume.score.is_none());
    }
}
