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
        "indicators.rsi_family" => rsi_family_pane(candles),
        "indicators.macd_family" => macd_family_pane(candles),
        "indicators.atr_family" => atr_family_pane(candles),
        "indicators.ema_family" => ema_family_overlay(candles),
        "indicators.stochastic" => stochastic_pane(candles),
        "indicators.cci" => cci_pane(candles),
        "indicators.williams_r" => williams_pane(candles),
        "indicators.roc" => roc_pane(candles),
        "indicators.obv" => obv_pane(candles),
        "indicators.adx" => adx_pane(candles),
        "indicators.bollinger_percent" => percent_b_pane(candles),
        "indicators.keltner" => keltner_overlay(candles),
        "indicators.donchian" => donchian_overlay(candles),
        "indicators.supertrend" => supertrend_overlay(candles),
        "indicators.psar" => psar_overlay(candles),
        "indicators.macd" => macd_pane(candles),
        "indicators.atr" => atr_pane(candles),
        "indicators.ema" => ema_overlay(candles),
        "indicators.sma" => sma_overlay(candles),
        "indicators.bollinger" => bollinger_overlay(candles),
        _ => None,
    }
}

/// The RSI family ribbon (his `rsi_behavior.py` exhibit): every period
/// 2..=24 plotted together on the fixed 0..100 scale, coloured along a
/// fast-to-slow gradient so the eye reads the family's own structure --
/// clustered lines are a consensus trend, a fanning ribbon is a transition,
/// the fast edge parting from the slow edge is the divergence the PCA model
/// later quantifies. The middle lines go unnamed: twenty-three legend chips
/// would be wallpaper, and the two edge names carry the meaning.
fn rsi_family_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    if closes.len() < 30 {
        return None;
    }
    // The gradient: amber (fast, period 2) to blue (slow, period 24).
    let (fa, fb) = ((0xf5u32, 0x9eu32, 0x0bu32), (0x3bu32, 0x82u32, 0xf6u32));
    let n = crate::rsi_pca::RSI_PERIODS.len();
    let lines: Vec<SeriesLine> = crate::rsi_pca::RSI_PERIODS
        .iter()
        .enumerate()
        .map(|(i, period)| {
            let t = i as f64 / (n - 1) as f64;
            let mix = |a: u32, b: u32| (a as f64 + (b as f64 - a as f64) * t) as u32;
            let color = (mix(fa.0, fb.0) << 16) | (mix(fa.1, fb.1) << 8) | mix(fa.2, fb.2);
            SeriesLine {
                name: if i == 0 || i == n - 1 {
                    format!("rsi{period}")
                } else {
                    String::new()
                },
                color,
                values: crate::indicators::rsi::rsi(&closes, *period),
            }
        })
        .collect();
    Some(IndicatorSeries {
        label: "RSI Family (2..24)".to_string(),
        scale: SeriesScale::Fixed { min: 0.0, max: 100.0 },
        levels: vec![70.0, 30.0],
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// The MACD family ribbon: the MACD LINE of every fast period 2..=24 with
/// slow = 2x fast (the 12:26 ratio kept), on the zero line, auto-scaled,
/// fast-to-slow gradient. The ribbon agreeing on a side of zero is the
/// trend's own consensus; the fast edge crossing first is the early read.
fn macd_family_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    if closes.len() < 60 {
        return None;
    }
    let n = crate::rsi_pca::RSI_PERIODS.len();
    let (fa, fb) = ((0xf5u32, 0x9eu32, 0x0bu32), (0x3bu32, 0x82u32, 0xf6u32));
    let lines: Vec<SeriesLine> = crate::rsi_pca::RSI_PERIODS
        .iter()
        .enumerate()
        .map(|(i, fast)| {
            let t = i as f64 / (n - 1) as f64;
            let mix = |a: u32, b: u32| (a as f64 + (b as f64 - a as f64) * t) as u32;
            let color = (mix(fa.0, fb.0) << 16) | (mix(fa.1, fb.1) << 8) | mix(fa.2, fb.2);
            SeriesLine {
                name: if i == 0 || i == n - 1 {
                    format!("macd{fast}")
                } else {
                    String::new()
                },
                color,
                values: crate::indicators::macd::macd(&closes, *fast, fast * 2, 1)
                    .iter()
                    .map(|p| p.map(|m| m.line))
                    .collect(),
            }
        })
        .collect();
    Some(IndicatorSeries {
        label: "MACD Family (2..24, 2x)".to_string(),
        scale: SeriesScale::Auto,
        levels: vec![0.0],
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// The ATR family ribbon: every period 2..=24, fast-to-slow gradient,
/// auto-scaled. Agreement across periods is a real volatility regime; one
/// period spiking alone is noise.
fn atr_family_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    if candles.len() < 30 {
        return None;
    }
    let n = crate::rsi_pca::RSI_PERIODS.len();
    let (fa, fb) = ((0xf5u32, 0x9eu32, 0x0bu32), (0x3bu32, 0x82u32, 0xf6u32));
    let lines: Vec<SeriesLine> = crate::rsi_pca::RSI_PERIODS
        .iter()
        .enumerate()
        .map(|(i, period)| {
            let t = i as f64 / (n - 1) as f64;
            let mix = |a: u32, b: u32| (a as f64 + (b as f64 - a as f64) * t) as u32;
            let color = (mix(fa.0, fb.0) << 16) | (mix(fa.1, fb.1) << 8) | mix(fa.2, fb.2);
            SeriesLine {
                name: if i == 0 || i == n - 1 {
                    format!("atr{period}")
                } else {
                    String::new()
                },
                color,
                values: crate::indicators::atr::atr(candles, *period),
            }
        })
        .collect();
    Some(IndicatorSeries {
        label: "ATR Family (2..24)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// Stochastic (14, 3): %K and %D on the fixed 0..100 scale, 80/20 bands.
fn stochastic_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let (k_line, d_line) = crate::indicators::oscillators::stochastic(candles, 14, 3);
    if k_line.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "Stochastic (14, 3)".to_string(),
        scale: SeriesScale::Fixed { min: 0.0, max: 100.0 },
        levels: vec![80.0, 20.0],
        lines: vec![
            SeriesLine { name: "%K".to_string(), color: RSI_BLUE, values: k_line },
            SeriesLine { name: "%D".to_string(), color: 0xf59e0b, values: d_line },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// CCI (20): one line, ±100 reference levels, auto-scaled (a CCI legitimately
/// runs to several hundred in a trend).
fn cci_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let values = crate::indicators::oscillators::cci(candles, 20);
    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "CCI (20)".to_string(),
        scale: SeriesScale::Auto,
        levels: vec![100.0, -100.0],
        lines: vec![SeriesLine { name: "cci".to_string(), color: PRED_VIOLET, values }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// Williams %R (14): fixed -100..0, the -80/-20 bands.
fn williams_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let values = crate::indicators::oscillators::williams_r(candles, 14);
    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "Williams %R (14)".to_string(),
        scale: SeriesScale::Fixed { min: -100.0, max: 0.0 },
        levels: vec![-20.0, -80.0],
        lines: vec![SeriesLine { name: "%R".to_string(), color: RSI_BLUE, values }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// ROC (12): one line on the zero level, auto-scaled.
fn roc_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let values = crate::indicators::oscillators::roc(&closes, 12);
    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "ROC (12)".to_string(),
        scale: SeriesScale::Auto,
        levels: vec![0.0],
        lines: vec![SeriesLine { name: "roc".to_string(), color: 0xf59e0b, values }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// OBV: Granville's running total, auto-scaled, no levels.
fn obv_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let values = crate::indicators::oscillators::obv(candles);
    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "OBV".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![SeriesLine { name: "obv".to_string(), color: 0x34d399, values }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// Wilder's ADX (14): the ADX line with +DI and -DI, fixed 0..100, the 25
/// trend-strength level.
fn adx_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let (plus, minus, adx_line) = crate::indicators::trend::adx(candles, 14);
    if adx_line.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "ADX (14)".to_string(),
        scale: SeriesScale::Fixed { min: 0.0, max: 100.0 },
        levels: vec![25.0],
        lines: vec![
            SeriesLine { name: "adx".to_string(), color: RSI_BLUE, values: adx_line },
            SeriesLine { name: "+di".to_string(), color: 0x34d399, values: plus },
            SeriesLine { name: "-di".to_string(), color: 0xfb7185, values: minus },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// Bollinger %B and bandwidth: where the close sits inside the bands (the
/// band edges as levels) beside the bands' relative width (the squeeze's
/// own line).
fn percent_b_pane(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let bands = crate::indicators::bollinger::bollinger(&closes, 20, 2.0);
    if bands.iter().all(|b| b.is_none()) {
        return None;
    }
    let pct_b: Vec<Option<f64>> = (0..candles.len())
        .map(|i| {
            bands[i].map(|b| {
                let width = b.upper - b.lower;
                if width > 0.0 { (closes[i] - b.lower) / width } else { 0.5 }
            })
        })
        .collect();
    let bandwidth: Vec<Option<f64>> = bands.iter().map(|b| b.map(|p| p.bandwidth)).collect();
    Some(IndicatorSeries {
        label: "Bollinger %B + bandwidth".to_string(),
        scale: SeriesScale::Auto,
        levels: vec![1.0, 0.0],
        lines: vec![
            SeriesLine { name: "%b".to_string(), color: RSI_BLUE, values: pct_b },
            SeriesLine { name: "bandwidth".to_string(), color: 0xf59e0b, values: bandwidth },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Pane,
    })
}

/// Keltner (20, 10, 2): the center EMA and both ATR bands on the price plot.
fn keltner_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let bands = crate::indicators::trend::keltner(candles, 20, 10, 2.0);
    if bands.iter().all(|b| b.is_none()) {
        return None;
    }
    let pick = |f: fn((f64, f64, f64)) -> f64| -> Vec<Option<f64>> { bands.iter().map(|b| b.map(f)).collect() };
    Some(IndicatorSeries {
        label: "Keltner (20, 10, 2)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![
            SeriesLine { name: "upper".to_string(), color: 0x94a3b8, values: pick(|(u, _, _)| u) },
            SeriesLine { name: "center".to_string(), color: 0xf59e0b, values: pick(|(_, c, _)| c) },
            SeriesLine { name: "lower".to_string(), color: 0x94a3b8, values: pick(|(_, _, l)| l) },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// Donchian (20): the 20-bar high/low channel and its middle, on the plot.
fn donchian_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let bands = crate::indicators::trend::donchian(candles, 20);
    if bands.iter().all(|b| b.is_none()) {
        return None;
    }
    let pick = |f: fn((f64, f64, f64)) -> f64| -> Vec<Option<f64>> { bands.iter().map(|b| b.map(f)).collect() };
    Some(IndicatorSeries {
        label: "Donchian (20)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![
            SeriesLine { name: "upper".to_string(), color: 0x94a3b8, values: pick(|(u, _, _)| u) },
            SeriesLine { name: "middle".to_string(), color: 0xf59e0b, values: pick(|(_, m, _)| m) },
            SeriesLine { name: "lower".to_string(), color: 0x94a3b8, values: pick(|(_, _, l)| l) },
        ],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// Supertrend (10, 3): the trailing line on the price plot; its flips are
/// the concept's signals.
fn supertrend_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let st = crate::indicators::trend::supertrend(candles, 10, 3.0);
    if st.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "Supertrend (10, 3)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![SeriesLine {
            name: "supertrend".to_string(),
            color: 0x22d3ee,
            values: st.iter().map(|v| v.map(|(value, _)| value)).collect(),
        }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// Parabolic SAR (0.02, 0.2): the accelerating stop on the price plot.
fn psar_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let sar = crate::indicators::trend::parabolic_sar(candles, 0.02, 0.2);
    if sar.iter().all(|v| v.is_none()) {
        return None;
    }
    Some(IndicatorSeries {
        label: "Parabolic SAR".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines: vec![SeriesLine { name: "psar".to_string(), color: 0xf472b6, values: sar }],
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
}

/// The Guppy ribbon (GMMA) on the price plot: the short group (3, 5, 8, 10,
/// 12, 15) in one tone and the long group (30, 35, 40, 45, 50, 60) in
/// another -- the groups separating and joining IS the trend's life cycle,
/// the oldest ribbon in the book.
fn ema_family_overlay(candles: &[Candle]) -> Option<IndicatorSeries> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    if closes.len() < 70 {
        return None;
    }
    let short: [usize; 6] = [3, 5, 8, 10, 12, 15];
    let long: [usize; 6] = [30, 35, 40, 45, 50, 60];
    let mut lines = Vec::new();
    for (group, periods, base) in [("s", &short, (0x22u32, 0xd3u32, 0xeeu32)), ("l", &long, (0xf4u32, 0x72u32, 0xb6u32))] {
        for (i, period) in periods.iter().enumerate() {
            let shade = 1.0 - i as f64 * 0.09; // later lines of a group dim a little
            let color = ((base.0 as f64 * shade) as u32) << 16
                | ((base.1 as f64 * shade) as u32) << 8
                | (base.2 as f64 * shade) as u32;
            lines.push(SeriesLine {
                name: if i == 0 || i == periods.len() - 1 {
                    format!("ema{group}{period}")
                } else {
                    String::new()
                },
                color,
                values: crate::indicators::ema::ema(&closes, *period),
            });
        }
    }
    Some(IndicatorSeries {
        label: "EMA Ribbon (GMMA)".to_string(),
        scale: SeriesScale::Auto,
        levels: Vec::new(),
        lines,
        segments: Vec::new(),
        markers: Vec::new(),
        bars: None,
        target: SeriesTarget::Overlay,
    })
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
    fn the_family_ribbon_is_every_period_on_one_scale() {
        let spec = indicator_series("indicators.rsi_family", &moving_series(120)).expect("spec");
        assert_eq!(spec.lines.len(), 23, "every period 2..=24");
        assert_eq!(spec.scale, SeriesScale::Fixed { min: 0.0, max: 100.0 });
        // Each line warms up for exactly its own period.
        for (i, line) in spec.lines.iter().enumerate() {
            let period = i + 2;
            assert!(line.values[..period].iter().all(|v| v.is_none()), "rsi{period} warms up");
            assert!(line.values[period].is_some(), "rsi{period} starts on time");
            for v in line.values.iter().flatten() {
                assert!((0.0..=100.0).contains(v), "rsi{period} bounded: {v}");
            }
        }
        // Only the edges carry names; the gradient runs amber to blue.
        assert_eq!(spec.lines[0].name, "rsi2");
        assert_eq!(spec.lines[22].name, "rsi24");
        assert!(spec.lines[1].name.is_empty());
        assert_eq!(spec.lines[0].color, 0xf59e0b, "the fast edge is amber");
        assert_eq!(spec.lines[22].color, 0x3b82f6, "the slow edge is blue");
        // The fast line is genuinely more reactive than the slow one: its
        // bar-to-bar swings average larger.
        let swing = |values: &[Option<f64>]| {
            let vs: Vec<f64> = values.iter().flatten().copied().collect();
            vs.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / vs.len() as f64
        };
        assert!(
            swing(&spec.lines[0].values) > swing(&spec.lines[22].values),
            "the fast edge swings wider: {} vs {}",
            swing(&spec.lines[0].values),
            swing(&spec.lines[22].values)
        );
    }

    #[test]
    fn the_macd_family_ribbon_keeps_the_ratio_and_the_zero() {
        let spec = indicator_series("indicators.macd_family", &moving_series(300)).expect("spec");
        assert_eq!(spec.lines.len(), 23);
        assert_eq!(spec.levels, vec![0.0]);
        // The slow=2x fast ratio means the slowest pair (24, 48) warms up
        // last: its line starts later than the fastest pair's (2, 4).
        let first = |values: &[Option<f64>]| values.iter().position(|v| v.is_some()).expect("some");
        assert!(first(&spec.lines[22].values) > first(&spec.lines[0].values), "slower pairs warm up later");
        // MACD lines of an oscillating series cross zero somewhere.
        let vs: Vec<f64> = spec.lines[0].values.iter().flatten().copied().collect();
        assert!(vs.iter().any(|v| *v > 0.0) && vs.iter().any(|v| *v < 0.0), "the fast line swings through zero");
    }

    #[test]
    fn the_atr_family_is_twenty_three_positive_lines() {
        let spec = indicator_series("indicators.atr_family", &moving_series(120)).expect("spec");
        assert_eq!(spec.lines.len(), 23);
        for line in &spec.lines {
            for v in line.values.iter().flatten() {
                assert!(*v > 0.0, "ATR is positive");
            }
        }
    }

    #[test]
    fn the_gmma_ribbon_is_two_groups_of_prices() {
        let candles = moving_series(200);
        let spec = indicator_series("indicators.ema_family", &candles).expect("spec");
        assert_eq!(spec.target, SeriesTarget::Overlay);
        assert_eq!(spec.lines.len(), 12, "six short + six long");
        let names: Vec<&str> = spec.lines.iter().map(|l| l.name.as_str()).filter(|n| !n.is_empty()).collect();
        assert_eq!(names, vec!["emas3", "emas15", "emal30", "emal60"]);
        // Every value is a price inside the traded range, and the long group
        // warms up after the short group.
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in &candles {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
        }
        for line in &spec.lines {
            for v in line.values.iter().flatten() {
                assert!(*v >= lo * 0.95 && *v <= hi * 1.05, "a price: {v}");
            }
        }
        let first = |values: &[Option<f64>]| values.iter().position(|v| v.is_some()).expect("some");
        assert!(first(&spec.lines[11].values) > first(&spec.lines[0].values), "ema60 after ema3");
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
