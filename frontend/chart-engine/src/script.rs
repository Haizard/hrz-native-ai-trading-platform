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
    /// Input values by variable name, typed (docs/25 layer settings): JSON
    /// numbers feed `input.int` / `input.float`, booleans feed `input.bool`.
    /// Anything the script declares but the host does not supply runs on its
    /// declared default. An older shell sending `{ "len": 14 }` deserializes
    /// unchanged -- numbers were the whole of the old shape.
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, serde_json::Value>,
    /// The second instrument (`sec=` in the header), time-aligned onto the
    /// chart's own bars: candle i covers the same window as candle i. The
    /// shell fills it by fetching the pair's klines; empty when absent, and
    /// then `request.*` reads are the VM's data-missing error.
    #[serde(default)]
    pub security: Vec<analytics_core::types::Candle>,
    /// Every `request.security("SYM", "tf", ...)` pair the script names,
    /// keyed `SYM@TF` and time-aligned onto the chart's bars by the shell
    /// (same carry-forward contract as `security`). The engine hands the
    /// pool straight to the VM. Absent on older shells' requests — the
    /// script's reads then report the missing-key error, which is the truth.
    #[serde(default)]
    pub series_pool: std::collections::HashMap<String, Vec<analytics_core::types::Candle>>,
    /// `request.data("name")` series (docs/23 Phase 14): platform feeds as
    /// per-bar values aligned onto the chart's bars, filled by the shell
    /// from the same sources the gateway preview uses (ticker fields today).
    #[serde(default)]
    pub data_series: std::collections::HashMap<String, Vec<f64>>,
}

/// One positioned drawing object (docs/23 Phase 13): the script chose
/// bar-index + price coordinates; the engine maps them to the canvas with
/// the same frame every plot and shape uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScriptDraw {
    /// `line.new` — two anchors, canvas coordinates.
    Line {
        /// First anchor.
        x1: f64,
        /// First anchor.
        y1: f64,
        /// Second anchor.
        x2: f64,
        /// Second anchor.
        y2: f64,
        /// Packed RGBA.
        color: u32,
        /// "solid" | "dashed" | "dotted".
        style: String,
        /// Width in CSS pixels.
        width: f64,
    },
    /// `label.new` — anchor + text.
    Label {
        /// Anchor.
        x: f64,
        /// Anchor.
        y: f64,
        /// The text.
        text: String,
        /// Packed RGBA.
        color: u32,
    },
    /// `box.new` — rectangle.
    Box {
        /// Left.
        x1: f64,
        /// Top.
        y1: f64,
        /// Right.
        x2: f64,
        /// Bottom.
        y2: f64,
        /// Packed RGBA (fill).
        color: u32,
    },
}

/// Every drawing object of one overlay script, in creation order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptObjects {
    /// The objects, ordered.
    pub objects: Vec<ScriptDraw>,
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
    /// The bar index the marker sits on, counted over the visible slice the
    /// engine ran the script on. The shell reads it to single out *fresh*
    /// markers -- one that landed on the newest bars -- for the pulse; a
    /// marker's story ("this just happened") is part of its meaning.
    pub bar: usize,
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
    /// The bottom of the pane's y-range (lowest plotted value).
    pub value_min: f64,
    /// The top of the pane's y-range (highest plotted value).
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
    /// The drawing objects (`line.new`/`label.new`/`box.new`), positioned.
    #[serde(default)]
    pub objects: Vec<ScriptDraw>,
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
    let (header, output) = run_script(spec, candles)?;
    pane_from_output(&header, &output, slot, plot)
}

/// Run one script and lay out everything it produces: the pane itself plus,
/// for strategies, the positioned fills and the equity pane (docs/24 S2).
/// One run feeds all three -- the strategy extras are projections of the
/// same `Output`, never a second interpretation of a second run.
#[allow(private_interfaces)] // `Frame` is crate-private by design
pub fn script_pane_full(
    spec: &ScriptSpec,
    candles: &[analytics_core::types::Candle],
    slot: f64,
    plot: &crate::scene::Plot,
    equity_plot: &crate::scene::Plot,
    frame: &crate::scene::Frame,
) -> Result<(SceneScriptPane, Option<ScriptStrategyLayer>, Option<ScriptEquityPane>), String> {
    let (header, output) = run_script(spec, candles)?;
    let pane = pane_from_output(&header, &output, slot, plot)?;
    let title = header.title.clone().unwrap_or_else(|| "script".into());
    let layer = scene_strategy_layer(&title, &output, slot, frame);
    let equity = equity_pane(&title, &output, slot, equity_plot);
    Ok((pane, layer, equity))
}

/// Vet and run one script: the shared front half of [`script_pane`] and
/// [`script_pane_full`].

/// Build the pane from a run's output: scale each plot to the pane's own
/// y-range and position every point.
///
/// The y-range is the union of the finite plot values (plus hlines), padded
/// 5% -- an empty series yields the 0..1 fallback so a pane still draws its
/// frame and the note says why it is empty.
/// Build the VM's typed `Inputs` from a spec (docs/25 layer settings). The
/// spec's values are JSON-typed; each scalar sorts into the map its kind
/// belongs to. A value whose kind disagrees with the script's declaration
/// (a bool sent for an `input.int`) lands in a map the declaration never
/// reads, so the declared default applies -- the honest "that setting does
/// not exist", not a coerced wrong-typed value. Non-scalar JSON (null,
/// arrays, objects) is not an input value at all and is skipped.
///
/// Shared by the sub-pane path ([`run_script`]) and the scene's overlay path,
/// so both spell the sorting the same way.
#[must_use]
pub fn inputs_from_spec(spec: &ScriptSpec) -> Inputs {
    let mut inputs = Inputs {
        security: spec.security.clone(),
        series_pool: spec.series_pool.clone(),
        data_series: spec.data_series.clone(),
        ..Inputs::default()
    };
    for (name, value) in &spec.inputs {
        match value {
            serde_json::Value::Number(n) => {
                if let Some(n) = n.as_f64() {
                    inputs.numbers.insert(name.clone(), n);
                }
            }
            serde_json::Value::Bool(b) => {
                inputs.bools.insert(name.clone(), *b);
            }
            serde_json::Value::String(s) => {
                // The VM grows a string evaluator when the language does; the
                // map exists so that day needs no ABI change.
                inputs.strings.insert(name.clone(), s.clone());
            }
            _ => {}
        }
    }
    inputs
}

/// Vet and run one script: the shared front half of [`script_pane`] and
/// [`script_pane_full`].
fn run_script(
    spec: &ScriptSpec,
    candles: &[analytics_core::types::Candle],
) -> Result<(pine_lite::Header, Output), String> {
    let (header, parsed) =
        pine_lite::vet(&spec.source).map_err(|errs| format_script_errors(&errs))?;
    let inputs = inputs_from_spec(spec);
    let output: Output = run(&parsed, candles, &inputs).map_err(|err| err.to_string())?;
    Ok((header, output))
}

/// Lay out a script pane from an already-run output: the projection
/// [`script_pane`] performs, shared with the strategy path
/// ([`script_pane_full`]) so one run feeds pane + layer + equity.
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
            bar: s.bar,
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
                bar: s.bar,
            }
        })
        .collect()
}

/// Position an overlay script's drawing objects (docs/23 Phase 13): the
/// script's bar-index + price coordinates through the SAME frame mapping
/// plots and shapes use. Negative bar indexes count back from the last bar
/// (Pine's `bar_index - n` convention); an object whose anchors fall outside
/// the visible window still maps — the canvas clips it.
pub fn scene_overlay_objects(
    output: &Output,
    slot: f64,
    frame: &crate::scene::Frame,
) -> Vec<ScriptDraw> {
    let (lo, hi) = (frame.price_min, frame.price_max);
    // Bars in the window. The frame's edges are ABSOLUTE unix-nanos
    // timestamps, so the count is the span over one bar -- `frame.to /
    // frame.bar_nanos` is a *date* in bar units (tens of millions), which
    // used to throw every negative (right-edge-anchored) coordinate millions
    // of bar-slots off the right side of the canvas. A Pine-style
    // `box.new(start, top, -1, bottom)` "extend to now" was invisible.
    let last = ((frame.to - frame.from) / frame.bar_nanos.max(1)) as f64;
    let bx = |bar: f64| -> f64 {
        // Negative indexes anchor from the right edge; >= 0 from the left.
        let idx = if bar < 0.0 { last + bar } else { bar };
        frame.plot.x + slot * (idx + 0.5)
    };
    let py = |price: f64| -> f64 { crate::scene::price_to_y(price, lo, hi, &frame.plot) };
    output
        .objects
        .iter()
        .filter_map(|o| match o {
            pine_lite::interp::ScriptObject::Line { bar1, price1, bar2, price2, color, style, width } => {
                if !bar1.is_finite() || !bar2.is_finite() || !price1.is_finite() || !price2.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Line {
                    x1: bx(*bar1),
                    y1: py(*price1),
                    x2: bx(*bar2),
                    y2: py(*price2),
                    color: *color,
                    style: style.clone(),
                    width: *width,
                })
            }
            pine_lite::interp::ScriptObject::Label { bar, price, text, color } => {
                if !bar.is_finite() || !price.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Label { x: bx(*bar), y: py(*price), text: text.clone(), color: *color })
            }
            pine_lite::interp::ScriptObject::Box { left, top, right, bottom, color } => {
                if !left.is_finite() || !right.is_finite() || !top.is_finite() || !bottom.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Box {
                    x1: bx(*left),
                    y1: py(*top),
                    x2: bx(*right),
                    y2: py(*bottom),
                    color: *color,
                })
            }
            // docs/28: time-anchored twins. The anchors are unix-nanos
            // timestamps (a pooled higher timeframe's bar times), mapped by
            // absolute time -- the same mapping drawings and regions use --
            // so an HTF zone lands exactly where its bars sit on this chart's
            // axis. An anchor outside the window maps off-plot and the canvas
            // clips, exactly as a bar-index object does.
            pine_lite::interp::ScriptObject::LineTime { t1, price1, t2, price2, color, style, width } => {
                if !t1.is_finite() || !t2.is_finite() || !price1.is_finite() || !price2.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Line {
                    x1: frame.x_at_nanos(*t1 as i64),
                    y1: py(*price1),
                    x2: frame.x_at_nanos(*t2 as i64),
                    y2: py(*price2),
                    color: *color,
                    style: style.clone(),
                    width: *width,
                })
            }
            pine_lite::interp::ScriptObject::LabelTime { nanos, price, text, color } => {
                if !nanos.is_finite() || !price.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Label {
                    x: frame.x_at_nanos(*nanos as i64),
                    y: py(*price),
                    text: text.clone(),
                    color: *color,
                })
            }
            pine_lite::interp::ScriptObject::BoxTime { left_nanos, top, right_nanos, bottom, color } => {
                if !left_nanos.is_finite() || !right_nanos.is_finite() || !top.is_finite() || !bottom.is_finite() {
                    return None;
                }
                Some(ScriptDraw::Box {
                    x1: frame.x_at_nanos(*left_nanos as i64),
                    y1: py(*top),
                    x2: frame.x_at_nanos(*right_nanos as i64),
                    y2: py(*bottom),
                    color: *color,
                })
            }
        })
        .collect()
}

/// One simulated fill, positioned on the price pane (docs/24 S2). The
/// strategy-side twin of [`ScriptShape`]: the engine mapped the sim's fill
/// through the same frame every plot and marker uses, so the shell only
/// paints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptFill {
    /// Canvas x of the fill. The sim's order bar is the *decision* bar; the
    /// fill lands on the NEXT bar's open (docs/24 S1) -- a stop that gaps
    /// fills intrabar on that same next bar, at the mapped stop price.
    pub x: f64,
    /// Canvas y of the fill price on the price pane.
    pub y: f64,
    /// The fill price itself, for tooltips.
    pub price: f64,
    /// Long side when true; sides pick the marker direction and color.
    pub long: bool,
    /// The fill bar's index in the visible slice (decision + 1); the shell's
    /// fresh-marker pulse reads it the way a shape's `bar` is read.
    pub bar: usize,
}

/// One round trip: the entry fill and the closing fill that realized it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptTrade {
    /// The position's opening fill.
    pub entry: ScriptFill,
    /// The position's closing fill.
    pub exit: ScriptFill,
    /// Realized pnl of the round trip, net of commission (the sim's number).
    pub pnl: f64,
    /// The carried stop price at exit time, when one was armed.
    pub stop: Option<f64>,
}

/// The strategy report, shell-facing: the same headline numbers the gateway
/// chat row carries, so both surfaces speak one vocabulary (docs/24 §8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptReport {
    /// Realized net profit over the window, initial capital excluded.
    pub net_profit: f64,
    /// Closed round trips.
    pub total_trades: usize,
    /// Fraction of closed trades that made money, 0..1.
    pub win_rate: f64,
    /// Gross wins over gross losses; 0 when there were no losses.
    pub profit_factor: f64,
    /// Peak-to-trough decline of the equity curve, as a fraction.
    pub max_drawdown: f64,
}

/// A strategy script's simulation, positioned for the shell (docs/24 S2).
/// Scene-level rather than a [`ScriptOverlay`] field because the fills belong
/// to the *price* pane regardless of the script's overlay flag -- a strategy's
/// trades always live where the candles are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptStrategyLayer {
    /// The script this layer belongs to (`script:{title}`).
    pub id: String,
    /// Every closed round trip, chronological.
    pub trades: Vec<ScriptTrade>,
    /// The position still held at the window's end, if any.
    pub open_position: Option<ScriptFill>,
    /// The report the shell renders in the chip's tooltip.
    pub report: ScriptReport,
}

/// A strategy's equity curve as its own pane (docs/24 S2, §8.1 decision:
/// every strategy gets a dedicated sub-pane -- an equity series' scale is
/// account-size, never the candle scale, overlay or not).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptEquityPane {
    /// Stable id: `equity:{title}`.
    pub id: String,
    /// The strategy's title.
    pub title: String,
    /// The pane's own plot rectangle.
    pub plot: crate::scene::Plot,
    /// The equity polyline, positioned and bar-aligned with the candles.
    pub points: Vec<crate::scene::Point>,
    /// The pane's y-axis range, so the shell can draw its own ticks.
    pub value_min: f64,
    /// The top of the pane's y-range.
    pub value_max: f64,
}

/// Position a strategy script's simulation onto the scene (docs/24 S2):
/// fills at their fill-bar slot on the price pane, the report verbatim, and
/// the open position when one survives the window.
///
/// `None` when the run was not a strategy (no simulation) -- the common case
/// for every indicator script, and the reason the scene costs nothing extra
/// for them.
#[allow(private_interfaces)] // `Frame` is crate-private by design
pub fn scene_strategy_layer(
    title: &str,
    output: &Output,
    slot: f64,
    frame: &crate::scene::Frame,
) -> Option<ScriptStrategyLayer> {
    let sim = output.simulation.as_ref()?;
    let (lo, hi) = (frame.price_min, frame.price_max);
    let py = |price: f64| -> f64 { crate::scene::price_to_y(price, lo, hi, &frame.plot) };
    let fill = |o: &pine_lite::sim::SimOrder| ScriptFill {
        // The sim's `bar` is the decision bar; the fill is the next open, so
        // the marker sits one slot right, on the bar that actually filled.
        x: frame.plot.x + slot * ((o.bar + 1) as f64 + 0.5),
        y: py(o.price),
        price: o.price,
        long: o.long,
        bar: o.bar + 1,
    };

    // The sim's orders are chronological fills; `pnl` marks the closer of a
    // round trip. Pairing on that marker reconstructs exactly the trades the
    // report counted, in the sim's own order.
    let mut trades = Vec::new();
    let mut open: Option<&pine_lite::sim::SimOrder> = None;
    for o in &sim.orders {
        match open.take() {
            None => {
                if o.pnl.is_none() {
                    open = Some(o);
                }
            }
            Some(entry) => trades.push(ScriptTrade {
                entry: fill(entry),
                exit: fill(o),
                pnl: o.pnl.unwrap_or(0.0),
                stop: o.stop,
            }),
        }
    }
    // A position still held has no closing fill -- the shell draws it as the
    // dashed live-position box instead.
    let open_position = open.map(|entry| fill(entry));

    Some(ScriptStrategyLayer {
        id: format!("script:{title}"),
        trades,
        open_position,
        report: ScriptReport {
            net_profit: sim.report.net_profit,
            total_trades: sim.report.total_trades as usize,
            win_rate: sim.report.win_rate,
            profit_factor: sim.report.profit_factor,
            max_drawdown: sim.report.max_drawdown,
        },
    })
}

/// Build a strategy's equity pane: the account curve over the visible
/// window, scaled to its own slice like every other pane (docs/24 S2).
/// `None` when the run was not a strategy or the curve is empty.
pub fn equity_pane(
    title: &str,
    output: &Output,
    slot: f64,
    plot: &crate::scene::Plot,
) -> Option<ScriptEquityPane> {
    let sim = output.simulation.as_ref()?;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for v in &sim.equity {
        if v.is_finite() {
            min = min.min(*v);
            max = max.max(*v);
        }
    }
    if !(min.is_finite() && max.is_finite()) {
        return None;
    }
    if (max - min).abs() < f64::EPSILON {
        // A flat curve still deserves a pane that reads: pad around it.
        min -= 1.0;
        max += 1.0;
    }
    let pad = (max - min) * 0.05;
    let (value_min, value_max) = (min - pad, max + pad);
    let y_at = |v: f64| plot.y + plot.h - (v - value_min) / (value_max - value_min) * plot.h;
    let points = sim
        .equity
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .map(|(bar, v)| crate::scene::Point {
            x: plot.x + slot * (bar as f64 + 0.5),
            y: y_at(*v),
        })
        .collect();
    Some(ScriptEquityPane {
        id: format!("equity:{title}"),
        title: title.to_string(),
        plot: *plot,
        points,
        value_min,
        value_max,
    })
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
            &ScriptSpec { source: RSI_SCRIPT.into(), inputs: Default::default(), security: Vec::new(), series_pool: Default::default(), data_series: Default::default() },
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
                series_pool: Default::default(),
                data_series: Default::default(),
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
                series_pool: Default::default(),
                data_series: Default::default(),
            },
            &candles,
            10.0,
            &plot_rect(),
        );
        assert!(err.is_err(), "an all-na series must refuse, not draw an empty pane");
    }

    #[test]
    fn a_pooled_request_security_script_panes() {
        // Phase 11 in the engine: the shell sends the pool it fetched and
        // aligned; the pane draws the PAIR's series, not the chart's.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
            "pair = request.security(\"ETHUSDT\", \"1m\", request.close())\n",
            "plot(pair)\n",
        );
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0)).collect();
        let pool: Vec<Candle> = (0..30)
            .map(|i| {
                let close = 50.0 + (i as f64) * 0.5;
                Candle {
                    symbol: "ETHUSDT".into(),
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
            })
            .collect();
        let mut series_pool = std::collections::HashMap::new();
        series_pool.insert("ETHUSDT@1M".to_string(), pool);
        let spec = ScriptSpec {
            source: src.into(),
            inputs: Default::default(),
            security: Vec::new(),
            series_pool,
            data_series: Default::default(),
        };
        let pane = script_pane(&spec, &candles, 10.0, &plot_rect()).expect("pane");
        assert_eq!(pane.plots.len(), 1);
        // The pooled close at bar 20 is 60.0, not the chart's 100.0: the
        // pane's value range must span the pair, not the chart.
        assert!(pane.value_max < 70.0, "max={} (pair, not chart)", pane.value_max);
    }

    #[test]
    fn typed_inputs_reach_the_run_from_the_wire() {
        // docs/25 layer settings: the settings popover sends JSON-typed
        // values -- a number for `input.int`, a bool for `input.bool` -- and
        // the run must see them in the VM's typed maps. The script plots
        // `close * mult` only while `on` is true, so the wire values are
        // legible in the pane's value range, not just in an internal map.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"settings\"\n",
            "mult = input.int(defval=2)\n",
            "on = input.bool(defval=0)\n",
            "v = close\n",
            "if on\n",
            "    v = close * mult\n",
            "plot(v)\n",
        );
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0)).collect();
        // The wire form: exactly what the shell's settings popover sends.
        let spec: ScriptSpec = serde_json::from_value(serde_json::json!({
            "source": src,
            "inputs": { "mult": 3, "on": true },
        }))
        .expect("the wire shape deserializes");
        let pane = script_pane(&spec, &candles, 10.0, &plot_rect()).expect("pane");
        assert_eq!(pane.plots.len(), 1);
        // close * 3 = 300 flat: the range spans 300 only if BOTH the bool
        // (the branch ran) and the number (the multiplier) landed.
        assert!(pane.value_max >= 300.0, "max={}", pane.value_max);

        // A wrong-KIND value is not coerced: `"3"` the string is not the
        // number 3, and `1` the number is not a bool, so both declared
        // defaults apply and the branch stays closed (v = close = 100).
        let spec: ScriptSpec = serde_json::from_value(serde_json::json!({
            "source": src,
            "inputs": { "mult": "3", "on": 1 },
        }))
        .expect("the wire shape deserializes");
        let pane = script_pane(&spec, &candles, 10.0, &plot_rect()).expect("pane");
        assert!(pane.value_max < 150.0, "max={} -- a wrong-typed input was coerced", pane.value_max);
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
        // The bar index rides along: the shell pulses only markers on the
        // freshest bars, so "which bar is this marker on" must survive.
        assert_eq!(shapes[20].bar, 20, "marker i sits on bar i");
    }

    #[test]
    fn drawing_objects_position_through_the_frame() {
        // A line from bar 5 to bar 25 at prices 100..110, one label at bar
        // 15/105, one box 8..18 / 104..102. All must land inside the plot.
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from: 0,
            to: 40 * 60_000_000_000,
            price_min: 99.0,
            price_max: 111.0,
            bar_nanos: 60_000_000_000,
        };
        let src = concat!(
            "//@pine_lite version=1 overlay=true\n",
            "if bar_index == 0\n",
            "    line.new(5, 100.0, 25, 110.0, color=color.blue)\n",
            "    label.new(15, 105.0, \"mid\", color=color.red)\n",
            "    box.new(8, 104.0, 18, 102.0, color=color.green)\n",
            "plot(close)\n",
        );
        let (header, parsed) = pine_lite::vet(src).expect("vet");
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0)).collect();
        let inputs = Inputs::default();
        let output = run(&parsed, &candles, &inputs).expect("run");
        assert_eq!(output.objects.len(), 3);
        let objects = scene_overlay_objects(&output, 10.0, &frame);
        assert_eq!(objects.len(), 3, "every finite object maps");
        match &objects[0] {
            ScriptDraw::Line { x1, y1, x2, y2, .. } => {
                let rect = plot_rect();
                assert!(*x1 > rect.x && *x1 < rect.x + rect.w, "{x1}");
                assert!(*x2 > rect.x && *x2 < rect.x + rect.w, "{x2}");
                // price 110 is near the top of 99..111.
                assert!(*y1 > rect.y && *y1 < rect.y + rect.h, "{y1}");
                assert!(*y2 < *y1, "higher price maps higher (smaller y)");
            }
            other => panic!("expected a line: {other:?}"),
        }
        match &objects[1] {
            ScriptDraw::Label { text, .. } => assert_eq!(text, "mid"),
            other => panic!("expected a label: {other:?}"),
        }
        let _ = header;
    }

    #[test]
    fn a_right_edge_anchored_box_lands_on_the_chart() {
        // Regression: `frame.to / frame.bar_nanos` is an absolute date in bar
        // units (tens of millions), not the window's bar count. A negative
        // bar coordinate -- Pine's "count back from the last bar", the only
        // way a script says "extend this zone to the right edge" -- was
        // anchored that many slots off-screen and never drawn. The frame
        // here carries real unix-nanos edges so `from` is nowhere near zero.
        let bar_nanos = 60_000_000_000i64;
        let bars = 40i64;
        let from = 1_755_000_000_000_000_000i64;
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from,
            to: from + bars * bar_nanos,
            price_min: 99.0,
            price_max: 111.0,
            bar_nanos,
        };
        let src = concat!(
            "//@pine_lite version=1 overlay=true\n",
            "if bar_index == 0\n",
            "    box.new(8, 104.0, -1, 102.0, color=color.green)\n",
            "plot(close)\n",
        );
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let candles: Vec<Candle> = (0..bars as usize).map(|i| candle(i, 100.0)).collect();
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        // slot = plot.w / bars, the invariant `build` keeps in production.
        let objects = scene_overlay_objects(&output, 380.0 / bars as f64, &frame);
        assert_eq!(objects.len(), 1);
        match &objects[0] {
            ScriptDraw::Box { x1, x2, .. } => {
                let rect = plot_rect();
                assert!(*x1 > rect.x && *x1 < rect.x + rect.w, "left edge on-plot: {x1}");
                // -1 counts back from the last bar: the right edge lands at
                // the window's right side, not millions of pixels past it.
                let right_edge = rect.x + rect.w;
                assert!(
                    (*x2 - right_edge).abs() <= 10.0,
                    "right edge anchored at the last bar: x2={x2} plot right={right_edge}"
                );
            }
            other => panic!("expected a box: {other:?}"),
        }
    }

    #[test]
    fn time_anchored_objects_map_by_absolute_time_not_bar_index() {
        // docs/28: a script that read a pooled higher timeframe holds its
        // zone's edges as unix-nanos timestamps, which are not bar indexes
        // on this chart. `box.new_time` / `line.new_time` / `label.new_time`
        // map through x_at_nanos -- the same mapping drawings and regions
        // use -- so the HTF zone lands exactly where its bars sit on this
        // chart's axis. A far-future right edge maps off-plot; the canvas
        // clips, exactly as the bar-index path does.
        let bar_nanos = 60_000_000_000i64;
        let bars = 40i64;
        let from = 1_755_000_000_000_000_000i64;
        let frame = crate::scene::Frame {
            plot: plot_rect(),
            from,
            to: from + bars * bar_nanos,
            price_min: 99.0,
            price_max: 111.0,
            bar_nanos,
        };
        // The script computes the anchors from its own `time` series (the
        // run window's candle times are `from + i*bar_nanos`), then draws
        // the zone from bar 8's open to a far-future edge, a trendline
        // between two times, and a label at one.
        let src = concat!(
            "//@pine_lite version=1 overlay=true\n",
            "if bar_index == 0\n",
            "    t8 = time + 8.0 * 60000000000.0\n",
            "    t18 = time + 18.0 * 60000000000.0\n",
            "    far = time + 10000000000000000.0\n",
            "    box.new_time(t8, 104.0, far, 102.0, color=color.green)\n",
            "    line.new_time(t8, 100.0, t18, 110.0, color=color.blue)\n",
            "    label.new_time(t18, 108.0, \"htf\", color=color.red)\n",
            "plot(close)\n",
        );
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let candles: Vec<Candle> = (0..bars as usize)
            .map(|i| {
                let mut c = candle(i, 100.0);
                c.open_time = from + (i as i64) * bar_nanos;
                c
            })
            .collect();
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        assert_eq!(output.objects.len(), 3);
        let slot = 380.0 / bars as f64;
        let objects = scene_overlay_objects(&output, slot, &frame);
        assert_eq!(objects.len(), 3, "every finite time anchor maps");
        let rect = plot_rect();
        // x_at_nanos puts a bar-open timestamp at the slot's LEFT edge; the
        // bar-index path centers it (+0.5). Bar 8's open: 8/40 across.
        let x8 = rect.x + (8.0 / 40.0) * rect.w;
        let x18 = rect.x + (18.0 / 40.0) * rect.w;
        match &objects[0] {
            ScriptDraw::Box { x1, y1, x2, y2, .. } => {
                assert!((x1 - x8).abs() < 1.0, "left edge at bar 8's open: {x1} vs {x8}");
                assert!(*x2 > rect.x + rect.w, "the far edge maps off-plot: {x2}");
                assert!(*y1 < *y2, "top above bottom in canvas y");
            }
            other => panic!("expected a box: {other:?}"),
        }
        match &objects[1] {
            ScriptDraw::Line { x1, x2, .. } => {
                assert!((x1 - x8).abs() < 1.0, "{x1} vs {x8}");
                assert!((x2 - x18).abs() < 1.0, "{x2} vs {x18}");
            }
            other => panic!("expected a line: {other:?}"),
        }
        match &objects[2] {
            ScriptDraw::Label { x, text, .. } => {
                assert!((x - x18).abs() < 1.0, "{x} vs {x18}");
                assert_eq!(text, "htf");
            }
            other => panic!("expected a label: {other:?}"),
        }
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

    // -- docs/24 S2: strategy positioning --------------------------------

    const STRAT_SCRIPT: &str = concat!(
        "//@pine_lite version=1 overlay=false title=\"Strat\" strategy(commission_value=0)\n",
        "up = ta.crossover(close, ta.sma(close, 5))\n",
        "down = ta.crossunder(close, ta.sma(close, 5))\n",
        "if up\n",
        "    strategy.entry(\"long\", direction=\"long\")\n",
        "if down\n",
        "    strategy.close_all()\n",
        "plot(close)\n",
    );

    fn strat_candles(n: usize) -> Vec<Candle> {
        (0..n).map(|i| candle(i, 100.0 + ((i % 7) as f64))).collect()
    }

    fn strat_frame() -> crate::scene::Frame {
        crate::scene::Frame {
            plot: plot_rect(),
            from: 0,
            to: 40 * 60_000_000_000,
            price_min: 99.0,
            price_max: 108.0,
            bar_nanos: 60_000_000_000,
        }
    }

    #[test]
    fn a_strategy_script_positions_fills_trades_and_report() {
        let (_, parsed) = pine_lite::vet(STRAT_SCRIPT).expect("vet");
        let output = run(&parsed, &strat_candles(40), &Inputs::default()).expect("run");
        let layer = scene_strategy_layer("Strat", &output, 10.0, &strat_frame()).expect("strategy layer");
        assert_eq!(layer.id, "script:Strat");
        assert!(!layer.trades.is_empty(), "the cycling fixture trades");
        for t in &layer.trades {
            for f in [&t.entry, &t.exit] {
                // Fill markers map through the price frame, like every plot.
                assert!(f.x >= plot_rect().x && f.x <= plot_rect().x + plot_rect().w, "{}", f.x);
                assert!(f.y >= plot_rect().y && f.y <= plot_rect().y + plot_rect().h, "{}", f.y);
            }
            // Next-open rule: the fill bar is the decision bar's successor.
            assert!(t.exit.bar > t.entry.bar, "{} > {}", t.exit.bar, t.entry.bar);
            assert!(t.entry.long, "the fixture only enters long");
        }
        // The trades' pnl must sum to the report's net profit -- the pairing
        // reconstructs exactly the round trips the sim counted.
        let summed: f64 = layer.trades.iter().map(|t| t.pnl).sum();
        assert!((summed - layer.report.net_profit).abs() < 1e-6, "{summed} vs {}", layer.report.net_profit);
        assert_eq!(layer.report.total_trades, layer.trades.len());
    }

    #[test]
    fn a_strategy_gets_a_dedicated_equity_pane() {
        let (_, parsed) = pine_lite::vet(STRAT_SCRIPT).expect("vet");
        let output = run(&parsed, &strat_candles(40), &Inputs::default()).expect("run");
        let pane = equity_pane("Strat", &output, 10.0, &plot_rect()).expect("equity pane");
        assert_eq!(pane.id, "equity:Strat");
        assert_eq!(pane.title, "Strat");
        // One point per bar of the window, all inside the pane's slice.
        assert_eq!(pane.points.len(), 40);
        for p in &pane.points {
            assert!(p.y >= plot_rect().y && p.y <= plot_rect().y + plot_rect().h, "{}", p.y);
        }
        // The curve brackets the initial capital (the sim starts there).
        assert!(pane.value_min <= 10_000.0 && pane.value_max >= 10_000.0);
    }

    #[test]
    fn an_open_position_becomes_the_dashed_box_seed() {
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"Held\" strategy(commission_value=0)\n",
            "if bar_index == 3\n",
            "    strategy.entry(\"long\", direction=\"long\")\n",
            "plot(close)\n",
        );
        let (_, parsed) = pine_lite::vet(src).expect("vet");
        let output = run(&parsed, &strat_candles(40), &Inputs::default()).expect("run");
        let layer = scene_strategy_layer("Held", &output, 10.0, &strat_frame()).expect("strategy layer");
        assert!(layer.trades.is_empty(), "never closed");
        let open = layer.open_position.expect("the position survives the window");
        assert!(open.long);
        assert!(open.y >= plot_rect().y && open.y <= plot_rect().y + plot_rect().h);
    }

    #[test]
    fn an_indicator_script_yields_no_strategy_layer_or_equity() {
        let candles: Vec<Candle> = (0..40).map(|i| candle(i, 100.0 + ((i % 7) as f64))).collect();
        let (_, parsed) = pine_lite::vet(RSI_SCRIPT).expect("vet");
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        assert!(scene_strategy_layer("RSI", &output, 10.0, &strat_frame()).is_none());
        assert!(equity_pane("RSI", &output, 10.0, &plot_rect()).is_none());
    }
}
