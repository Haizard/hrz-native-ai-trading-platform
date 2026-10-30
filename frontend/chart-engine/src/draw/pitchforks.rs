//! The pitchfork group: a median line and two tines parallel to it.
//!
//! All four templates are the same drawing -- a handle, a median, two parallels
//! -- and differ *only* in which three points the median is drawn between. So
//! they are one function taking a [`Template`], not four functions that each
//! re-derive a parallel: four copies is four chances for one fork to be quietly
//! drawn as another, and the tests below pin which templates must produce a
//! different median and which produce the same line by construction.
//!
//! ## Where the midpoints are computed
//!
//! A pivot's midpoint is a statement about **price and time**, so it is taken in
//! the points' own space and mapped to pixels afterwards. On a log-scale window
//! the two disagree, and the trader means the price: "halfway between these two
//! pivots" that drifted when the vertical scale changed would be a fork about the
//! chart's zoom rather than about the market. The parallels themselves are drawn
//! in pixel space -- the user asked for three lines that *look* parallel, which is
//! the same reason [`super::channels`] offsets its band in pixels.

use crate::drawing::{DrawingPart, Extend};

use super::{pixel_slope, Geometry, Point};

/// Which three points the median is drawn between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// Andrews': the handle is the first pivot and the tines pass through the
    /// second and third, so the median runs to the midpoint of the last two --
    /// the construction every reference calls the original.
    Andrews,
    /// Schiff: the handle's **price** is the midpoint of the first two pivots
    /// while its time stays the first pivot's. The fork tilts toward the second
    /// pivot without leaving the handle's bar.
    Schiff,
    /// Modified Schiff: the handle is the midpoint of the first two pivots in
    /// price **and** time, so the fork starts between the pivots instead of at
    /// the first one.
    ModifiedSchiff,
    /// Inside: the tines pass through the midpoints of handle-to-second and
    /// handle-to-third, which shrinks the plain fork toward its handle by half --
    /// the same median line, a narrower band. Used when the outer fork is too wide
    /// for the move.
    Inside,
}

/// The median and the two tines, as three lines of one length.
///
/// Every line spans the same x-range so the shape reads as one fork rather than as
/// three unrelated strokes, and that range is the three pivots: a tine is always
/// drawn through its own swing, so a span that stopped short of the furthest pivot
/// would cut a line under the point it was placed on. The default runs from the
/// earliest pivot forward to the plot's right edge, which is how the references
/// draw it, and `extend` moves that span the way [`super::lines::horizontal_ray`]
/// moves a level's.
#[must_use]
pub fn fork(geometry: Geometry<'_>, template: Template) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let (first, second, third) = (
        geometry.points[0],
        geometry.points[1],
        geometry.points[2],
    );
    let (handle, first_tine, second_tine) = match template {
        Template::Andrews => (first, second, third),
        Template::Schiff => ((first.0, midpoint_price(first, second)), second, third),
        Template::ModifiedSchiff => (midpoint(first, second), second, third),
        Template::Inside => (first, midpoint(first, second), midpoint(first, third)),
    };

    let plot = geometry.frame.plot;
    let origin = pixel(geometry, handle);
    let tine_one = pixel(geometry, first_tine);
    let tine_two = pixel(geometry, second_tine);
    // The median aims at the midpoint of the two tines' pivots, which is what makes
    // the band symmetric about it by construction.
    let target = pixel(geometry, midpoint(first_tine, second_tine));
    let slope = pixel_slope(origin, target);

    let extent = geometry.options.extend.unwrap_or(Extend::Right);
    // The shape's own span is the three pivots, not the point the median aims at. A
    // band stopped there would leave the further tine's pivot outside its own line,
    // and a tine that does not reach the swing it was drawn from is a different
    // claim than the one the trader placed.
    let from = origin.0.min(tine_one.0).min(tine_two.0);
    let to = origin.0.max(tine_one.0).max(tine_two.0);
    let (left, right) = match extent {
        Extend::None => (from, to),
        Extend::Left => (plot.x, to),
        Extend::Right => (from, plot.x + plot.w),
        Extend::Both => (plot.x, plot.x + plot.w),
    };

    // The median first: it is the line the trader reads, the shell strokes in
    // order, and where the three overlap the median should be the one on top.
    let mut parts = Vec::with_capacity(3);
    parts.push(stretched(slope, left, right, origin));
    for tine in [tine_one, tine_two] {
        parts.push(stretched(slope, left, right, tine));
    }
    parts
}

/// The line through `point` at `slope`, drawn between the two x positions.
#[must_use]
fn stretched(slope: f64, left: f64, right: f64, point: (f64, f64)) -> DrawingPart {
    DrawingPart::Segment {
        x1: left,
        y1: point.1 + slope * (left - point.0),
        x2: right,
        y2: point.1 + slope * (right - point.0),
        dashed: false,
    }
}

/// The price halfway between two pivots.
#[must_use]
fn midpoint_price(a: Point, b: Point) -> f64 {
    (a.1 + b.1) / 2.0
}

/// The pivot halfway between two pivots, in time and price.
#[must_use]
fn midpoint(a: Point, b: Point) -> Point {
    ((a.0 + b.0) / 2.0, midpoint_price(a, b))
}

/// A point's pixel position.
#[must_use]
fn pixel(geometry: Geometry<'_>, point: Point) -> (f64, f64) {
    (
        geometry.frame.x_at_ms(point.0),
        geometry.frame.y_at(point.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::DrawingOptions;
    use crate::draw::Stage;

    /// A fork's three strokes, median first.
    fn lines(geometry: Geometry<'_>, template: Template) -> Vec<(f64, f64, f64, f64)> {
        fork(geometry, template)
            .into_iter()
            .filter_map(|part| match part {
                DrawingPart::Segment {
                    x1,
                    y1,
                    x2,
                    y2,
                    ..
                } => Some((x1, y1, x2, y2)),
                _ => None,
            })
            .collect()
    }

    /// Where a stroke runs at one x -- the only way to compare two strokes of
    /// different length as the *lines* they are.
    fn y_at(line: (f64, f64, f64, f64), x: f64) -> f64 {
        line.1 + (line.3 - line.1) / (line.2 - line.0) * (x - line.0)
    }

    /// Three pivots over a 100..300 window on a 1000x500 plot: the handle low and
    /// left, the second pivot high, the third to the right and between them.
    fn triangle() -> (Stage, Vec<Point>) {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let points = vec![
            (stage.ms(1), 120.0),
            (stage.ms(8), 260.0),
            (stage.ms(15), 180.0),
        ];
        (stage, points)
    }

    #[test]
    fn the_templates_that_differ_by_their_median_actually_do() {
        let (stage, points) = triangle();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let handle_x = geometry.x(0);
        let right = 1_000.0;

        let median = |template: Template| lines(geometry, template)[0];
        let andrews = median(Template::Andrews);
        let schiff = median(Template::Schiff);
        let modified = median(Template::ModifiedSchiff);

        // Three different lines: no template can be drawn as another.
        for (a, b) in [(andrews, schiff), (andrews, modified), (schiff, modified)] {
            assert_ne!(y_at(a, right), y_at(b, right), "two templates share a median");
        }

        // Andrews starts at the handle; Schiff has been lifted halfway toward the
        // second pivot in price, at the handle's own bar.
        assert_eq!(y_at(andrews, handle_x), geometry.y(0));
        let lifted = y_at(schiff, handle_x);
        // The canvas runs downward, so a handle lifted in *price* is smaller in y.
        assert!(lifted < geometry.y(0) && lifted > geometry.y(1), "{lifted}");
        // Modified is the midpoint of the first two pivots in *both* units, so its
        // median passes through that pivot -- the fact that distinguishes it here.
        let shifted = midpoint(points[0], points[1]);
        assert!(
            (y_at(modified, geometry.frame.x_at_ms(shifted.0)) - geometry.frame.y_at(shifted.1)).abs()
                < 1e-9,
            "the Modified Schiff median runs from the midpoint of the first two pivots"
        );
    }

    #[test]
    fn the_inside_fork_shrinks_the_plain_one_toward_its_handle() {
        let (stage, points) = triangle();
        let options = DrawingOptions {
            extend: Some(Extend::None),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);

        let andrews = lines(geometry, Template::Andrews);
        let inside = lines(geometry, Template::Inside);
        // Halving the fork about the handle keeps the median on the same infinite
        // line -- the geometry, not a coincidence to be tested around.
        for x in [0.0, 400.0, 1_000.0] {
            assert!(
                (y_at(andrews[0], x) - y_at(inside[0], x)).abs() < 1e-9,
                "the inside median is the same line at x={x}"
            );
        }
        // and stops half as far along it, because its own tines are halfway in.
        assert!(
            inside[0].2 < andrews[0].2,
            "the inside median aims at the midpoint of its own tines"
        );
    }

    #[test]
    fn the_median_passes_through_its_handle_and_the_tines_midpoint() {
        let (stage, points) = triangle();
        let options = DrawingOptions::default();
        let median = lines(stage.geometry(&points, &options), Template::Andrews)[0];
        let mid_time = (points[1].0 + points[2].0) / 2.0;
        let mid_price = (points[1].1 + points[2].1) / 2.0;

        assert!(
            (y_at(median, stage.frame.x_at_ms(points[0].0)) - stage.frame.y_at(points[0].1)).abs() < 1e-9,
            "the median runs through the handle"
        );
        assert!(
            (y_at(median, stage.frame.x_at_ms(mid_time)) - stage.frame.y_at(mid_price)).abs() < 1e-9
        );
    }

    #[test]
    fn a_fork_whose_handle_is_the_latest_bar_is_not_mirrored() {
        let (stage, mut points) = triangle();
        // The handle placed *after* both tines: the median's run is negative, and a
        // clamped denominator would draw it tilting the other way.
        points[0].0 = stage.ms(19);
        let options = DrawingOptions {
            extend: Some(Extend::Both),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);
        let median = lines(geometry, Template::Andrews)[0];
        let mid_time = (points[1].0 + points[2].0) / 2.0;

        assert!(
            (y_at(median, stage.frame.x_at_ms(mid_time)) - stage.frame.y_at((points[1].1 + points[2].1) / 2.0)).abs()
                < 1e-9,
            "the median still aims at the tines' midpoint"
        );
        assert!(
            (y_at(median, stage.frame.x_at_ms(points[0].0)) - stage.frame.y_at(points[0].1)).abs() < 1e-9
        );
    }

    #[test]
    fn the_tines_are_parallel_to_the_median_and_pass_through_their_pivots() {
        let (stage, points) = triangle();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let drawn = lines(geometry, Template::Andrews);
        let median = drawn[0];
        let median_slope = (median.3 - median.1) / (median.2 - median.0);

        for (index, tine) in drawn[1..].iter().enumerate() {
            let tine_slope = (tine.3 - tine.1) / (tine.2 - tine.0);
            assert!(
                (tine_slope - median_slope).abs() < 1e-9,
                "the tine is parallel to the median"
            );
            assert!(
                (y_at(*tine, geometry.x(index + 1)) - geometry.y(index + 1)).abs() < 1e-9,
                "the tine passes through its own pivot"
            );
        }
    }

    #[test]
    fn the_inside_forks_tines_sit_halfway_between_the_median_and_the_outer_ones() {
        let (stage, points) = triangle();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let andrews = lines(geometry, Template::Andrews);
        let inside = lines(geometry, Template::Inside);
        let x = geometry.x(1);

        for (outer, inner) in andrews[1..].iter().zip(inside[1..].iter()) {
            let half = (y_at(*outer, x) + y_at(andrews[0], x)) / 2.0;
            assert!(
                (y_at(*inner, x) - half).abs() < 1e-6,
                "the inside tine is halfway in"
            );
        }
    }

    #[test]
    fn a_fork_told_to_extend_left_starts_at_the_plot_edge_and_stops_at_its_pivots() {
        let (stage, points) = triangle();
        let options = DrawingOptions {
            extend: Some(Extend::Left),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);
        let furthest = geometry.x(2);
        for line in lines(geometry, Template::Andrews) {
            assert_eq!(line.0, 0.0, "extend left reaches the plot's left edge");
            assert_eq!(line.2, furthest, "and stops at the last pivot, not the handle");
        }
    }

    #[test]
    fn a_fork_stopped_at_its_points_reaches_every_pivot() {
        let (stage, points) = triangle();
        let options = DrawingOptions {
            extend: Some(Extend::None),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);
        let drawn = lines(geometry, Template::Andrews);

        for line in &drawn {
            assert_eq!(
                line.0,
                geometry.x(0).min(geometry.x(1)).min(geometry.x(2)),
                "the band starts at its earliest pivot"
            );
            assert_eq!(
                line.2,
                geometry.x(0).max(geometry.x(1)).max(geometry.x(2)),
                "and ends at its latest, so the far tine is drawn through its own swing"
            );
        }
        assert!(drawn[0].2 < 1_000.0, "and never at the plot edge");
    }

    #[test]
    fn two_points_draw_no_fork() {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let options = DrawingOptions::default();
        let points = vec![(stage.ms(1), 120.0), (stage.ms(8), 260.0)];
        assert!(fork(stage.geometry(&points, &options), Template::Andrews).is_empty());
    }
}
