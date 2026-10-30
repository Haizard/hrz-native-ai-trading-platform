//! The measurement group: a box between two points that reports a number.
//!
//! Both kinds draw the same outline as [`crate::draw::shapes::rect`] and differ
//! in what they say about it. That is deliberate: a measurement is a question
//! about two prices and two times, and the box is only the frame around the
//! answer.
//!
//! ## The numbers are formatted here
//!
//! A percentage and a bar count are arithmetic, and `docs/14` keeps arithmetic out
//! of JavaScript. The bar count is measured against the series' own timeframe,
//! which the [`crate::scene::Frame`] carries, rather than guessed from the pixel
//! width -- the same reason the price scale is in Rust.

use crate::drawing::DrawingPart;

use super::shapes::corners;
use super::Geometry;

/// The price/distance ruler: an empty box, guides to both axes, and the delta.
#[must_use]
pub fn ruler(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((left, top, w, h)) = corners(geometry) else {
        return Vec::new();
    };
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let (t1, p1) = geometry.points[0];
    let (t2, p2) = geometry.points[1];
    let bars = bar_count(geometry, t1, t2);
    let change = (p2 - p1).abs() / p1.abs().max(f64::EPSILON) * 100.0;
    vec![
        // Unfilled: it is a measurement, and a filled one would read as a zone
        // the user placed.
        DrawingPart::Rect {
            x: left,
            y: top,
            w,
            h,
            filled: false,
        },
        // Dashed guides to both axes, so which two things are being compared is
        // on the chart rather than implied.
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2,
            y2: y1,
            dashed: true,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2: x1,
            y2,
            dashed: true,
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: top + 10.0,
            text: format!("{:.2} ({change:.1}%) {bars} bars", (p2 - p1).abs()),
        },
    ]
}

/// The date-range box: the same drawing, reporting the range as a percentage.
///
/// The parity tool's whole addition is the signed percentage instead of the
/// absolute delta, which is the question a date range actually asks -- "what did
/// this stretch of time do to price?"
#[must_use]
pub fn range(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((left, top, w, h)) = corners(geometry) else {
        return Vec::new();
    };
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let (t1, p1) = geometry.points[0];
    let (t2, p2) = geometry.points[1];
    let bars = bar_count(geometry, t1, t2);
    let change = (p2 - p1) / p1.abs().max(f64::EPSILON) * 100.0;
    vec![
        DrawingPart::Rect {
            x: left,
            y: top,
            w,
            h,
            filled: false,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2,
            y2: y1,
            dashed: true,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2: x1,
            y2,
            dashed: true,
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: top + 10.0,
            text: format!("{change:+.2}% {bars} bars"),
        },
    ]
}

/// How many bars of the series' own timeframe lie between two timestamps.
fn bar_count(geometry: Geometry<'_>, t1: f64, t2: f64) -> i64 {
    ((t2 - t1).abs() * 1_000_000.0 / geometry.frame.bar_nanos as f64).round() as i64
}
