//! Pine-lite script panes: run a script, position its plots (`docs/23`).
//!
//! The rule is the scene's own: the math belongs to `pine-lite` (which gets it
//! from `analytics-core`), the mapping to this module, and the painting to the
//! shell. [`ScriptSpec`] is the request side -- source plus input values --
//! and [`SceneScriptPane`] is the response side: one pane whose plots are
//! already positioned polylines with their legend colors and titles resolved.

use serde::{Deserialize, Serialize};

use pine_lite::{run, Inputs, Output};

/// One script attached to the chart: its source and the host-supplied input
/// values for its `input.*` declarations. The gateway vets the source before
/// it can be stored; the engine compiles per run, which is fast enough at
/// these budgets (≤500 statements) and keeps the ABI a plain string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptSpec {
    /// The script source, including its `//@pine_lite` header.
    pub source: String,
    /// Input values by variable name. Anything the script declares but the
    /// host does not supply runs on its declared default.
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, f64>,
    /// The second instrument (`sec=` in the header), time-aligned onto the
    /// chart's own bars: candle i covers the same window as candle i. The
    /// shell fills it by fetching the pair's klines; empty when absent, and
    /// then `request.*` reads are the VM's data-missing error.
    #[serde(default)]
    pub security: Vec<analytics_core::types::Candle>,
}

/// One positioned script plot inside a [`SceneScriptPane`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptPlot {
    /// `title=`, or `plot 2`.
    pub title: String,
    /// The polyline, in canvas coordinates. `na` bars are skipped by the
    /// engine -- the shell draws whatever this carries, gaps included, which
    /// is what a line-with-na should look like.
    pub points: Vec<crate::scene::Point>,
    /// Packed RGBA; the shell renders it as `#rrggbbaa`.
    pub color: u32,
    /// Line width in CSS pixels.
    pub linewidth: f64,
}

/// One horizontal reference level inside a script pane (`hline()`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScriptLevel {
    /// Canvas y.
    pub y: f64,
    /// The value it marks.
    pub value: f64,
    /// Packed RGBA.
    pub color: u32,
}

/// One point marker (`plotshape`/`plotchar`), positioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptShape {
    /// Canvas x -- the center of the marker's bar slot.
    pub x: f64,
    /// Canvas y (`location.absolute`), or the pane's bottom when the shape
    /// had no absolute value and the renderer should anchor to the pane.
    pub y: f64,
    /// The shape/char name, from the renderer's glyph table.
    pub glyph: String,
    /// Packed RGBA.
    pub color: u32,
}

/// One script's pane: the plots, levels and shapes it asked for, in its own
/// y-scale, already positioned. A script with `overlay=true` comes back
/// differently -- see [`scene_overlay_plots`] -- because sharing the price
/// pane's scale is a different claim than owning one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneScriptPane {
    /// Stable id: `script:{title}` -- the shell's chip and legend key.
    pub id: String,
    /// The `title=` from the header, or `script`.
    pub title: String,
    /// The pane's own plot rectangle, below the price plot.
    pub plot: crate::scene::Plot,
    /// The plots, in source order.
    pub plots: Vec<ScriptPlot>,
    /// The `hline()` levels.
    pub levels: Vec<ScriptLevel>,
    /// The point markers.
    pub shapes: Vec<ScriptShape>,
    /// The pane's y-axis range, so the shell can draw its own ticks.
    pub value_min: f64,
    /// The top of the pane's y-range.
    pub value_max: f64,
}

/// An overlay script's plots and markers, mapped through the price pane's
/// scale. No levels: `hline()` in an overlay script means a *price* level,
/// and those ride the same [`ScriptLevel`] mapping only a pane can give.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptOverlay {
    /// Stable id, `script:{title}`.
    pub id: String,
    /// The header's title.
    pub title: String,
    /// The line plots, positioned on the price pane.
    pub plots: Vec<ScriptPlot>,
    /// The point markers (`plotshape`), positioned on the price pane.
    pub shapes: Vec<ScriptShape>,
}

/// Run one script and lay its pane out.
///
/// Total on failure: an error is returned as text and the caller puts it in
/// the scene note -- a script that cannot run is a caveat, not a dead frame.
pub fn script_pane(
    spec: &ScriptSpec,
    candles: &[analytics_core::types::Candle],
    slot: f64,
    plot: &crate::scene::Plot,
) -> Result<SceneScriptPane, String> {
    let (header, parsed) =
        pine_lite::vet(&spec.source).map_err(|errs| format_script_errors(&errs))?;
    let inputs = Inputs {
        numbers: spec.inputs.clone(),
        security: spec.security.clone(),
        ..Inputs::default()
    };
    let output: Output = run(&parsed, candles, &inputs).map_err(|err| err.to_string())?;
    pane_from_output(&header, &output, slot, plot)
}

/// Build the pane from a run's output: scale each plot to the pane's own
/// y-range and position every point.
///
/// The y-range is the union of the finite plot values (plus hlines), padded
/// 5% -- an empty series yields the 0..1 fallback so a pane still draws its
/// frame and the note says why it is empty.
pub fn pane_from_output(
    header: &pine_lite::Header,
    output: &Output,
    slot: f64,
    plot: &crate::scene::Plot,
) -> Result<SceneScriptPane, String> {
    let title = header.title.clone().unwrap_or_else(|| "script".to_string());
    let id = format!("script:{title}");

    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for p in &output.plots {
        for v in &p.values {
            if v.is_finite() {
                min = min.min(*v);
                max = max.max(*v);
            }
        }
    }
    for h in &output.hlines {
        if h.value.is_finite() {
            min = min.min(h.value);
            max = max.max(h.value);
        }
    }
    if !(min.is_finite() && max.is_finite()) {
        return Err(format!("script `{title}` produced no plottable values"));
    }
    if (max - min).abs() < f64::EPSILON {
        // A flat series still deserves a pane that reads: pad around it.
        min -= 1.0;
        max += 1.0;
    }
    let pad = (max - min) * 0.05;
    let (value_min, value_max) = (min - pad, max + pad);

    let y_at = |v: f64| plot.y + plot.h - (v - value_min) / (value_max - value_min) * plot.h;

    let plots = output
        .plots
        .iter()
        .filter(|p| p.kind == pine_lite::PlotKind::Line)
        .map(|p| ScriptPlot {
            title: p.title.clone(),
            points: p
                .values
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .map(|(bar, v)| crate::scene::Point {
                    x: plot.x + slot * (bar as f64 + 0.5),
                    y: y_at(*v),
                })
                .collect(),
            color: p.color,
            linewidth: p.linewidth,
        })
        .collect();

    let levels = output
        .hlines
        .iter()
        .filter(|h| h.value.is_finite())
        .map(|h| ScriptLevel {
            y: y_at(h.value),
            value: h.value,
            color: h.color,
        })
        .collect();

    let shapes = output
        .shapes
        .iter()
        .map(|s| ScriptShape {
            x: plot.x + slot * (s.bar as f64 + 0.5),
            // A shape without `location_value=` has no absolute value: anchor
            // it just above the pane's bottom edge -- the pane-adapted form of
            // Pine's below-bar markers, so plotshape() always renders.
            y: if s.value.is_finite() { y_at(s.value) } else { plot.y + plot.h - 6.0 },
            glyph: s.glyph.clone(),
            color: s.color,
        })
        .collect();

    Ok(SceneScriptPane {
        id,
        title,
        plot: *plot,
        plots,
        levels,
        shapes,
        value_min,
        value_max,
    })
}

/// Position an *overlay* script's line plots onto the price pane's own scale.
///
/// An overlay script (`overlay=true`) plots into the price range the frame
/// already maps -- the same reason VWAP is a line in the price pane and not a
/// second axis. A plot value outside the visible range is simply off-plot and
/// the canvas clips it.
#[allow(private_interfaces)] // `Frame` is crate-private by design
pub fn scene_overlay_plots(
    output: &Output,
    slot: f64,
    frame: &crate::scene::Frame,
) -> Vec<ScriptPlot> {
    let price_to_y = |v: f64| {
        crate::scene::price_to_y(v, frame.price_min, frame.price_max, &frame.plot)
    };
    output
        .plots
        .iter()
        .filter(|p| p.kind == pine_lite::PlotKind::Line)
        .map(|p| ScriptPlot {
            title: p.title.clone(),
            points: p
                .values
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .map(|(bar, v)| crate::scene::Point {
                    x: frame.plot.x + slot * (bar as f64 + 0.5),
                    y: price_to_y(*v),
                })
                .collect(),
            color: p.color,
            linewidth: p.linewidth,
        })
        .collect()
}

/// Position an overlay script's `plotshape` markers on the price pane.
///
/// A shape carries the price it marks (`location_value=`) or nothing at all --
/// and nothing at all still needs a place to sit, so it anchors a few percent
/// above the bottom of the price range, which reads as Pine's below-bar
/// marker without pretending the script told us the bar's low.
#[allow(private_interfaces)] // `Frame` is crate-private by design
pub fn scene_overlay_shapes(
    output: &Output,
    slot: f64,
    frame: &crate::scene::Frame,
) -> Vec<ScriptShape> {
    let (lo, hi) = (frame.price_min, frame.price_max);
    output
        .shapes
        .iter()
        .map(|s| {
            let value = if s.value.is_finite() { s.value } else { lo + (hi - lo) * 0.04 };
            ScriptShape {
                x: frame.plot.x + slot * (s.bar as f64 + 0.5),
                y: crate::scene::price_to_y(value, lo, hi, &frame.plot),
                glyph: s.glyph.clone(),
                color: s.color,
            }
        })
        .collect()
}

/// Human-readable multi-error rendering, for the scene note.
fn format_script_errors(errs: &[pine_lite::ScriptError]) -> String {
    errs.iter()
        .map(|e| format!("line {}: {}", e.span.line, e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::{Candle, Timeframe};

    fn candle(i: usize, close: f64) -> Candle {
        Candle {
            symbol: "T".into(),
            timeframe: Timeframe::M1,
            open_time: i as i64 * 60_000_000_000,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
            buy_volume: 5.0,
            sell_volume: 5.0,
        }
    }

    fn plot_rect() -> crate::scene::Plot {
        crate::scene::Plot { x: 10.0, y: 20.0, w: 380.0, h: 90.0 }
    }

    const RSI_SCRIPT: &str = "//@pine_lite version=1 overlay=false title=\"RSI\"\n\
         r = ta.rsi(close, 14)\n\
         plot(r, title=\"RSI\", color=color.purple)\n\
         hline(70)\n\
         hline(30)\n";

    #[test]
    fn an_rsi_script_yields_a_positioned_pane() {
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0 + ((i % 7) as f64))).collect();
        let pane = script_pane(
            &ScriptSpec { source: RSI_SCRIPT.into(), inputs: Default::default(), security: Vec::new() },
            &candles,
            10.0,
            &plot_rect(),
        )
        .expect("pane");
        assert_eq!(pane.title, "RSI");
        assert_eq!(pane.plots.len(), 1);
        // One polyline per hline too? No: hlines are levels, not plots. But
        // the shell's own debug caught a real bug here once -- the two
        // `hline()` calls must land as two levels.
        assert_eq!(pane.levels.len(), 2, "hline(70) and hline(30) are two levels");
        // `analytics_core::rsi` produces its first value at index `period`
        // (14), so bars 0..13 are na and absent from the polyline.
        assert_eq!(pane.plots[0].points.len(), 40 - 14);
        // The y positions sit inside the pane.
        for p in &pane.plots[0].points {
            assert!(p.y >= plot_rect().y && p.y <= plot_rect().y + plot_rect().h);
        }
        // The 70 level maps above the 30 level (canvas y grows downward).
        assert!(pane.levels[0].y < pane.levels[1].y);
    }

    #[test]
    fn a_broken_script_returns_its_errors() {
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0)).collect();
        let err = script_pane(
            &ScriptSpec {
                source: "//@pine_lite version=1\nx = zzz\n".into(),
                inputs: Default::default(),
                security: Vec::new(),
            },
            &candles,
            10.0,
            &plot_rect(),
        )
        .expect_err("refused");
        assert!(err.contains("zzz"), "{err}");
    }

    #[test]
    fn an_empty_series_is_an_error_not_a_pane() {
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0)).collect();
        // plot(close) always has values; an empty pane needs a script that
        // never assigns -- so assert the *broken* path differently: a script
        // whose only plot is a ta series too short to warm up.
        let err = script_pane(
            &ScriptSpec {
                source: "//@pine_lite version=1\nm = ta.sma(close, 30)\nplot(m)\n".into(),
                inputs: Default::default(),
                security: Vec::new(),
            },
            &candles,
            10.0,
            &plot_rect(),
        );
        assert!(err.is_err(), "an all-na series must refuse, not draw an empty pane");
    }

    #[test]
    fn overlay_plots_map_through_the_price_scale() {
        // 40 candles at close=100 flat: a frame spanning 99..101.
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from: 0,
            to: 40 * 60_000_000_000,
            price_min: 99.0,
            price_max: 101.0,
            bar_nanos: 60_000_000_000,
        };
        let (header, parsed) =
            pine_lite::vet("//@pine_lite version=1 overlay=true\nplot(close)\n").expect("vet");
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0)).collect();
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        let plots = scene_overlay_plots(&output, 10.0, &frame);
        assert_eq!(plots.len(), 1);
        // close=100 is the middle of 99..101, so y is the plot's middle.
        let mid = plot_rect().y + plot_rect().h / 2.0;
        assert!((plots[0].points[20].y - mid).abs() < 1.0, "{}", plots[0].points[20].y);
        let _ = header;
    }

    #[test]
    fn overlay_shapes_land_on_the_price_scale() {
        // An overlay script that marks every 10th bar: markers must position
        // through the price scale, not vanish (the pane path used to drop
        // shapes without a value; an overlay dropped them all).
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from: 0,
            to: 40 * 60_000_000_000,
            price_min: 99.0,
            price_max: 101.0,
            bar_nanos: 60_000_000_000,
        };
        let src = "//@pine_lite version=1 overlay=true\nplot(close)\nmark = close == close\nplotshape(mark, color=color.green, location_value=high)\n";
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0)).collect();
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        let shapes = scene_overlay_shapes(&output, 10.0, &frame);
        assert_eq!(shapes.len(), 40, "one marker per bar");
        // high=101 is the top of the 99..101 frame, so it maps to the plot's
        // own top edge.
        assert!((shapes[20].y - plot_rect().y).abs() < 1.0, "high=101 is the top: {}", shapes[20].y);
    }

    #[test]
    fn overlay_shapes_without_a_value_still_get_a_place() {
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from: 0,
            to: 40 * 60_000_000_000,
            price_min: 99.0,
            price_max: 101.0,
            bar_nanos: 60_000_000_000,
        };
        let src = "//@pine_lite version=1 overlay=true\nplot(close)\nmark = close == close\nplotshape(mark, color=color.green)\n";
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0)).collect();
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        let shapes = scene_overlay_shapes(&output, 10.0, &frame);
        assert_eq!(shapes.len(), 40);
        // Anchored near the bottom of the price range, inside the plot.
        assert!(shapes[0].y > plot_rect().y + plot_rect().h * 0.85, "{}", shapes[0].y);
        assert!(shapes[0].y <= plot_rect().y + plot_rect().h, "{}", shapes[0].y);
    }
}
