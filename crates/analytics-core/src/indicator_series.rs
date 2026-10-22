//! Indicator series for sub-panes: the per-bar values a chart pane draws,
//! computed here (the chart engine reimplements nothing) and carried to the
//! shell as data. The field names mirror the engine's `SubPaneSpec::Series`
//! exactly, so the shell forwards the spec untouched -- a new indicator
//! adopts the pane by adding one function here and one registry flag.

use crate::indicators::rsi::rsi;
use crate::rsi_divergence::{rsi_divergences, rsi_series, RsiDivergenceConfig};
use crate::types::Candle;
use serde::Serialize;

/// The pane's y-scale: fixed bounds (RSI's 0..100) or fit to the data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum SeriesScale {
    /// Fit the pane to the data's own range.
    Auto,
    /// A fixed range; out-of-range values clip to the edge.
    Fixed {
        /// The low end.
        min: f64,
        /// The high end.
        max: f64,
    },
}

/// One value line: a name, a colour, one `Option` per candle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesLine {
    /// The line's name.
    pub name: String,
    /// 0xRRGGBB.
    pub color: u32,
    /// One value per candle index; `None` is a gap.
    pub values: Vec<Option<f64>>,
}

/// A segment mark between two bar/value points (divergences).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesSegment {
    /// The first point's candle index.
    pub from: usize,
    /// The first point's value.
    pub from_value: f64,
    /// The second point's candle index.
    pub to: usize,
    /// The second point's value.
    pub to_value: f64,
    /// `bullish` or `bearish`.
    pub kind: String,
}

/// A point marker (a signal crossing).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesMarker {
    /// The candle index.
    pub at: usize,
    /// The value.
    pub value: f64,
    /// `bullish` or `bearish`.
    pub kind: String,
    /// A short label beside the point.
    pub label: String,
}

/// The full pane spec for one concept: the shell fetches this and forwards
/// it into the scene request verbatim.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndicatorSeries {
    /// The pane's label.
    pub label: String,
    /// The y-scale.
    pub scale: SeriesScale,
    /// Reference levels.
    pub levels: Vec<f64>,
    /// The value lines.
    pub lines: Vec<SeriesLine>,
    /// Segment marks.
    pub segments: Vec<SeriesSegment>,
    /// Point markers.
    pub markers: Vec<SeriesMarker>,
}

const RSI_BLUE: u32 = 0x60a5fa;
const PRED_VIOLET: u32 = 0xa78bfa;

/// The pane spec for a concept, or `None` when the concept has no pane.
pub fn indicator_series(concept: &str, candles: &[Candle]) -> Option<IndicatorSeries> {
    match concept {
        "indicators.rsi" => rsi_pane(candles, 14, false),
        "indicators.rsi_divergence" => rsi_pane(candles, 14, true),
        "indicators.rsi_pca" => rsi_pca_pane(candles),
        _ => None,
    }
}

/// Wilder's RSI with the 30/70 bands; with `divergences`, every confirmed
/// RSI/price divergence in view rides as a segment and a marker.
fn rsi_pane(candles: &[Candle], period: usize, divergences: bool) -> Option<IndicatorSeries> {
    if candles.len() <= period {
        return None;
    }
    let values = rsi_series(candles, period);
    let mut segments = Vec::new();
    let mut markers = Vec::new();
    if divergences {
        let config = RsiDivergenceConfig {
            period,
            lookback: candles.len(),
            ..RsiDivergenceConfig::default()
        };
        for div in rsi_divergences(candles, &config) {
            let kind = match div.direction {
                crate::rsi_divergence::RsiDivergenceKind::Bullish => "bullish",
                crate::rsi_divergence::RsiDivergenceKind::Bearish => "bearish",
            };
            segments.push(SeriesSegment {
                from: div.prior.index,
                from_value: div.prior.rsi,
                to: div.rsi_index,
                to_value: div.rsi,
                kind: kind.to_string(),
            });
            markers.push(SeriesMarker {
                at: div.rsi_index,
                value: div.rsi,
                kind: kind.to_string(),
                label: kind.to_string(),
            });
        }
    }
    Some(IndicatorSeries {
        label: if divergences {
            format!("RSI Divergence ({period})")
        } else {
            format!("RSI ({period})")
        },
        scale: SeriesScale::Fixed { min: 0.0, max: 100.0 },
        levels: vec![70.0, 30.0],
        lines: vec![SeriesLine {
            name: "RSI".to_string(),
            color: RSI_BLUE,
            values,
        }],
        segments,
        markers,
    })
}

/// The RSI-PCA model's per-bar prediction, with its signal crossings marked
/// and the zero line for reference. Auto-scaled: the prediction is a return
/// estimate, not a bounded oscillator.
fn rsi_pca_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let predictions = crate::rsi_pca::prediction_series(candles)?;
    let mut markers = Vec::new();
    for e in crate::rsi_pca::detect_rsi_pca(candles, "pane") {
        // Find this event's candle index by time.
        if let Some(at) = candles.iter().position(|c| c.open_time == e.bar_time) {
            if let (Some(kind), Some(value)) = (e.side, e.magnitude) {
                markers.push(SeriesMarker {
                    at,
                    value,
                    kind: match kind {
                        crate::types::Side::Buy => "bullish".to_string(),
                        crate::types::Side::Sell => "bearish".to_string(),
                    },
                    label: "signal".to_string(),
                });
            }
        }
    }
    Some(IndicatorSeries {
        label: format!("RSI-PCA ({}-bar)", crate::rsi_pca::LOOKAHEAD),
        scale: SeriesScale::Auto,
        levels: vec![0.0],
        lines: vec![SeriesLine {
            name: "prediction".to_string(),
            color: PRED_VIOLET,
            values: predictions,
        }],
        segments: Vec::new(),
        markers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, close: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: close,
            high: close * 1.001,
            low: close * 0.999,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn moving_series(n: usize) -> Vec<Candle> {
        let mut price = 100.0;
        (0..n)
            .map(|i| {
                price *= 1.0 + (i as f64 / 11.0).sin() * 0.006;
                k(i as i64 * 3_600_000_000_000, price)
            })
            .collect()
    }

    #[test]
    fn the_plain_rsi_pane_is_the_line_and_the_bands() {
        let spec = indicator_series("indicators.rsi", &moving_series(120)).expect("spec");
        assert_eq!(spec.scale, SeriesScale::Fixed { min: 0.0, max: 100.0 });
        assert_eq!(spec.levels, vec![70.0, 30.0]);
        assert_eq!(spec.lines.len(), 1);
        // The line warms up for `period` bars and never lies outside 0..100.
        assert!(spec.lines[0].values[0].is_none());
        for v in spec.lines[0].values.iter().flatten() {
            assert!((0.0..=100.0).contains(v), "RSI bounds: {v}");
        }
        assert!(spec.segments.is_empty(), "the plain RSI draws no divergences");
    }

    #[test]
    fn the_divergence_pane_carries_its_segments() {
        // A two-swing shape: price makes a higher high while RSI makes a
        // lower high -- the bearish divergence the detector confirms.
        let mut candles = moving_series(60);
        let mut price = 110.0;
        // Swing 1 up to a price/RSI high.
        for i in 0..15 {
            price *= 1.008;
            candles.push(k((60 + i) as i64 * 3_600_000_000_000, price));
        }
        for i in 0..10 {
            price *= 0.997;
            candles.push(k((75 + i) as i64 * 3_600_000_000_000, price));
        }
        // Swing 2: price higher, momentum shallower (smaller steps over more bars).
        for i in 0..25 {
            price *= 1.0022;
            candles.push(k((85 + i) as i64 * 3_600_000_000_000, price));
        }
        let spec = indicator_series("indicators.rsi_divergence", &candles).expect("spec");
        assert!(spec.lines.len() == 1, "the RSI line is the base");
        // Segments, when the detector confirms, are pairs of RSI extremes.
        for seg in &spec.segments {
            assert!(seg.from < seg.to, "a segment runs forward in time");
            assert!((0.0..=100.0).contains(&seg.from_value));
            assert!(seg.kind == "bullish" || seg.kind == "bearish");
        }
    }

    #[test]
    fn the_rsi_pca_pane_is_the_prediction_line() {
        let mut price = 100.0;
        let candles: Vec<Candle> = (0..500)
            .map(|i| {
                price *= 1.0 + (i as f64 / 25.0).sin() * 0.004 + ((i * 7919) % 13) as f64 * 0.0001 - 0.0006;
                k(i as i64 * 3_600_000_000_000, price)
            })
            .collect();
        let spec = indicator_series("indicators.rsi_pca", &candles).expect("spec");
        assert_eq!(spec.scale, SeriesScale::Auto, "a return estimate is not bounded");
        assert_eq!(spec.levels, vec![0.0], "the zero line anchors the prediction");
        // Predictions exist only after the walk-forward warm-up.
        let first = spec.lines[0].values.iter().position(|v| v.is_some()).expect("some predictions");
        assert!(first >= 248, "no predictions before the model can honestly fit: {first}");
        // Every marker sits on a real bar with a real prediction.
        for m in &spec.markers {
            assert!(m.at < candles.len());
            assert!(spec.lines[0].values[m.at].is_some(), "the marker rides its bar's prediction");
        }
    }

    #[test]
    fn unknown_concepts_have_no_pane() {
        assert!(indicator_series("smc.bos", &moving_series(60)).is_none());
        assert!(indicator_series("indicators.macd", &moving_series(60)).is_none());
    }
}
