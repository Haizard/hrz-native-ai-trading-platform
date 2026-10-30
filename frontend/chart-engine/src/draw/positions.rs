//! The position group: an entry line, a profit band, a stop band.
//!
//! One geometry for both kinds. A long's target is above its entry and a short's
//! below, and everything else -- the width the second point's time sets, the 1:1
//! default, the two labels -- is the same drawing read from the other side.
//!
//! ## The 1:1 default
//!
//! The second anchor's price is read as the *target*, and the stop is its mirror
//! across the entry line. That is the reference behaviour for a risk-reward box
//! before the user has said anything more, and it is why the tool takes two
//! points rather than three: the third number is implied by the first two, and a
//! box that asked for it would be a box the user has to fill in before it means
//! anything.

use crate::drawing::DrawingPart;

use super::Geometry;

/// The risk box for one position, entered at the first point and targeted at the
/// second.
#[must_use]
pub fn risk_box(geometry: Geometry<'_>, is_long: bool) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let (p1, p2) = (geometry.price(0), geometry.price(1));
    let left = x1.min(x2);
    let width = (x2 - x1).abs();
    let risk = (y1 - y2).abs();

    // The stop band sits on the far side of the entry from the target: below it
    // for a long, above for a short.
    let stop_y = if is_long { y1 } else { y1 - risk };
    let stop_price = if is_long {
        p1 - (p2 - p1).abs()
    } else {
        p1 + (p1 - p2).abs()
    };

    vec![
        // The entry line, spanning the box so it reads as a price rather than the
        // band's edge.
        DrawingPart::Segment {
            x1: left,
            y1,
            x2: x1.max(x2),
            y2: y1,
            dashed: false,
        },
        // Profit: the band between entry and target, filled.
        DrawingPart::Rect {
            x: left,
            y: y1.min(y2),
            w: width,
            h: risk,
            filled: true,
        },
        // Stop: the same height mirrored across the entry, outlined only, so the
        // two bands are tellable apart without a legend.
        DrawingPart::Rect {
            x: left,
            y: stop_y,
            w: width,
            h: risk,
            filled: false,
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: y2 + 10.0,
            text: format!("target {p2:.2}"),
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: stop_y + 10.0,
            text: format!("stop {stop_price:.2}"),
        },
    ]
}
