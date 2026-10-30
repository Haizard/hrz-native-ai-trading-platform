//! The geometry of a drawing: resolved points in, positioned parts out.
//!
//! `scene.rs` owns the frame, the anchors and the request; this module owns the
//! rule for what goes *between* the points, and it is split one way per toolbar
//! group so a 80-tool vocabulary stays readable (`docs/21`). The split is
//! enforced by the compiler rather than by review: [`parts`] matches on every
//! [`DrawingKind`] and has no wildcard arm, so a new kind without geometry is a
//! build failure, not a tool that draws nothing.
//!
//! ## What a group function is given
//!
//! A [`Geometry`] — the points already resolved to `(milliseconds, price)`, the
//! frame that turns those into pixels, and the options the user set. There is no
//! unit left to get wrong inside a group function, which is why the resolved
//! pair is passed rather than the [`crate::drawing::Anchor`]s: a fraction that
//! reached geometry code would be a *second* place the plot is inverted, and the
//! whole reason [`crate::scene`] exists is that there is one.
//!
//! ## What a group function returns
//!
//! [`DrawingPart`]s in canvas pixels, already ordered, already dashed or filled.
//! The shell strokes them and decides nothing (`docs/14`), and a shape that
//! cannot be placed is refused upstream in `scene::place` rather than returning
//! an empty list from here.

use crate::drawing::{DrawingKind, DrawingOptions, DrawingPart, Extend};
use crate::scene::Frame;

pub mod channels;
pub mod fibonacci;
pub mod lines;
pub mod measurement;
pub mod positions;
pub mod shapes;

/// One point of a drawing, resolved: `(milliseconds, price)`.
pub type Point = (f64, f64);

/// Everything a group function reads: where the points are, what the user asked
/// for, and the mapping that makes either one a pixel.
#[derive(Debug, Clone, Copy)]
pub struct Geometry<'a> {
    /// The window's price/time mapping.
    pub frame: &'a Frame,
    /// The drawing's points, in placement order.
    pub points: &'a [Point],
    /// What the user asked the shape to do beyond where its points are.
    pub options: &'a DrawingOptions,
}

impl Geometry<'_> {
    /// A point's canvas x.
    #[must_use]
    pub fn x(self, index: usize) -> f64 {
        self.frame.x_at_ms(self.points[index].0)
    }

    /// A point's canvas y.
    #[must_use]
    pub fn y(self, index: usize) -> f64 {
        self.frame.y_at(self.points[index].1)
    }

    /// A point's price.
    #[must_use]
    pub fn price(self, index: usize) -> f64 {
        self.points[index].1
    }

    /// The point at `index`, when the drawing has one.
    #[must_use]
    pub fn point(self, index: usize) -> Option<Point> {
        self.points.get(index).copied()
    }
}

/// The shapes one drawing is made of, including its grab points.
///
/// `points` is `kind.anchors()` long -- `place` refused anything else -- so a
/// group function may index the points its kind declares and no further.
#[must_use]
pub fn parts(
    kind: DrawingKind,
    selected: bool,
    geometry: Geometry<'_>,
) -> Vec<DrawingPart> {
    let mut parts = match kind {
        // Lines.
        DrawingKind::Trendline => lines::stroked(geometry, Extend::None),
        DrawingKind::Ray => lines::stroked(geometry, Extend::Right),
        DrawingKind::Extended => lines::stroked(geometry, Extend::Both),
        DrawingKind::Angle => lines::stroked(geometry, Extend::None),
        DrawingKind::Hline => lines::horizontal(geometry),
        DrawingKind::Vline => lines::vertical(geometry),
        // Channels.
        DrawingKind::Channel => channels::parallel(geometry),
        // Shapes.
        DrawingKind::Rect => shapes::rect(geometry),
        DrawingKind::Triangle => shapes::triangle(geometry),
        DrawingKind::Circle => shapes::circle(geometry),
        DrawingKind::Arc => shapes::arc(geometry),
        // Fibonacci.
        DrawingKind::Fib => fibonacci::retracement(geometry),
        DrawingKind::FibExtension => fibonacci::extension(geometry),
        // Measurement.
        DrawingKind::Measure => measurement::ruler(geometry),
        DrawingKind::DatepriceRange => measurement::range(geometry),
        // Positions.
        DrawingKind::PositionLong => positions::risk_box(geometry, true),
        DrawingKind::PositionShort => positions::risk_box(geometry, false),
    };

    // A grab point at every point the kind declares, and only when this drawing
    // is the selected one. That is what makes "the shell cannot offer a grab
    // point the engine would not honour" true rather than aspirational: the
    // handles come from the same list the geometry drew from.
    if selected {
        for index in 0..geometry.points.len() {
            parts.push(DrawingPart::Handle {
                x: geometry.x(index),
                y: geometry.y(index),
                anchor: index as u8,
            });
        }
    }

    parts
}
