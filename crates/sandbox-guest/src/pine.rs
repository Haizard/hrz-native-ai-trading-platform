//! Bounded-data execution for Pine-Lite preview scripts.

use analytics_core::types::Candle;
use pine_lite::interp::{Inputs, ScriptObject};
use serde::{Deserialize, Serialize};

/// One isolated script execution request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Pine-Lite source.
    pub source: String,
    /// Host-fetched chart candles.
    pub candles: Vec<Candle>,
    /// Host-fetched inputs and aligned provider series.
    #[serde(default)]
    pub inputs: Inputs,
}

/// A finite range produced by one plot call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotRange {
    /// Stable source-order identifier.
    pub id: String,
    /// Plot title.
    pub title: String,
    /// Smallest finite value.
    pub low: f64,
    /// Largest finite value.
    pub high: f64,
}

/// One chart marker produced by `plotshape`, `plotchar`, or `plotarrow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapePoint {
    /// Bar index in the submitted candle window.
    pub bar: usize,
    /// Marker value.
    pub value: f64,
    /// Marker glyph.
    pub glyph: String,
}

/// Small strategy summary used by the preview card.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SimulationSummary {
    /// Closed trades.
    pub trades: usize,
    /// Net profit.
    pub net_profit: f64,
    /// Maximum drawdown.
    pub max_drawdown: f64,
}

/// Bounded preview evidence returned across the Wasmtime boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    /// Number of plot call sites represented by the VM output.
    pub plot_count: usize,
    /// Finite value ranges, not the VM's full per-bar buffers.
    pub plots: Vec<PlotRange>,
    /// Per-bar markers.
    pub shapes: Vec<ShapePoint>,
    /// Number of horizontal levels.
    pub levels: usize,
    /// Total drawing primitives.
    pub drawing_objects: usize,
    /// Number of active boxes.
    pub boxes: usize,
    /// Number of active labels.
    pub labels: usize,
    /// Number of fired alerts.
    pub alerts: usize,
    /// Whether the VM truncated its object heap.
    pub objects_truncated: bool,
    /// Strategy results, when the script is a strategy.
    pub simulation: Option<SimulationSummary>,
    /// Non-fatal runtime notes.
    pub notes: Vec<String>,
}

/// Vet and execute a script, then reduce its output to preview evidence.
pub fn execute(request: Request) -> Result<Preview, String> {
    let (_, parsed) = pine_lite::vet(&request.source).map_err(|errors| {
        errors
            .iter()
            .map(|error| {
                format!(
                    "line {} col {} [{} {}]: {}",
                    error.span.line,
                    error.span.col,
                    error.diagnostic_code().as_str(),
                    pine_lite::kind_name(error.kind),
                    error.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let output = pine_lite::run(&parsed, &request.candles, &request.inputs)
        .map_err(|error| error.to_string())?;

    let plots = output
        .plots
        .iter()
        .filter_map(|plot| {
            let mut values = plot
                .values
                .iter()
                .copied()
                .filter(|value| value.is_finite());
            let first = values.next()?;
            let (low, high) = values.fold((first, first), |(low, high), value| {
                (low.min(value), high.max(value))
            });
            Some(PlotRange {
                id: plot.id.clone(),
                title: plot.title.clone(),
                low,
                high,
            })
        })
        .collect();
    let shapes = output
        .shapes
        .iter()
        .map(|shape| ShapePoint {
            bar: shape.bar,
            value: shape.value,
            glyph: shape.glyph.clone(),
        })
        .collect();
    let boxes = output
        .objects
        .iter()
        .filter(|object| {
            matches!(
                object,
                ScriptObject::Box { .. } | ScriptObject::BoxTime { .. }
            )
        })
        .count();
    let labels = output
        .objects
        .iter()
        .filter(|object| {
            matches!(
                object,
                ScriptObject::Label { .. } | ScriptObject::LabelTime { .. }
            )
        })
        .count();
    let simulation = output
        .simulation
        .as_ref()
        .map(|simulation| SimulationSummary {
            trades: simulation.report.total_trades.max(0.0) as usize,
            net_profit: simulation.report.net_profit,
            max_drawdown: simulation.report.max_drawdown,
        });

    Ok(Preview {
        plot_count: output.plots.len(),
        plots,
        shapes,
        levels: output.hlines.len(),
        drawing_objects: output.objects.len()
            + output.polylines.len()
            + output.linefills.len()
            + output.tables.len(),
        boxes,
        labels,
        alerts: output.alerts.len(),
        objects_truncated: output.objects_truncated,
        simulation,
        notes: output.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::Timeframe;

    fn candle(index: usize, close: f64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::M1,
            open_time: index as i64 * Timeframe::M1.nanos(),
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 100.0,
            buy_volume: 50.0,
            sell_volume: 50.0,
        }
    }

    #[test]
    fn executes_and_reduces_a_reference_indicator() {
        let preview = execute(Request {
            source: concat!(
                "//@pine_lite version=1 overlay=false title=\"Fixture\"\n",
                "avg = ta.sma(close, 2)\n",
                "plot(avg, title=\"Average\")\n",
                "plotshape(close > 2, shape=\"circle\", location_value=high)\n",
            )
            .into(),
            candles: vec![candle(0, 1.0), candle(1, 2.0), candle(2, 3.0)],
            inputs: Inputs::default(),
        })
        .expect("fixture should execute");

        assert_eq!(preview.plots.len(), 1);
        assert_eq!(preview.plots[0].title, "Average");
        assert_eq!(preview.plots[0].low, 1.5);
        assert_eq!(preview.plots[0].high, 2.5);
        assert_eq!(preview.shapes.len(), 1);
        assert_eq!(preview.shapes[0].bar, 2);
    }

    #[test]
    fn refuses_invalid_source_instead_of_returning_an_empty_preview() {
        let error = execute(Request {
            source: "//@pine_lite version=1\nplot(unknown_name)\n".into(),
            candles: vec![candle(0, 1.0)],
            inputs: Inputs::default(),
        })
        .expect_err("unknown variables must be rejected");
        assert!(error.contains("unknown_name"), "{error}");
    }
}
