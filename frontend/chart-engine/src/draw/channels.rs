//! The channels group: two parallel edges, and the band the trader claims price
//! holds between them.
//!
//! The three tools disagree about how far their edges run, and the difference is
//! what each one is *for*:
//!
//! - [`parallel`] spans the whole plot. Its third click only sets an offset, so the
//!   band has no extent of its own to stop at -- a claim about a pair of price
//!   *levels* is a claim about the visible window, the same reason
//!   [`super::lines::horizontal`] runs a level across the plot.
//! - [`disjoint`] stops each edge at its own two clicks. Four points, two of which
//!   are the ends of the first edge and two the ends of the second: the extent the
//!   user dragged *is* the claim, which is the whole point of the tool -- a band
//!   that starts and ends apart, for a slope that has not yet been tested.
//! - [`flat_top_bottom`] spans the angled edge's own drag and takes the horizontal
//!   with it, because the shape is bounded by the move it was drawn over.
//!
//! [`Extend`] overrides the span on the latter two -- TradingView's *Extend
//! Left/Right* -- with each kind's own default as the fallback.
//!
//! Width and slope are measured in **pixels**, and that is deliberate: the user
//! asked for two lines that look parallel on this window. A band that is parallel
//! in price space while visibly converging on screen would be a band about the
//! vertical scale rather than about the chart.

use crate::drawing::{DrawingPart, Extend};

use super::{pixel_slope, Geometry, Point};

/// The line through the first two points and its parallel through the third.
#[must_use]
pub fn parallel(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let plot = geometry.frame.plot;
    let first = (geometry.x(0), geometry.y(0));
    let second = (geometry.x(1), geometry.y(1));
    let third = (geometry.x(2), geometry.y(2));
    let left = plot.x;
    let right = plot.x + plot.w;
    let slope = pixel_slope(first, second);
    // The second edge is the parallel *through* the third click: a point the
    // trader placed on the chart belongs on the line it defines, which means the
    // width is the gap at the third click's own time and not the gap between its
    // price and the mid-point of the first two. Off-centre, the two differ, and
    // only one of them puts the edge where the user clicked.
    let offset = third.1 - (first.1 + slope * (third.0 - first.0));
    let mid = |x: f64| first.1 + slope * (x - first.0);
    vec![
        DrawingPart::Segment {
            x1: left,
            y1: mid(left),
            x2: right,
            y2: mid(right),
            dashed: false,
        },
        DrawingPart::Segment {
            x1: left,
            y1: mid(left) + offset,
            x2: right,
            y2: mid(right) + offset,
            dashed: false,
        },
    ]
}

/// Two parallel edges that begin and end apart, and the band between them.
///
/// The first edge is the line the user dragged. The second is forced parallel to it
/// through the third click; the fourth click sets how far that edge runs and its
/// price is ignored -- the hand was pointing at a *time*, and the offset already
/// committed to on the third click is the one that stays.
#[must_use]
pub fn disjoint(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 4 {
        return Vec::new();
    }
    let first = (geometry.x(0), geometry.y(0));
    let second = (geometry.x(1), geometry.y(1));
    let third = (geometry.x(2), geometry.y(2));
    let fourth_x = geometry.x(3);
    let slope = pixel_slope(first, second);

    let edges = [
        stroked_edge(geometry, slope, first, first.0, second.0),
        stroked_edge(geometry, slope, third, third.0, fourth_x),
    ];

    let mut parts = Vec::with_capacity(3);
    // The fill first: the shell paints in order, so the two edges stay on top of
    // their own band instead of being half-buried in it.
    parts.push(DrawingPart::Polygon {
        points: vec![
            (edges[0].0, edges[0].1),
            (edges[0].2, edges[0].3),
            (edges[1].2, edges[1].3),
            (edges[1].0, edges[1].1),
        ],
        filled: true,
    });
    for edge in edges {
        parts.push(segment(edge));
    }
    parts
}

/// An angled edge and a horizontal one, and the wedge between them.
///
/// The first two clicks are the trend line, the third is the boundary price. The
/// horizontal runs the angled edge's own x-span rather than the whole plot: this
/// tool draws *one side* of a move that is flattening out, and a level spanning the
/// window would be the [`super::lines::horizontal`] the user already has.
#[must_use]
pub fn flat_top_bottom(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let first = (geometry.x(0), geometry.y(0));
    let second = (geometry.x(1), geometry.y(1));
    let flat_y = geometry.y(2);
    let slope = pixel_slope(first, second);
    let angled = stroked_edge(geometry, slope, first, first.0, second.0);
    // The flat edge shares the angled one's span rather than re-deriving it: the
    // wedge is the region the drag covered, and extend has already been applied.
    let flat = (angled.0, flat_y, angled.2, flat_y);

    vec![
        DrawingPart::Polygon {
            points: vec![
                (angled.0, angled.1),
                (angled.2, angled.3),
                (flat.2, flat.3),
                (flat.0, flat.1),
            ],
            filled: true,
        },
        segment(angled),
        segment(flat),
    ]
}

/// An edge as `(x, y, x, y)`, from `from` to `to` on the line through `at`.
///
/// `from` and `to` are the anchors' own positions; [`stretched`] has already
/// widened them if the user asked the band to run past them.
#[must_use]
fn stroked_edge(
    geometry: Geometry<'_>,
    slope: f64,
    at: Point,
    from: f64,
    to: f64,
) -> (f64, f64, f64, f64) {
    let (left, right) = stretched(geometry, Extend::None, from.min(to), from.max(to));
    (
        left,
        at.1 + slope * (left - at.0),
        right,
        at.1 + slope * (right - at.0),
    )
}

/// The same edge, as a stroke.
#[must_use]
fn segment(edge: (f64, f64, f64, f64)) -> DrawingPart {
    DrawingPart::Segment {
        x1: edge.0,
        y1: edge.1,
        x2: edge.2,
        y2: edge.3,
        dashed: false,
    }
}

/// How far an edge runs, given the kind's own default and what the user asked.
///
/// Same rule as [`super::lines::stroked`]: `Left` reaches the plot's left edge and
/// stops at the shape, `Right` stops at the shape and reaches the right edge,
/// `Both` is the infinite band, and `None` is the anchors.
#[must_use]
fn stretched(geometry: Geometry<'_>, default: Extend, from: f64, to: f64) -> (f64, f64) {
    let plot = geometry.frame.plot;
    match geometry.options.extend.unwrap_or(default) {
        Extend::None => (from, to),
        Extend::Left => (plot.x, to),
        Extend::Right => (from, plot.x + plot.w),
        Extend::Both => (plot.x, plot.x + plot.w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::DrawingOptions;
    use crate::draw::Stage;

    fn segments(
        geometry: Geometry<'_>,
        kind: fn(Geometry<'_>) -> Vec<DrawingPart>,
    ) -> Vec<(f64, f64, f64, f64)> {
        kind(geometry)
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

    fn corners(
        geometry: Geometry<'_>,
        kind: fn(Geometry<'_>) -> Vec<DrawingPart>,
    ) -> Vec<(f64, f64)> {
        kind(geometry)
            .into_iter()
            .find_map(|part| match part {
                DrawingPart::Polygon { points, .. } => Some(points),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// A 10-bar stage over 100..300, so bar *i* is at pixel `i * 100` and price
    /// `p` is at pixel `2.5 * (300 - p)` -- every fixture below is round numbers.
    fn stage() -> Stage {
        Stage::flat(10, 200.0)
    }

    /// An edge from (100, 220) to (500, 180) -- pixels (100, 200) to (500, 300),
    /// slope +0.25 -- with its parallel through (200, 140) = pixel (200, 400) and
    /// its second edge ending at bar 4, so the two spans do not line up.
    fn disjoint_points() -> Vec<Point> {
        let stage = stage();
        vec![
            (stage.ms(1), 220.0),
            (stage.ms(5), 180.0),
            (stage.ms(2), 140.0),
            // Off-scale on purpose: the fourth click's price must not be read, and
            // a plausible number would let a wrong implementation look right.
            (stage.ms(4), 999.0),
        ]
    }

    /// The same edge, with the boundary a third click below it and *off-centre* --
    /// bar 4 of a drag from bar 1 to bar 5, so a width measured at the mid-point
    /// instead of at the click itself is caught.
    fn wedge_points() -> Vec<Point> {
        let stage = stage();
        vec![
            (stage.ms(1), 220.0),
            (stage.ms(5), 180.0),
            (stage.ms(4), 140.0),
        ]
    }

    /// Where a stroke runs at one x.
    fn at(line: (f64, f64, f64, f64), x: f64) -> f64 {
        line.1 + (line.3 - line.1) / (line.2 - line.0) * (x - line.0)
    }

    #[test]
    fn a_disjoint_channels_second_edge_is_parallel_but_stops_short() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = disjoint_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, disjoint);
        assert_eq!(drawn.len(), 2, "two edges");

        let first_slope = (drawn[0].3 - drawn[0].1) / (drawn[0].2 - drawn[0].0);
        let second_slope = (drawn[1].3 - drawn[1].1) / (drawn[1].2 - drawn[1].0);
        assert!(
            (first_slope - second_slope).abs() < 1e-9,
            "the second edge is parallel: {first_slope} vs {second_slope}"
        );
        assert_eq!((drawn[0].0, drawn[0].2), (100.0, 500.0));
        assert_eq!(
            (drawn[1].0, drawn[1].2),
            (200.0, 400.0),
            "the two edges start and end apart, and the fourth click sets a *time*"
        );
    }

    #[test]
    fn a_disjoint_channels_band_is_one_width_everywhere_it_overlaps() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = disjoint_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, disjoint);
        // Pixel arithmetic the fixture can name: edge 1 is y = 200 + 0.25(x-100) and
        // edge 2 is y = 400 + 0.25(x-200), so the gap is 175 -- the third click's
        // offset, and never the fourth click's price.
        for x in [150.0, 250.0, 380.0] {
            let gap = at(drawn[1], x) - at(drawn[0], x);
            assert!(
                (gap - 175.0).abs() < 1e-9,
                "the band is one width everywhere, not one per click: {gap} at x={x}"
            );
        }
    }

    #[test]
    fn a_disjoint_channel_fills_between_its_two_edges() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = disjoint_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, disjoint);
        assert_eq!(
            corners(geometry, disjoint),
            vec![
                (drawn[0].0, drawn[0].1),
                (drawn[0].2, drawn[0].3),
                (drawn[1].2, drawn[1].3),
                (drawn[1].0, drawn[1].1),
            ],
            "one quad tracing both edges"
        );
    }

    #[test]
    fn a_disjoint_channel_told_to_extend_reaches_the_plot_edges() {
        let options = DrawingOptions {
            extend: Some(Extend::Both),
            ..DrawingOptions::default()
        };
        let stage = stage();
        let points = disjoint_points();
        let geometry = stage.geometry(&points, &options);
        for edge in segments(geometry, disjoint) {
            assert_eq!((edge.0, edge.2), (0.0, 1_000.0));
        }
    }

    #[test]
    fn a_flat_top_bottom_runs_the_angled_edges_span_at_the_third_clicks_price() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = wedge_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, flat_top_bottom);

        assert_eq!(drawn.len(), 2, "one angled edge and one flat one");
        assert_ne!(drawn[0].1, drawn[0].3, "the first edge is the trend line");
        assert_eq!(drawn[1].1, drawn[1].3, "the second is horizontal");
        for edge in &drawn {
            assert_eq!(
                (edge.0, edge.2),
                (100.0, 500.0),
                "both stop where the angled drag stopped"
            );
        }
        assert_eq!(drawn[1].1, geometry.y(2), "at the third click's price");
    }

    #[test]
    fn a_flat_top_bottom_fills_the_wedge_its_three_points_define() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = wedge_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, flat_top_bottom);
        assert_eq!(
            corners(geometry, flat_top_bottom),
            vec![
                (drawn[0].0, drawn[0].1),
                (drawn[0].2, drawn[0].3),
                (drawn[1].2, drawn[1].3),
                (drawn[1].0, drawn[1].1),
            ],
            "the four corners are the two edges' own ends"
        );
    }

    #[test]
    fn a_channel_dragged_right_to_left_is_the_same_channel() {
        let options = DrawingOptions::default();
        let stage = stage();
        let forward = wedge_points();
        let mut backward = forward.clone();
        backward.swap(0, 1);

        let drawn = segments(stage.geometry(&forward, &options), parallel);
        let mirrored = segments(stage.geometry(&backward, &options), parallel);
        assert!(
            (drawn[0].3 - drawn[0].1) > 0.0,
            "price falling left-to-right is a line falling on screen"
        );
        assert_eq!(drawn, mirrored, "the drag's direction cannot change the band");
    }

    #[test]
    fn a_channel_spans_the_plot_and_takes_its_width_from_where_the_third_click_is() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = wedge_points();
        let geometry = stage.geometry(&points, &options);
        let drawn = segments(geometry, parallel);
        for edge in &drawn {
            assert_eq!(
                (edge.0, edge.2),
                (0.0, 1_000.0),
                "a band is a claim about the window"
            );
        }
        assert!(
            (at(drawn[1], geometry.x(2)) - geometry.y(2)).abs() < 1e-9,
            "the second edge runs through the third click, not beside it"
        );
        // The fixture's own arithmetic: 125 pixels of gap at bar 4, where the click
        // landed -- a width read off the first two clicks would be 150.
        assert!(
            (drawn[1].1 - drawn[0].1 - 125.0).abs() < 1e-9,
            "the band is one parallel shift of that: {} vs {}",
            drawn[0].1,
            drawn[1].1
        );
    }

    #[test]
    fn three_points_draw_no_disjoint_and_two_no_band() {
        let options = DrawingOptions::default();
        let stage = stage();
        let points = wedge_points();
        assert!(disjoint(stage.geometry(&points, &options)).is_empty());
        assert!(flat_top_bottom(stage.geometry(&points[..2], &options)).is_empty());
        assert!(parallel(stage.geometry(&points[..2], &options)).is_empty());
    }
}
