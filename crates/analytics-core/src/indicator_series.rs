//! Indicator series for sub-panes: the per-bar values a chart pane draws,
//! computed here (the chart engine reimplements nothing) and carried to the
//! shell as data. The field names mirror the engine's `SubPaneSpec::Series`
//! exactly, so the shell forwards the spec untouched -- a new indicator
//! adopts the pane by adding one function here and one registry flag.

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

/// A histogram for a pane (MACD and friends).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesBars {
    /// The histogram's name.
    pub name: String,
    /// 0xRRGGBB for non-negative bars.
    pub color_up: u32,
    /// 0xRRGGBB for negative bars.
    pub color_down: u32,
    /// One value per candle index; `None` is a gap.
    pub values: Vec<Option<f64>>,
}

/// Where a series draws: its own sub-pane, or overlaid on the price plot
/// (EMAs, Bollinger bands -- measurements in the instrument's own price).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesTarget {
    /// A sub-pane below the chart.
    Pane,
    /// Lines on the price plot itself.
    Overlay,
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
    /// An optional histogram.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bars: Option<SeriesBars>,
    /// Where this series draws.
    pub target: SeriesTarget,
}

const RSI_BLUE: u32 = 0x60a5fa;
const PRED_VIOLET: u32 = 0xa78bfa;

/// The pane spec for a concept, or `None` when the concept has no pane.
pub fn indicator_series(concept: &str, candles: &[Candle]) -> Option<IndicatorSeries> {
    match concept {
        "indicators.rsi" => rsi_pane(candles, 14, false),
        "indicators.rsi_divergence" => rsi_pane(candles, 14, true),
        "indicators.rsi_pca" => rsi_pca_pane(candles),
        "indicators.macd" => macd_pane(candles),
        "indicators.atr" => atr_pane(candles),
        "indicators.ema" => ema_overlay(candles),
        "indicators.sma" => sma_overlay(candles),
        "indicators.bollinger" => bollinger_overlay(candles),
        _ => None,
    }
}

/// MACD (12, 26, 9): the line and its signal as two lines, the histogram as
/// zero-anchored bars, the zero line as the only reference. Auto-scaled --
/// a MACD's range is the instrument's own volatility.
fn macd_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let points = crate::indicators::macd::macd(&closes, 12, 26, 9);
    if points.iter().all(|p| p.is_none()) {
        return None;
    }
    let line: Vec<Option<f64>> = points.iter().map(|p| p.map(|m| m.line)).collect();
    let signal: Vec<Option<f64>> = points.iter().map(|p| p.map(|m| m.signal)).collect();
    let histogram: Vec<Option<f64>> = points.iter().map(|p| p.map(|m| m.histogram)).collect();
    Some(IndicatorSeries {
        label: "MACD (12, 26, 9)".to_string(),
        scale: SeriesScale::Auto,
        levels: vec![0.0],
        lines: vec![
            SeriesLine { name: "macd".to_string(), color: RSI_BLUE, values: line },
            SeriesLine { name: "signal".to_string(), color: 0xf59e0b, values: signal },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: Some(SeriesBars {
            name: "histogram".to_string(),
            color_up: 0x34d399,
            color_down: 0xfb7185,
            values: histogram,
        }),
        target: SeriesTarget::Pane,
    })
}

/// Wilder's ATR (14): one line, auto-scaled -- a volatility measurement in
/// the instrument's own units.
fn atr_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let values = crate::indicators::atr::atr(candles, 14);
    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "ATR (14)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![SeriesLine { name: "atr".to_string(), color: 0xf59e0b, values }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// EMA 20/50/200 on the price plot: the trend stack traders actually watch.
fn ema_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let lines = [(20, 0x22d3ee), (50, 0xf59e0b), (200, 0xf472b6)]
        .iter()
        .map(|(period, color)| SeriesLine {
            name: format!("ema{period}"),
            color: *color,
            values: crate::indicators::ema::ema(&closes, *period),
        })
        .collect();
    Some(IndicatorSeries {
        label: "EMA (20, 50, 200)".to_string(),
        scale: SeriesScale::Auto, // unused on the overlay: the price frame maps it
        levels: Vec::new(),
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// SMA 20/50/200 on the price plot.
fn sma_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let lines = [(20, 0x22d3ee), (50, 0xf59e0b), (200, 0xf472b6)]
        .iter()
        .map(|(period, color)| SeriesLine {
            name: format!("sma{period}"),
            color: *color,
            values: crate::indicators::sma::sma(&closes, *period),
        })
        .collect();
    Some(IndicatorSeries {
        label: "SMA (20, 50, 200)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// Bollinger bands (20, 2): the basis and both bands on the price plot.
fn bollinger_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let bands = crate::indicators::bollinger::bollinger(&closes, 20, 2.0);
    if bands.iter().all(|b| b.is_none()) {
        return None;
    }
    let pick = |f: fn(&crate::indicators::bollinger::BollingerPoint) -> f64| -> Vec<Option<f64>> {
        bands.iter().map(|b| b.as_ref().map(|p| f(p))).collect()
    };
    Some(IndicatorSeries {
        label: "Bollinger (20, 2)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![
            SeriesLine { name: "upper".to_string(), color: 0x94a3b8, values: pick(|p| p.upper) },
            SeriesLine { name: "middle".to_string(), color: 0xf59e0b, values: pick(|p| p.middle) },
            SeriesLine { name: "lower".to_string(), color: 0x94a3b8, values: pick(|p| p.lower) },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
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
        bars: None,
        target: SeriesTarget::Pane,
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
        bars: None,
        target: SeriesTarget::Pane,
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
        assert!(indicator_series("smc.bos", &moving_series(120)).is_none());
        assert!(indicator_series("indicators.vwap", &moving_series(120)).is_none());
    }

    #[test]
    fn the_macd_pane_is_two_lines_and_a_histogram() {
        let spec = indicator_series("indicators.macd", &moving_series(200)).expect("spec");
        assert_eq!(spec.lines.len(), 2, "macd + signal");
        assert_eq!(spec.lines[0].name, "macd");
        assert_eq!(spec.lines[1].name, "signal");
        let bars = spec.bars.expect("the histogram");
        assert_eq!(spec.levels, vec![0.0], "the zero line anchors it");
        // The histogram IS line - signal, bar for bar.
        for (i, h) in bars.values.iter().enumerate() {
            if let (Some(h), Some(l), Some(sig)) = (h, spec.lines[0].values[i], spec.lines[1].values[i]) {
                assert!((h - (l - sig)).abs() < 1e-9, "bar {i}: histogram is the difference");
            }
        }
        // Warm-up: no values before slow + signal - 2 bars.
        assert!(spec.lines[0].values[..33].iter().all(|v| v.is_none()), "no early MACD");
        assert!(spec.lines[0].values[40].is_some(), "values after warm-up");
    }

    #[test]
    fn the_atr_pane_is_one_positive_line() {
        let spec = indicator_series("indicators.atr", &moving_series(120)).expect("spec");
        assert_eq!(spec.lines.len(), 1);
        for v in spec.lines[0].values.iter().flatten() {
            assert!(*v > 0.0, "a range average is positive: {v}");
        }
    }

    #[test]
    fn overlays_carry_prices_and_the_overlay_target() {
        let candles = moving_series(250);
        for id in ["indicators.ema", "indicators.sma", "indicators.bollinger"] {
            let spec = indicator_series(id, &candles).expect(id);
            assert_eq!(spec.target, SeriesTarget::Overlay, "{id} rides the price frame");
            assert!(spec.segments.is_empty() && spec.markers.is_empty() && spec.bars.is_none());
            // Every value is a PRICE: inside the series' own traded range.
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for c in &candles {
                lo = lo.min(c.low);
                hi = hi.max(c.high);
            }
            for line in &spec.lines {
                for v in line.values.iter().flatten() {
                    assert!(*v >= lo * 0.9 && *v <= hi * 1.1, "{id} {} value {v} is a price", line.name);
                }
            }
        }
        // The bollinger upper band never sits below the lower band.
        let spec = indicator_series("indicators.bollinger", &candles).expect("bb");
        for i in 0..candles.len() {
            if let (Some(u), Some(l)) = (spec.lines[0].values[i], spec.lines[2].values[i]) {
                assert!(u >= l, "bands ordered at {i}");
            }
        }
        // The ema stack names its periods.
        let spec = indicator_series("indicators.ema", &candles).expect("ema");
        let names: Vec<&str> = spec.lines.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["ema20", "ema50", "ema200"]);
    }
}
