//! The shapes group: a closed outline around the points, filled or not.
//!
//! Four kinds and no clipping: a rectangle, a triangle, a circle and an arc each
//! take the points they declare and close the boundary through them. What differs
//! is only which point means what -- two opposite corners, one vertex each, a
//! centre plus a radius.
//!
//! The `filled` flag is the difference between "a zone the user placed" and "a
//! measurement", and the two are deliberately not the same shape: a filled
//! rectangle reads as an area of the chart that means something, which is why
//! [`crate::draw::measurement`] strokes the same box empty.

use crate::drawing::DrawingPart;

use super::Geometry;

/// The rectangle with two anchors at opposite corners.
///
/// Corner order is not guaranteed -- a drag up-and-left has its first point at
/// the bottom right -- so the box is normalised from the min and the absolute
/// span rather than assumed.
#[must_use]
pub fn rect(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some(box_) = corners(geometry) else {
        return Vec::new();
    };
    vec![DrawingPart::Rect {
        x: box_.0,
        y: box_.1,
        w: box_.2,
        h: box_.3,
        filled: true,
    }]
}

/// The triangle with one point per vertex.
#[must_use]
pub fn triangle(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    vec![DrawingPart::Polygon {
        points: vec![
            (geometry.x(0), geometry.y(0)),
            (geometry.x(1), geometry.y(1)),
            (geometry.x(2), geometry.y(2)),
        ],
        filled: true,
    }]
}

/// The circle with a centre and one point on its radius.
///
/// The radius is the *distance* in canvas pixels, so the shape is a circle
/// whatever the drag direction -- which is the whole difference from a rectangle
/// stretched around the same two points.
#[must_use]
pub fn circle(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let dx = geometry.x(1) - geometry.x(0);
    let dy = geometry.y(1) - geometry.y(0);
    let r = (dx * dx + dy * dy).sqrt().max(1.0);
    vec![DrawingPart::Ellipse {
        cx: geometry.x(0),
        cy: geometry.y(0),
        rx: r,
        ry: r,
        half: false,
        filled: true,
    }]
}

/// The arc: centre, then a point for each radius.
///
/// The second point sets the horizontal radius with its *time* and the third the
/// vertical with its *price*, because those are the axes each drag travels along.
/// Half the ellipse, upper only -- an "arc" tool that drew a full ellipse is a
/// circle.
#[must_use]
pub fn arc(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let rx = (geometry.x(1) - geometry.x(0)).abs();
    let ry = (geometry.y(2) - geometry.y(0)).abs();
    vec![DrawingPart::Ellipse {
        cx: geometry.x(0),
        cy: geometry.y(0),
        rx: rx.max(1.0),
        ry: ry.max(1.0),
        half: true,
        filled: false,
    }]
}

/// The normalised `(x, y, w, h)` of the two-corner box, if there are two corners.
///
/// Shared with the measurement group for the same reason the parts are not: both
/// ask "the box between these two points", and a second copy is how a drag
/// up-and-left ends up with a negative width in one of them.
pub(super) fn corners(geometry: Geometry<'_>) -> Option<(f64, f64, f64, f64)> {
    if geometry.points.len() < 2 {
        return None;
    }
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    Some((
        x1.min(x2),
        y1.min(y2),
        (x2 - x1).abs(),
        (y2 - y1).abs(),
    ))
}
