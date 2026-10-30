//! The channels group: two parallel lines, the second one's offset set by a
//! third point.
//!
//! A channel is a *band* claim -- price holds between these two lines -- so both
//! edges span the plot rather than stopping at the anchors, the same reason a
//! horizontal level does ([`crate::draw::lines::horizontal`]). The width is
//! measured on the price axis only: the third click's time is where the user
//! happened to click, and its price is the thing they were pointing at.

use crate::drawing::DrawingPart;

use super::Geometry;

/// The line through the first two points and its parallel through the third.
#[must_use]
pub fn parallel(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let plot = geometry.frame.plot;
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let y3 = geometry.y(2);
    let left = plot.x;
    let right = plot.x + plot.w;
    let slope = (y2 - y1) / (x2 - x1).max(f64::EPSILON);
    // The offset is the gap between the third click and the midline, so a channel
    // can be drawn *around* a line rather than only from one edge of a band.
    let offset = y3 - (y1 + y2) / 2.0;
    vec![
        DrawingPart::Segment {
            x1: left,
            y1: y1 + slope * (left - x1),
            x2: right,
            y2: y1 + slope * (right - x1),
            dashed: false,
        },
        DrawingPart::Segment {
            x1: left,
            y1: y1 + slope * (left - x1) + offset,
            x2: right,
            y2: y1 + slope * (right - x1) + offset,
            dashed: false,
        },
    ]
}
