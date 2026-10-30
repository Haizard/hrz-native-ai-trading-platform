//! The fibonacci group: horizontal levels at ratios of one swing.
//!
//! Both kinds read the same two points and the same rule -- `price = from + (to -
//! from) * ratio` -- and differ only in which ratios mean something. A
//! retracement's levels live *inside* the swing, so they are solid: they describe
//! where price already has been. An extension's run *past* the swing, so the
//! projected ones are dashed: they describe where price might go, and a chart
//! that draws a hope and a fact the same way teaches a reader nothing.
//!
//! ## The two options these arms read
//!
//! [`crate::drawing::DrawingOptions::reverse`] swaps which anchor is the swing's
//! zero, which is the reference packages' "flip direction" control and the reason
//! the two anchors are a *pair* rather than an ordered claim about which is the
//! low. [`crate::drawing::Extend`] decides how far the levels run; their default
//! is the anchors themselves, because levels measured over a range and drawn
//! wider than it imply a claim the user did not make.

use crate::drawing::{DrawingPart, Extend, FIB_LEVELS};

use super::Geometry;

/// The retracement set: 0, 23.6, 38.2, 50, 61.8, 78.6, 100.
#[must_use]
pub fn retracement(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    levels(geometry, &FIB_LEVELS, |_| false)
}

/// The extension set: the swing marked 0 and 100, then the projections.
#[must_use]
pub fn extension(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    // 0 and 100 mark the swing itself; the rest are the targets.
    const EXTENSION_LEVELS: [(f64, &str); 7] = [
        (0.0, "0"),
        (1.0, "100"),
        (1.272, "127.2"),
        (1.618, "161.8"),
        (2.0, "200"),
        (2.618, "261.8"),
        (3.618, "361.8"),
    ];
    let mut parts = levels(geometry, &EXTENSION_LEVELS, |ratio| ratio > 1.0);
    if geometry.points.len() >= 2 {
        // The projection ray: out from the 100% level's edge in the direction the
        // swing implies, so the direction is on the chart and not only in the
        // level prices. A zero-length swing was refused upstream by the
        // same-point rule, so `signum` is never 0 here.
        let (from, to) = swing(geometry);
        let direction = (to - from).signum();
        let (left, right) = extent(geometry);
        let edge = if direction >= 0.0 { right } else { left };
        let y = geometry.frame.y_at(to);
        parts.push(DrawingPart::Segment {
            x1: edge,
            y1: y,
            x2: edge + direction * (right - left) * 0.35,
            y2: y,
            dashed: true,
        });
    }
    parts
}

/// The levels themselves: one segment and one label per ratio.
///
/// `dashed` is decided per ratio rather than per kind because the two sets differ
/// exactly there -- a retracement's levels are all facts, an extension's are facts
/// up to 100 and projections after it.
fn levels(
    geometry: Geometry<'_>,
    set: &[(f64, &str)],
    dashed: impl Fn(f64) -> bool,
) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (from, to) = swing(geometry);
    let (left, right) = extent(geometry);
    let mut parts = Vec::with_capacity(set.len() * 2);
    for (ratio, label) in set {
        let price = from + (to - from) * ratio;
        let y = geometry.frame.y_at(price);
        parts.push(DrawingPart::Segment {
            x1: left,
            y1: y,
            x2: right,
            y2: y,
            dashed: dashed(*ratio),
        });
        // The percentage *and* the price, formatted here: turning 0.618 into
        // "61.8" is arithmetic, and `docs/14` keeps arithmetic out of JavaScript.
        parts.push(DrawingPart::Text {
            x: left + 4.0,
            y: y - 3.0,
            text: format!("{label}%  {price:.2}"),
        });
    }
    parts
}

/// The swing's zero and its unit, in the direction the user asked for.
///
/// With `reverse` unset this is the first point to the second, which is what every
/// drawing stored before the option existed already means.
fn swing(geometry: Geometry<'_>) -> (f64, f64) {
    let first = geometry.price(0);
    let second = geometry.price(1);
    if geometry.options.reverse {
        (second, first)
    } else {
        (first, second)
    }
}

/// How far across the plot the levels run: between the anchors unless an option
/// says one or both ends go to the plot's edge.
fn extent(geometry: Geometry<'_>) -> (f64, f64) {
    let plot = geometry.frame.plot;
    let left = geometry.x(0).min(geometry.x(1));
    let right = geometry.x(0).max(geometry.x(1));
    match geometry.options.extend.unwrap_or(Extend::None) {
        Extend::None => (left, right),
        Extend::Left => (plot.x, right),
        Extend::Right => (left, plot.x + plot.w),
        Extend::Both => (plot.x, plot.x + plot.w),
    }
}
