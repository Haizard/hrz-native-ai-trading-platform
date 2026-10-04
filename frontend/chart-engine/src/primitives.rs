//! The primitive registry (docs/37): positioned drawing plugins.
//!
//! A primitive is a named, parameterized drawing whose **positioning** lives
//! here, in the engine — the shell never learns what a fib IS, because the
//! registry decomposes each primitive into the wire geometry the shell
//! already paints (lines and labels). That is the whole architecture:
//!
//! - the **VM** stays dumb: `fib.new(...)` lands a raw `ScriptObject::Fib`
//!   (two anchors and a color) on the drawing heap;
//! - the **registry** owns the decomposition: ratios, label texts, which
//!   lines are solid, how an anchor outside the window behaves;
//! - the **shell** gains nothing: a fib is seven `ScriptDraw::Line`s and
//!   seven `ScriptDraw::Label`s, painted by painters that have existed since
//!   the first script drew a line.
//!
//! Adding a primitive is adding one row to [`PRIMITIVES`] plus its position
//! function — a plugin in the honest sense: one registration point, no
//! scattered branches. What this registry is *not*: dynamically loaded (wasm
//! host plugins are a different security story), and not a shell painter
//! extension point (the shell stays primitive-agnostic on purpose).

use crate::scene::Frame;
use crate::script::ScriptDraw;
use pine_lite::interp::ScriptObject;

/// The placement context every primitive positions itself against: the
/// frame (time/price mapping, owned by the engine) and the window's slot
/// width (bar spacing, for bar-index anchors). The two closures scripts use
/// — bar-index x and price y — are methods here so every primitive shares
/// exactly one mapping, the same one the built-in shapes use.
pub struct Placement<'a> {
    /// The frame the scene was laid out with.
    pub frame: &'a Frame,
    /// The window's bar-slot width in canvas pixels.
    pub slot: f64,
}

impl Placement<'_> {
    /// Canvas x of a bar index — negative anchors count back from the
    /// window's right edge, Pine's `bar_index - n` convention. The frame's
    /// edges are absolute timestamps, so the last bar is the span over one
    /// bar, not `frame.to / bar_nanos` (a date in bar units).
    pub fn bx(&self, bar: f64) -> f64 {
        let last = ((self.frame.to - self.frame.from) / self.frame.bar_nanos.max(1)) as f64;
        let idx = if bar < 0.0 { last + bar } else { bar };
        self.frame.plot.x + self.slot * (idx + 0.5)
    }

    /// Canvas y of a price.
    pub fn py(&self, price: f64) -> f64 {
        crate::scene::price_to_y(price, self.frame.price_min, self.frame.price_max, &self.frame.plot)
    }
}

/// One registry entry: the name the docs and prompts use, and the function
/// that decomposes the raw object into positioned wire geometry.
pub struct Primitive {
    /// The primitive's name, for the docs, the codegen prompt, and refusals.
    pub name: &'static str,
    /// Decompose into positioned wire geometry. An object that cannot be
    /// placed (non-finite anchors) decomposes to nothing.
    pub position: fn(&Placement, &ScriptObject) -> Vec<ScriptDraw>,
}

/// Position a primitive by registry lookup. Unknown entries — none, today,
/// since the VM only constructs objects for registered kinds — position as
/// nothing rather than panicking: a drawing that silently vanished beats a
/// scene that never built.
pub fn position(placement: &Placement, name: &str, object: &ScriptObject) -> Vec<ScriptDraw> {
    PRIMITIVES
        .iter()
        .find(|primitive| primitive.name == name)
        .map(|primitive| (primitive.position)(placement, object))
        .unwrap_or_default()
}

/// The registry. One row per primitive the VM can construct.
pub const PRIMITIVES: &[Primitive] = &[Primitive { name: "fib", position: position_fib }];

/// The retracement ratios and their label texts, in price order from the
/// second anchor toward the first: 0 sits ON the second anchor (the swing's
/// end), 1 on the first.
const FIB_LEVELS: [(f64, &str); 7] = [
    (0.0, "0"),
    (0.236, "23.6"),
    (0.382, "38.2"),
    (0.5, "50"),
    (0.618, "61.8"),
    (0.786, "78.6"),
    (1.0, "100"),
];

/// Decompose a `fib.new` into one horizontal segment per ratio across the
/// anchors' span, plus a right-edge label per level. The 0 and 100 lines —
/// the anchors themselves — are solid; the retracement levels dashed, the
/// TradingView reading.
fn position_fib(placement: &Placement, object: &ScriptObject) -> Vec<ScriptDraw> {
    let ScriptObject::Fib { bar1, price1, bar2, price2, color } = object else {
        return Vec::new();
    };
    if ![bar1, price1, bar2, price2].iter().all(|v| v.is_finite()) {
        return Vec::new();
    }
    let xa = placement.bx(*bar1).min(placement.bx(*bar2));
    let xb = placement.bx(*bar1).max(placement.bx(*bar2));
    FIB_LEVELS
        .iter()
        .flat_map(|(ratio, label)| {
            // Level 0 is the second anchor, level 1 the first: retracement
            // reads backwards from the swing's end.
            let price = price2 - (price2 - price1) * ratio;
            let y = placement.py(price);
            let boundary = *ratio == 0.0 || *ratio == 1.0;
            [
                ScriptDraw::Line {
                    x1: xa,
                    y1: y,
                    x2: xb,
                    y2: y,
                    color: *color,
                    style: if boundary { "solid" } else { "dashed" }.to_string(),
                    width: 1.0,
                },
                ScriptDraw::Label {
                    x: xb + 4.0,
                    y,
                    text: format!("{label}% · {price:.2}"),
                    color: *color,
                },
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Plot;
    use crate::viewport::PriceRange;

    fn placement() -> (Frame, f64) {
        // A 100-bar 5-minute window, prices 90..110, so the mapping math is
        // checkable by hand.
        let frame = Frame {
            plot: Plot { x: 0.0, y: 0.0, w: 1000.0, h: 500.0 },
            from: 1_700_000_000_000_000_000,
            to: 1_700_000_000_000_000_000 + 100 * 300_000_000_000,
            price_min: 90.0,
            price_max: 110.0,
            bar_nanos: 300_000_000_000,
        };
        (frame, 10.0)
    }

    #[test]
    fn a_fib_decomposes_into_seven_positioned_levels_and_labels() {
        let (frame, slot) = placement();
        let placement = Placement { frame: &frame, slot };
        let fib = ScriptObject::Fib {
            bar1: 10.0,
            price1: 100.0,
            bar2: 40.0,
            price2: 120.0,
            color: 0x11_22_33_44,
        };
        let parts = position(&placement, "fib", &fib);
        let lines: Vec<_> = parts
            .iter()
            .filter_map(|p| match p {
                ScriptDraw::Line { x1, y1, x2, y2, style, .. } => Some((*x1, *y1, *x2, *y2, style)),
                _ => None,
            })
            .collect();
        let labels: Vec<_> = parts
            .iter()
            .filter_map(|p| match p {
                ScriptDraw::Label { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 7, "one segment per ratio");
        assert_eq!(labels.len(), 7, "one label per ratio");
        // The span runs between the anchors' bar slots, either order.
        let x1 = slot * 10.5;
        let x2 = slot * 40.5;
        for (lx1, _, lx2, _, _) in &lines {
            assert_eq!((*lx1, *lx2), (x1, x2), "levels span the anchors");
        }
        // Level 0 sits ON the second anchor's price (120): off the top of
        // the axis, which is honest -- the canvas clips, exactly as any
        // out-of-range drawing does. Level 100 sits on the first (100):
        // y = (110 - 100) / 20 * 500 = 250.
        let y100 = lines[6].1;
        assert!((y100 - 250.0).abs() < 1e-9, "the 100 line at the swing start");
        // The 61.8% level: 120 - 20 * 0.618 = 107.64 -> y = (110 - 107.64)/20*500.
        let y618 = lines[4].1;
        let expect618 = (110.0 - (120.0 - 20.0 * 0.618)) / 20.0 * 500.0;
        assert!((y618 - expect618).abs() < 1e-9, "{} vs {expect618}", y618);
        // Boundaries solid, retracement levels dashed.
        assert_eq!(lines[0].4, "solid");
        assert_eq!(lines[6].4, "solid");
        assert_eq!(lines[3].4, "dashed");
        // The label says both the ratio and the price.
        assert_eq!(labels[4], "61.8% · 107.64");
    }

    #[test]
    fn a_fib_with_a_hole_decomposes_to_nothing() {
        let (frame, slot) = placement();
        let placement = Placement { frame: &frame, slot };
        let fib = ScriptObject::Fib {
            bar1: 10.0,
            price1: f64::NAN,
            bar2: 40.0,
            price2: 120.0,
            color: 0,
        };
        assert!(position(&placement, "fib", &fib).is_empty());
        // And a kind the registry does not know positions as nothing, never
        // a panic.
        assert!(position(&placement, "not-a-primitive", &fib).is_empty());
    }
}
