//! Drawable series, scaled in Rust.
//!
//! The same rule as [`crate::dom`], for the same reason: `docs/14` forbids
//! arithmetic over market data in JavaScript. A backtest report is not market
//! data, but the rule is about *who derives a number*, not about which numbers
//! -- and fitting a series into a box is arithmetic: a min, a max, and a
//! division per point. So it happens here and the shell is handed coordinates it
//! can put straight into an SVG. What is left for the shell is formatting, which
//! is what a shell is for.
//!
//! Coordinates are **percentages of the box**, `0..=100` on both axes, so the
//! panel can be any size and can be resized without recomputing anything. `y`
//! runs downward, the way SVG's does: the highest value sits at `y = 0`, the
//! top. `x` runs left to right in series order, and the first and last points
//! sit on the edges, so a line drawn through them fills the box exactly.
//!
//! The first consumer is an equity curve, but nothing here knows that: this
//! takes numbers and returns positions.

use serde::Serialize;

/// One point of a plotted series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlotPoint {
    /// Position across the box, left to right, `0..=100`.
    pub x: f64,
    /// Position down the box, `0..=100`. `0` is the **top**.
    pub y: f64,
    /// The value this point was plotted from.
    ///
    /// Carried so a label or a tooltip can name the number without reversing
    /// the scaling -- which would put the arithmetic back in the shell.
    pub value: f64,
}

/// A series fitted to a box.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plot {
    /// Smallest value in the series.
    pub min: f64,
    /// Largest value in the series.
    pub max: f64,
    /// Where a value of zero falls in the box, when it falls inside it at all.
    ///
    /// Drawn as a baseline. On an equity curve in R this is the water line --
    /// above it the run is up, below it the run is down -- and without it a
    /// mixed run is a line chart of an unknown quantity. `null` when zero is
    /// outside the box, because a baseline off the edge is not a baseline.
    ///
    /// It is a position rather than a number for the shell to place, for the
    /// same reason as everything else here: finding it is arithmetic.
    pub zero_y: Option<f64>,
    /// The points, in series order.
    pub points: Vec<PlotPoint>,
}

/// Fit a series into a box.
///
/// `None` when there is nothing to draw, and when a value is not finite: a
/// series with a hole in it has no honest picture, and a line that stops
/// halfway is worse than no line. Neither should happen -- the backtester's
/// numbers are all finite -- but a `NaN` here would reach the shell as a JSON
/// `null` and quietly break the path, which is worse than both.
///
/// A series that never moves (every value the same) is drawn down the middle
/// rather than along an edge: there is no high or low to put at either one.
#[must_use]
pub fn series(values: &[f64]) -> Option<Plot> {
    if values.is_empty() || !values.iter().all(|value| value.is_finite()) {
        return None;
    }

    let mut min = values[0];
    let mut max = values[0];
    for &value in values {
        min = min.min(value);
        max = max.max(value);
    }
    let span = max - min;

    // A single point has no distance to spread across, so it sits on the left
    // edge rather than dividing by zero.
    let last = (values.len() - 1) as f64;
    let points = values
        .iter()
        .enumerate()
        .map(|(index, &value)| PlotPoint {
            x: if last == 0.0 {
                0.0
            } else {
                index as f64 * 100.0 / last
            },
            y: position(value, span, max),
            value,
        })
        .collect();

    let zero_y = (min <= 0.0 && 0.0 <= max).then(|| position(0.0, span, max));

    Some(Plot {
        min,
        max,
        zero_y,
        points,
    })
}

/// Where `value` sits down the box, `0` at the top.
///
/// A series with no span has no high or low to pin to an edge, so it is drawn
/// down the middle.
fn position(value: f64, span: f64, max: f64) -> f64 {
    if span > 0.0 {
        (max - value) / span * 100.0
    } else {
        50.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn xs(plot: &Plot) -> Vec<f64> {
        plot.points.iter().map(|p| p.x).collect()
    }

    fn ys(plot: &Plot) -> Vec<f64> {
        plot.points.iter().map(|p| p.y).collect()
    }

    #[test]
    fn the_points_span_the_whole_box() {
        let plot = series(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("a real series");
        assert_eq!(xs(&plot), vec![0.0, 25.0, 50.0, 75.0, 100.0]);
    }

    #[test]
    fn the_highest_value_sits_at_the_top_and_the_lowest_at_the_bottom() {
        // y runs downward, so the biggest number is at y = 0. 5 is exactly
        // halfway between 2 and 8, so it lands halfway down.
        let plot = series(&[2.0, 8.0, 5.0]).expect("a real series");
        assert_eq!(plot.min, 2.0);
        assert_eq!(plot.max, 8.0);
        assert_eq!(ys(&plot), vec![100.0, 0.0, 50.0]);
    }

    #[test]
    fn a_series_that_never_moves_is_drawn_down_the_middle() {
        // There is no high or low to pin to an edge, so the flat line is
        // centred rather than drawn along the top.
        let plot = series(&[3.0, 3.0, 3.0]).expect("a real series");
        assert_eq!(ys(&plot), vec![50.0, 50.0, 50.0]);
    }

    #[test]
    fn a_negative_series_is_scaled_the_same_way() {
        // Losing runs go below the flat start; the scaling must not assume
        // that values are positive.
        let plot = series(&[0.0, -2.0, -1.0]).expect("a real series");
        assert_eq!(plot.min, -2.0);
        assert_eq!(plot.max, 0.0);
        assert_eq!(ys(&plot), vec![0.0, 100.0, 50.0]);
    }

    #[test]
    fn every_point_carries_the_value_it_was_plotted_from() {
        // So a label never has to undo the scaling.
        let values = [1.5, -0.5, 2.25];
        let plot = series(&values).expect("a real series");
        let carried: Vec<f64> = plot.points.iter().map(|p| p.value).collect();
        assert_eq!(carried, values.to_vec());
    }

    #[test]
    fn a_lone_point_sits_on_the_left_edge_rather_than_dividing_by_zero() {
        let plot = series(&[7.0]).expect("a real series");
        assert_eq!(xs(&plot), vec![0.0]);
        assert_eq!(ys(&plot), vec![50.0]);
    }

    #[test]
    fn there_is_no_plot_without_a_series() {
        assert!(series(&[]).is_none());
    }

    #[test]
    fn a_series_with_a_hole_in_it_is_not_a_plot() {
        // A NaN would reach the shell as a null and break the path.
        assert!(series(&[1.0, f64::NAN, 3.0]).is_none());
        assert!(series(&[1.0, f64::INFINITY, 3.0]).is_none());
    }

    #[test]
    fn the_water_line_sits_where_zero_falls() {
        // -1 .. 2, so zero is two thirds of the way down.
        let plot = series(&[0.0, 2.0, -1.0]).expect("a real series");
        let zero_y = plot.zero_y.expect("zero is inside the box");
        assert!((zero_y - 66.666_666_666_666_66).abs() < 1e-9, "{zero_y}");
        // And it is exactly where a point whose value is zero would be drawn.
        let at_zero = plot
            .points
            .iter()
            .find(|point| point.value == 0.0)
            .expect("the series starts at zero");
        assert!((at_zero.y - zero_y).abs() < 1e-9);
    }

    #[test]
    fn a_run_that_only_ever_wins_puts_the_water_line_on_the_bottom_edge() {
        // Every value is at or above zero, so the bottom edge *is* flat.
        let plot = series(&[0.0, 1.0, 3.0]).expect("a real series");
        assert_eq!(plot.zero_y, Some(100.0));
    }

    #[test]
    fn a_run_that_only_ever_loses_puts_the_water_line_on_the_top_edge() {
        let plot = series(&[0.0, -2.0, -1.0]).expect("a real series");
        assert_eq!(plot.zero_y, Some(0.0));
    }

    #[test]
    fn there_is_no_water_line_when_zero_is_outside_the_box() {
        // A baseline off the edge is not a baseline.
        let plot = series(&[1.0, 4.0, 2.0]).expect("a real series");
        assert_eq!(plot.zero_y, None);
    }

    #[test]
    fn a_flat_series_at_zero_keeps_its_water_line_in_the_middle() {
        // Where the flat line is drawn, which is the only place it could be.
        let plot = series(&[0.0, 0.0]).expect("a real series");
        assert_eq!(plot.zero_y, Some(50.0));
    }

    #[test]
    fn the_json_keys_are_the_ones_the_shell_indexes_by() {
        // `curveSvg` in `app.js` reads these by name. A rename here is not a
        // compile error anywhere -- it is a panel that draws nothing -- so the
        // wire shape is pinned rather than assumed, the same reason
        // `wasm_abi_check.mjs` exists for the engine's exports.
        let json = serde_json::to_value(series(&[0.0, 2.0]).expect("a real series"))
            .expect("a plot serialises");

        let keys: BTreeSet<&str> = json
            .as_object()
            .expect("a plot is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(["min", "max", "zero_y", "points"]));

        let point: BTreeSet<&str> = json["points"][0]
            .as_object()
            .expect("a point is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(point, BTreeSet::from(["x", "y", "value"]));
    }
}
