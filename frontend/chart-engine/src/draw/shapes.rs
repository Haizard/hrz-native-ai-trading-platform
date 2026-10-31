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

/// A rotated rectangle: three clicks define one edge (points 0→1) and the
/// depth/angle (point 2). The fourth corner is the vector sum.
#[must_use]
pub fn rotated_rect(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let (x0, y0) = (geometry.x(0), geometry.y(0));
    let (x1, y1) = (geometry.x(1), geometry.y(1));
    let (x2, y2) = (geometry.x(2), geometry.y(2));
    // The fourth corner = point 1 + (point 2 - point 0), closing the parallelogram.
    let x3 = x1 + (x2 - x0);
    let y3 = y1 + (y2 - y0);
    vec![DrawingPart::Polygon {
        points: vec![(x0, y0), (x1, y1), (x3, y3), (x2, y2)],
        filled: false,
    }]
}

/// A full ellipse: centre at anchor 0, anchor 1 on the circumference.
///
/// The radii are the absolute dx and dy from the centre.
#[must_use]
pub fn ellipse(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let cx = geometry.x(0);
    let cy = geometry.y(0);
    let rx = (geometry.x(1) - cx).abs();
    let ry = (geometry.y(1) - cy).abs();
    vec![DrawingPart::Ellipse {
        cx,
        cy,
        rx,
        ry,
        half: false,
        filled: false,
    }]
}

/// Polyline: a connected path through all points, optionally closed.
#[must_use]
pub fn polyline(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let n = geometry.points.len();
    if n < 2 {
        return Vec::new();
    }
    let mut pts: Vec<(f64, f64)> = (0..n).map(|i| (geometry.x(i), geometry.y(i))).collect();
    if geometry.options.closed && n >= 3 {
        pts.push(pts[0]);
        vec![DrawingPart::Polygon {
            points: pts,
            filled: false,
        }]
    } else {
        let segments: Vec<DrawingPart> = (0..n - 1)
            .map(|i| DrawingPart::Segment {
                x1: pts[i].0,
                y1: pts[i].1,
                x2: pts[i + 1].0,
                y2: pts[i + 1].1,
                dashed: false,
            })
            .collect();
        segments
    }
}

/// Curve: a smooth path through 3+ points, approximated by subdividing
/// quadratic Bézier segments between consecutive midpoints.
#[must_use]
pub fn curve(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let n = geometry.points.len();
    if n < 3 {
        return Vec::new();
    }
    let pts: Vec<(f64, f64)> = (0..n).map(|i| (geometry.x(i), geometry.y(i))).collect();
    let mut parts = Vec::new();
    let subdivisions = 12;
    // Each pair of consecutive points shares a quadratic curve with the
    // midpoint between them as start/end and the actual point as control.
    for i in 1..n - 1 {
        let (sx, sy) = lerp_pair(&pts[i - 1], &pts[i], 0.5);
        let (ex, ey) = lerp_pair(&pts[i], &pts[i + 1], 0.5);
        let (cx, cy) = pts[i];
        for s in 0..subdivisions {
            let t0 = s as f64 / subdivisions as f64;
            let t1 = (s + 1) as f64 / subdivisions as f64;
            let (ax, ay) = quadratic(sx, sy, cx, cy, ex, ey, t0);
            let (bx, by) = quadratic(sx, sy, cx, cy, ex, ey, t1);
            parts.push(DrawingPart::Segment {
                x1: ax,
                y1: ay,
                x2: bx,
                y2: by,
                dashed: false,
            });
        }
    }
    parts
}

/// Double curve (S-shape): three points — start, control, end. One quadratic.
#[must_use]
pub fn double_curve(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let (sx, sy) = (geometry.x(0), geometry.y(0));
    let (cx, cy) = (geometry.x(1), geometry.y(1));
    let (ex, ey) = (geometry.x(2), geometry.y(2));
    let subdivisions = 24;
    let mut parts = Vec::with_capacity(subdivisions);
    for s in 0..subdivisions {
        let t0 = s as f64 / subdivisions as f64;
        let t1 = (s + 1) as f64 / subdivisions as f64;
        let (ax, ay) = quadratic(sx, sy, cx, cy, ex, ey, t0);
        let (bx, by) = quadratic(sx, sy, cx, cy, ex, ey, t1);
        parts.push(DrawingPart::Segment {
            x1: ax,
            y1: ay,
            x2: bx,
            y2: by,
            dashed: false,
        });
    }
    parts
}

/// Brush: freehand path, same geometry as an open polyline.
#[must_use]
pub fn brush(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    polyline(geometry)
}

/// Highlighter: same geometry as brush; the shell renders it wider and translucent.
#[must_use]
pub fn highlighter(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    polyline(geometry)
}

fn lerp_pair(a: &(f64, f64), b: &(f64, f64), t: f64) -> (f64, f64) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn quadratic(sx: f64, sy: f64, cx: f64, cy: f64, ex: f64, ey: f64, t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    (
        u * u * sx + 2.0 * u * t * cx + t * t * ex,
        u * u * sy + 2.0 * u * t * cy + t * t * ey,
    )
}
