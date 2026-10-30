//! The lines group: a straight stroke through two points, clipped however the
//! tool says it should be.
//!
//! Six kinds, one geometry. What differs between a trendline, a ray, an extended
//! line and an angle is only *where the stroke stops*, so they share
//! [`stroked`] and pass their own default; a horizontal and a vertical are the
//! two degenerate cases where the plot itself is the extent.
//!
//! The clipping is parametric rather than edge-tested: the line is
//! `p1 + k * (p2 - p1)`, and each stop is a value of `k` (`0` at the first point,
//! `1` at the second), so the four cases are four `k` ranges computed the same
//! way. That matters because the answer must be the *visible* span: a stroke that
//! runs past the plot is clipped by the canvas and its dashed pattern restarts,
//! and one that stops at the second anchor is a trendline wearing another name.

use crate::drawing::{DrawingPart, Extend};

use super::Geometry;

/// The stroke between the two points, extended as `default` or as the user asked.
///
/// The kind's own default is a parameter because each line tool's reference
/// states a different one: TradingView's trendline stops at its anchors, its ray
/// "extends to the right of the second point", and its extended line "passes
/// through both points and extends infinitely in both directions". The
/// [`Extend`] option then overrides it -- which is the *Extend Left/Right*
/// checkbox on all three pages -- and an absent option means "the tool's own".
#[must_use]
pub fn stroked(geometry: Geometry<'_>, default: Extend) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let extent = geometry.options.extend.unwrap_or(default);
    let (from, to) = match extent {
        Extend::None => ((x1, y1), (x2, y2)),
        Extend::Right => ((x1, y1), forward(geometry, x1, y1, x2 - x1, y2 - y1)),
        Extend::Left => (forward(geometry, x2, y2, x1 - x2, y1 - y2), (x2, y2)),
        Extend::Both => both(geometry, x1, y1, x2 - x1, y2 - y1),
    };
    vec![DrawingPart::Segment {
        x1: from.0,
        y1: from.1,
        x2: to.0,
        y2: to.1,
        dashed: false,
    }]
}

/// A horizontal level across the whole plot, at the first point's price.
///
/// Across the plot rather than between two anchors because a horizontal line *is*
/// a price: one that stopped at wherever the user happened to click would be a
/// segment with an unfinished claim. The price is written at the left end for the
/// same reason the axis is there -- to be read without a crosshair.
#[must_use]
pub fn horizontal(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let y = geometry.y(0);
    let price = geometry.price(0);
    vec![
        DrawingPart::Segment {
            x1: plot.x,
            y1: y,
            x2: plot.x + plot.w,
            y2: y,
            dashed: false,
        },
        DrawingPart::Text {
            x: plot.x + 4.0,
            y: y - 4.0,
            text: format!("{price:.2}"),
        },
    ]
}

/// A vertical line down the whole plot, at the first point's time.
///
/// The mirror of [`horizontal`], and the same argument for spanning the plot.
#[must_use]
pub fn vertical(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x = geometry.x(0);
    vec![DrawingPart::Segment {
        x1: x,
        y1: plot.y,
        x2: x,
        y2: plot.y + plot.h,
        dashed: false,
    }]
}

/// How far a ray from `(x, y)` travelling `(dx, dy)` runs before it leaves the
/// plot -- the parametric `k` at which it stops, clamped to the axis that gets
/// there first.
fn forward(geometry: Geometry<'_>, x: f64, y: f64, dx: f64, dy: f64) -> (f64, f64) {
    let plot = geometry.frame.plot;
    let mut k = f64::INFINITY;
    if dx != 0.0 {
        let edge = if dx > 0.0 {
            plot.x + plot.w
        } else {
            plot.x
        };
        k = k.min((edge - x) / dx);
    }
    if dy != 0.0 {
        let edge = if dy > 0.0 {
            plot.y + plot.h
        } else {
            plot.y
        };
        k = k.min((edge - y) / dy);
    }
    if !k.is_finite() {
        // Should be unreachable: both deltas zero is the same-point case, which
        // `scene::place` refused. Guarded anyway, because a `NaN` from `0/0` is
        // a canvas that silently drops the shape.
        k = 1.0;
    }
    (x + dx * k, y + dy * k)
}

/// The visible span of the infinite line through `(x, y)` with direction
/// `(dx, dy)`: the two parametric crossings of the plot rectangle, low first.
fn both(geometry: Geometry<'_>, x: f64, y: f64, dx: f64, dy: f64) -> ((f64, f64), (f64, f64)) {
    let plot = geometry.frame.plot;
    let mut k_low = f64::NEG_INFINITY;
    let mut k_high = f64::INFINITY;
    if dx != 0.0 {
        let left = (plot.x - x) / dx;
        let right = (plot.x + plot.w - x) / dx;
        k_low = k_low.max(left.min(right));
        k_high = k_high.min(left.max(right));
    }
    if dy != 0.0 {
        let top = (plot.y - y) / dy;
        let bottom = (plot.y + plot.h - y) / dy;
        k_low = k_low.max(top.min(bottom));
        k_high = k_high.min(top.max(bottom));
    }
    if !(k_low.is_finite() && k_high.is_finite()) {
        k_low = 0.0;
        k_high = 1.0;
    }
    (
        (x + dx * k_low, y + dy * k_low),
        (x + dx * k_high, y + dy * k_high),
    )
}
