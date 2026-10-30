//! The Gann group: price and time held in one proportion.
//!
//! Every tool here makes the same claim, and it is the claim TradingView's four
//! Gann pages keep repeating: *"a proper scale ratio, so that a unit of price
//! would correspond a unit of time"*. A 1×1 line is only meaningful once both
//! axes are in the same unit, and that unit is the one thing this module
//! computes — from the drawing's own two points, because they are the only
//! declaration of scale the user made.
//!
//! ## The scale is the drag
//!
//! The second point states the ratio and nothing else does: the drag's price
//! delta per bar *is* the 1×1. That is why [`Scale`] is derived once, in
//! [`scale`], and every tool reads it — a fan whose 1×1 misses the point the
//! user dragged is a fan about a different chart. A drag with no time span or no
//! price span has no ratio to state, so every builder here refuses it rather
//! than dividing by zero into a shape with `NaN` in it.
//!
//! ## Eighths, and why
//!
//! [`crate::drawing`]'s Gann box is divided into eighths on both axes. Gann's
//! work divides a full turn by eight, and the box is the chart's version of that
//! wheel: the 45° diagonal is 1/8 of the turn, the square is 2/8. The divisions
//! are dashed and the box's own edges are not, so the grid reads as the interior
//! of one idea rather than as nine boxes.
//!
//! ## What the options do here
//!
//! [`crate::drawing::DrawingOptions::reverse`] reflects the angles: in the box it
//! swaps which diagonal is the solid one, in the fixed square it walks the levels
//! the other way from the anchor. [`crate::drawing::Extend`] moves the fan's rays'
//! ends; the box, the square and the fixed square ignore it, because their
//! extent *is* their anchors.

use crate::drawing::{DrawingPart, Extend};

use super::{ray_to, Geometry};

/// The nine Gann angles, as `(price units, bar units)`, shallowest first.
///
/// TradingView's fan is this set: 1×1 and its quarters one way, its doubles the
/// other, out to 16. The list is ordered not by steepness of the drawn line but
/// by the ratio, so a test can assert nine distinct slopes and a reader can see
/// the symmetry — the same nine, mirrored about the 1×1.
const ANGLES: [(u32, u32); 9] = [
    (1, 16),
    (1, 8),
    (1, 4),
    (1, 2),
    (1, 1),
    (2, 1),
    (4, 1),
    (8, 1),
    (16, 1),
];

/// The wheel's division: a box is cut in eight, in price and in time.
const DIVISIONS: u32 = 8;

/// The highest level a fixed square draws: TradingView's levels 0 through 5.
const FIXED_LEVELS: u32 = 5;

/// The box: the rectangle between the points, cut into eighths, with its angles.
#[must_use]
pub fn boxed(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x0, y0) = (geometry.x(0), geometry.y(0));
    let (x1, y1) = (geometry.x(1), geometry.y(1));
    let mut parts = vec![border(x0, y0, x1, y1)];
    parts.extend(eighths(x0, y0, x1, y1));
    // The drag's own diagonal is the 1×1, and the other one is its reflection:
    // `reverse` swaps which of the two is the solid claim.
    let (solid, reflected) = if geometry.options.reverse {
        ((x1, y0, x0, y1), (x0, y0, x1, y1))
    } else {
        ((x0, y0, x1, y1), (x1, y0, x0, y1))
    };
    parts.push(diagonal(solid, false));
    parts.push(diagonal(reflected, true));
    parts
}

/// The fan: nine rays from the first point at the angles the second point scales.
#[must_use]
pub fn fan(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some(scale) = scale(geometry) else {
        return Vec::new();
    };
    let apex = (geometry.x(0), geometry.y(0));
    let mut parts = Vec::with_capacity(ANGLES.len() * 2);
    for (price, bars) in ANGLES {
        let slope = screen_slope(geometry, apex, scale, price, bars);
        let ray = ray_to(geometry, apex, slope, apex.0, Extend::Right);
        // The label rides the ray's own far end, so an `extend` that moves the
        // stroke to the other edge takes its ratio with it.
        if let DrawingPart::Segment { x2, y2, .. } = &ray {
            parts.push(DrawingPart::Text {
                x: x2 - 32.0,
                y: y2 - 3.0,
                text: format!("{price}x{bars}"),
            });
        }
        parts.push(ray);
    }
    parts
}

/// The square: the box's grid and fan, with arcs at the eighths of its side.
///
/// The difference from [`boxed`] is the arcs — a square is the box read as a
/// radius around its corner, which is the step Gann's own construction takes
/// from the grid to the wheel.
#[must_use]
pub fn square(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x0, y0) = (geometry.x(0), geometry.y(0));
    let (x1, y1) = (geometry.x(1), geometry.y(1));
    let mut parts = vec![border(x0, y0, x1, y1)];
    parts.extend(eighths(x0, y0, x1, y1));
    parts.push(diagonal((x0, y0, x1, y1), false));
    parts.push(diagonal((x1, y0, x0, y1), false));
    let radius = (x1 - x0).abs();
    for step in 1..=DIVISIONS {
        let r = radius * f64::from(step) / f64::from(DIVISIONS);
        parts.push(DrawingPart::Ellipse {
            cx: x0,
            cy: y0,
            rx: r,
            ry: r,
            half: true,
            filled: false,
        });
    }
    parts
}

/// The fixed square: levels 0 through 5, one unit square each.
///
/// The second point does not set a direction here, it sets a *size*: the drag is
/// one unit square, and the levels are that square repeated. That is what "fixed"
/// means — the scale stays where the user put it instead of being recomputed
/// from each new corner.
#[must_use]
pub fn square_fixed(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x0, y0) = (geometry.x(0), geometry.y(0));
    let (dx, dy) = (geometry.x(1) - x0, geometry.y(1) - y0);
    let sense = if geometry.options.reverse { -1.0 } else { 1.0 };
    let mut parts = Vec::new();
    for level in 0..=FIXED_LEVELS {
        let n = f64::from(level) * sense;
        let (x, y) = (x0 + dx * n, y0 + dy * n);
        if level > 0 {
            parts.push(border(x0, y0, x, y));
            // Each square's own 1×1: the diagonal the ladder is built on.
            parts.push(diagonal((x0, y0, x, y), false));
        }
        parts.push(DrawingPart::Text {
            x: x + 4.0 * sense,
            y,
            text: level.to_string(),
        });
    }
    parts
}

/// The price/time ratio a Gann drawing declares, in the units it is drawn in.
#[derive(Debug, Clone, Copy)]
struct Scale {
    /// Canvas x for one bar.
    px_per_bar: f64,
    /// Signed price units per bar: one unit passes through the second point.
    price_per_bar: f64,
}

/// The ratio the two points state, or `None` when they state none.
///
/// A drag with no time between its points has no bars in its unit and a drag
/// with no price between them has no claim to make, so both are refused here
/// rather than producing an angle whose slope is infinite or zero for a reason
/// the reader cannot see.
fn scale(geometry: Geometry<'_>) -> Option<Scale> {
    if geometry.points.len() < 2 {
        return None;
    }
    let bar_ms = geometry.frame.bar_nanos as f64 / 1_000_000.0;
    if bar_ms <= 0.0 {
        return None;
    }
    let px_per_bar = geometry.frame.x_at_ms(geometry.points[0].0 + bar_ms) - geometry.x(0);
    let bars = (geometry.points[1].0 - geometry.points[0].0) / bar_ms;
    let price = geometry.points[1].1 - geometry.points[0].1;
    if px_per_bar == 0.0 || bars == 0.0 || price == 0.0 {
        return None;
    }
    Some(Scale {
        px_per_bar,
        price_per_bar: price / bars,
    })
}

/// The canvas slope of one angle, measured one bar from the apex.
///
/// Measured rather than derived from the frame's two scales separately: the
/// price axis may be logarithmic, and a slope computed as a ratio of the linear
/// ranges would draw a straight line through a curve's two ends.
fn screen_slope(geometry: Geometry<'_>, apex: (f64, f64), scale: Scale, price: u32, bars: u32) -> f64 {
    let k = f64::from(price) / f64::from(bars);
    let one_bar_ahead = geometry
        .frame
        .y_at(geometry.price(0) + scale.price_per_bar * k)
        - apex.1;
    one_bar_ahead / scale.px_per_bar
}

/// The box's outline, unfilled so the candles inside it stay readable.
fn border(x0: f64, y0: f64, x1: f64, y1: f64) -> DrawingPart {
    DrawingPart::Rect {
        x: x0.min(x1),
        y: y0.min(y1),
        w: (x1 - x0).abs(),
        h: (y1 - y0).abs(),
        filled: false,
    }
}

/// The seven interior divisions on each axis. Dashed: the grid is scaffolding
/// under the box's two solid edges.
fn eighths(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<DrawingPart> {
    let mut parts = Vec::with_capacity(DIVISIONS as usize * 2);
    for step in 1..DIVISIONS {
        let f = f64::from(step) / f64::from(DIVISIONS);
        let x = x0 + (x1 - x0) * f;
        let y = y0 + (y1 - y0) * f;
        parts.push(DrawingPart::Segment {
            x1: x,
            y1: y0.min(y1),
            x2: x,
            y2: y0.max(y1),
            dashed: true,
        });
        parts.push(DrawingPart::Segment {
            x1: x0.min(x1),
            y1: y,
            x2: x0.max(x1),
            y2: y,
            dashed: true,
        });
    }
    parts
}

/// One corner-to-corner angle.
fn diagonal((x1, y1, x2, y2): (f64, f64, f64, f64), dashed: bool) -> DrawingPart {
    DrawingPart::Segment {
        x1,
        y1,
        x2,
        y2,
        dashed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{Point, Stage};
    use crate::drawing::DrawingOptions;

    /// A drag of eight bars and eighty price units — a 1×1 of ten per bar, and
    /// an eighth of the box that a test can name without arithmetic.
    fn unit() -> (Stage, Vec<Point>) {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let points = vec![(stage.ms(2), 120.0), (stage.ms(10), 200.0)];
        (stage, points)
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Every solid ray in the list, as `(x1, y1, x2, y2)`.
    fn rays(parts: &[DrawingPart]) -> Vec<(f64, f64, f64, f64)> {
        parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Segment {
                    x1,
                    y1,
                    x2,
                    y2,
                    dashed: false,
                } => Some((*x1, *y1, *x2, *y2)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_box_is_cut_into_eighths_on_both_axes() {
        let (stage, points) = unit();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let parts = boxed(geometry);
        let (x0, y0, x1, y1) = (geometry.x(0), geometry.y(0), geometry.x(1), geometry.y(1));

        // The outline first, unfilled so the candles under it stay readable.
        match &parts[0] {
            DrawingPart::Rect { x, y, w, h, filled } => {
                // The outline spans the two corners whichever way the drag
                // went, and a rising drag puts point 1 *above* point 0, so the
                // rectangle's top edge is the smaller y, not the first point's.
                assert!(near(*x, x0.min(x1)) && near(*y, y0.min(y1)));
                assert!(near(*w, (x1 - x0).abs()) && near(*h, (y1 - y0).abs()));
                assert!(!filled, "a box is a grid, not a block of colour");
            }
            other => panic!("a box opens with its outline, not {other:?}"),
        }

        let dashed: Vec<&DrawingPart> = parts
            .iter()
            .filter(|part| matches!(part, DrawingPart::Segment { dashed: true, .. }))
            .collect();
        // Seven each way, plus the reflected angle, which is a claim the box
        // does not make yet and so is drawn dashed.
        assert_eq!(dashed.len(), 15, "seven verticals, seven horizontals, one reflection");

        let verticals: Vec<f64> = dashed
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Segment { x1, x2, .. } if x1 == x2 => Some(*x1),
                _ => None,
            })
            .collect();
        assert_eq!(verticals.len(), 7);
        for (step, x) in verticals.iter().enumerate() {
            let expected = x0 + (x1 - x0) * (step + 1) as f64 / 8.0;
            assert!(near(*x, expected), "division {step} at {x}, wanted {expected}");
        }
    }

    #[test]
    fn the_boxes_solid_angle_is_the_drag_that_declared_the_scale() {
        let (stage, points) = unit();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let parts = boxed(geometry);
        let (x0, y0, x1, y1) = (geometry.x(0), geometry.y(0), geometry.x(1), geometry.y(1));

        let solid = rays(&parts);
        assert_eq!(solid.len(), 1, "one claim is solid; the reflection is not");
        assert!(near(solid[0].0, x0) && near(solid[0].1, y0));
        assert!(near(solid[0].2, x1) && near(solid[0].3, y1));

        let reversed = DrawingOptions {
            reverse: true,
            ..DrawingOptions::default()
        };
        let mirrored = rays(&boxed(stage.geometry(&points, &reversed)));
        assert_eq!(mirrored.len(), 1);
        assert!(
            near(mirrored[0].0, x1) && near(mirrored[0].1, y0),
            "reverse reflects the angle about the box's vertical axis"
        );
    }

    #[test]
    fn the_fan_draws_nine_angles_and_the_one_one_through_the_second_point() {
        let (stage, points) = unit();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let parts = fan(geometry);
        let (x0, y0, x1, y1) = (geometry.x(0), geometry.y(0), geometry.x(1), geometry.y(1));
        let right = geometry.frame.plot.x + geometry.frame.plot.w;

        let list = rays(&parts);
        assert_eq!(list.len(), ANGLES.len(), "nine angles, not eight");

        // Every ray leaves the pivot and runs to the plot's edge.
        for (index, (from_x, from_y, to_x, _)) in list.iter().enumerate() {
            assert!(near(*from_x, x0) && near(*from_y, y0), "ray {index} starts at the pivot");
            assert!(near(*to_x, right), "ray {index} ends at the edge");
        }

        // Nine distinct slopes: a fan whose angles repeat is not a fan.
        let mut slopes: Vec<f64> = list
            .iter()
            .map(|(x1, y1, x2, y2)| (y2 - y1) / (x2 - x1))
            .collect();
        slopes.sort_by(|a, b| a.partial_cmp(b).expect("no NaN slopes"));
        for pair in slopes.windows(2) {
            assert!(pair[1] - pair[0] > 1e-9, "two angles share a slope");
        }

        // The 1×1 is the drag: it passes through the second point exactly,
        // which is what "the second point sets the scale" has to mean.
        let one = list[4];
        let slope = (one.3 - one.1) / (one.2 - one.0);
        let through = one.1 + slope * (x1 - one.0);
        assert!(
            near(through, y1),
            "the 1x1 misses the point that set it: {through} vs {y1}"
        );

        let labels: Vec<&str> = parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["1x16", "1x8", "1x4", "1x2", "1x1", "2x1", "4x1", "8x1", "16x1"]);
    }

    #[test]
    fn a_drag_with_no_time_span_states_no_scale_and_draws_nothing() {
        let (stage, _) = unit();
        let options = DrawingOptions::default();
        let points = vec![(stage.ms(2), 120.0), (stage.ms(2), 200.0)];
        assert!(
            fan(stage.geometry(&points, &options)).is_empty(),
            "no bars between the points means no price per bar"
        );
    }

    #[test]
    fn the_square_rings_the_anchor_at_the_eighths_of_its_side() {
        let (stage, points) = unit();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let parts = square(geometry);
        let (x0, y0) = (geometry.x(0), geometry.y(0));
        let side = (geometry.x(1) - x0).abs();

        let arcs: Vec<(f64, f64, f64, f64, bool)> = parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Ellipse { cx, cy, rx, ry, half, .. } => Some((*cx, *cy, *rx, *ry, *half)),
                _ => None,
            })
            .collect();
        assert_eq!(arcs.len(), DIVISIONS as usize);
        for (step, (cx, cy, rx, ry, half)) in arcs.iter().enumerate() {
            assert!(near(*cx, x0) && near(*cy, y0), "every arc is centred on the anchor");
            assert!(near(*rx, *ry), "a circle, not a stretched one");
            assert!(*half, "an arc is the upper half of its circle");
            let expected = side * (step + 1) as f64 / DIVISIONS as f64;
            assert!(near(*rx, expected), "radius {step} at {rx}, wanted {expected}");
        }
    }

    #[test]
    fn a_fixed_square_steps_the_levels_by_the_drag_itself() {
        let (stage, points) = unit();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let parts = square_fixed(geometry);
        let (x0, y0) = (geometry.x(0), geometry.y(0));
        let dx = geometry.x(1) - x0;

        let labels: Vec<&str> = parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["0", "1", "2", "3", "4", "5"]);

        let mut edges: Vec<f64> = parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Rect { x, w, .. } => Some(x + w),
                _ => None,
            })
            .collect();
        edges.sort_by(|a, b| a.partial_cmp(b).expect("no NaN edges"));
        assert_eq!(edges.len(), FIXED_LEVELS as usize, "level 0 has no square yet");
        for (step, edge) in edges.iter().enumerate() {
            let expected = x0 + dx * (step + 1) as f64;
            assert!(
                near(*edge, expected),
                "the levels are evenly spaced in time: {edge} vs {expected}"
            );
        }
    }

    #[test]
    fn reversing_a_fixed_square_walks_the_levels_the_other_way_from_the_anchor() {
        let (stage, points) = unit();
        let options = DrawingOptions {
            reverse: true,
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);
        let parts = square_fixed(geometry);
        let x0 = geometry.x(0);
        let dx = geometry.x(1) - x0;

        // The corner furthest from the anchor, whichever side of it the square
        // was drawn on: reverse walks the levels back from where the anchor is.
        let mut far: Vec<f64> = parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Rect { x, w, .. } => {
                    let (left, right) = (*x, x + w);
                    Some(if (left - x0).abs() > (right - x0).abs() {
                        left
                    } else {
                        right
                    })
                }
                _ => None,
            })
            .collect();
        far.sort_by(|a, b| b.partial_cmp(a).expect("no NaN edges"));
        assert_eq!(far.len(), FIXED_LEVELS as usize);
        for (step, edge) in far.iter().enumerate() {
            let expected = x0 - dx * (step + 1) as f64;
            assert!(near(*edge, expected), "level {step} at {edge}, wanted {expected}");
        }
    }
}
