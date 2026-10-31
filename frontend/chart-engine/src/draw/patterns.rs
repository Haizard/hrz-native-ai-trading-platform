//! Harmonic and structural pattern shapes: the multi-leg figures a trader
//! overlays to identify a reversal or continuation setup.
//!
//! Each pattern is defined by its pivot points; the geometry draws legs
//! connecting them and labels the pivots. Ratio annotations (XA, AB, BC, CD
//! percentages) confirm the pattern live as the user drags.
//!
//! Five kinds share this module because all draw "a polyline through N pivots
//! with text at each one", and the only difference is the number of points,
//! the labels used, and whether there is a supplementary structure (neckline,
//! converging edges, ratio badges).

use crate::drawing::DrawingPart;

use super::Geometry;

/// XABCD harmonic pattern: five pivots connected by legs, labeled X-A-B-C-D.
///
/// Each leg shows its ratio relative to the previous (AB/XA, BC/AB, CD/BC) so
/// the trader sees live whether the shape qualifies as a Gartley, Bat, Crab,
/// Butterfly, or Shark.
#[must_use]
pub fn xabcd(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    harmonic(geometry, &["X", "A", "B", "C", "D"])
}

/// ABCD pattern: four pivots, labeled A-B-C-D with leg ratios.
#[must_use]
pub fn abcd(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    harmonic(geometry, &["A", "B", "C", "D"])
}

/// Cypher harmonic: five pivots (X-A-B-C-D), same structure as XABCD with
/// different qualifying ratios.
#[must_use]
pub fn cypher(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    harmonic(geometry, &["X", "A", "B", "C", "D"])
}

/// Head-and-Shoulders: five pivots (LS, head, RS, left neck, right neck).
///
/// The shoulders and head form a polyline; the two neckline points produce a
/// horizontal or sloped line extending both directions, which is the break level.
#[must_use]
pub fn head_shoulders(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let mut parts = Vec::with_capacity(9);
    // Polyline through LS → head → RS → left neck → right neck.
    for i in 0..4 {
        parts.push(DrawingPart::Segment {
            x1: geometry.x(i),
            y1: geometry.y(i),
            x2: geometry.x(i + 1),
            y2: geometry.y(i + 1),
            dashed: false,
        });
    }
    // The neckline (points 3→4) extends to both plot edges as a dashed line.
    let plot = geometry.frame.plot;
    let (nx1, ny1) = (geometry.x(3), geometry.y(3));
    let (nx2, ny2) = (geometry.x(4), geometry.y(4));
    let slope = super::pixel_slope((nx1, ny1), (nx2, ny2));
    let y_at = |x: f64| ny1 + slope * (x - nx1);
    parts.push(DrawingPart::Segment {
        x1: plot.x,
        y1: y_at(plot.x),
        x2: plot.x + plot.w,
        y2: y_at(plot.x + plot.w),
        dashed: true,
    });
    // Labels.
    let labels = ["L.S.", "Head", "R.S.", "Neck L", "Neck R"];
    for (i, label) in labels.iter().enumerate() {
        parts.push(DrawingPart::Text {
            x: geometry.x(i) + 4.0,
            y: geometry.y(i) - 8.0,
            text: (*label).to_string(),
        });
    }
    parts
}

/// Three Drives: six pivots forming three impulse legs with retracements.
#[must_use]
pub fn three_drives(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let mut parts = Vec::with_capacity(11);
    // Five legs connecting six pivots.
    for i in 0..5 {
        parts.push(DrawingPart::Segment {
            x1: geometry.x(i),
            y1: geometry.y(i),
            x2: geometry.x(i + 1),
            y2: geometry.y(i + 1),
            dashed: false,
        });
    }
    // Labels: D1 start, D1 end, D2 start, D2 end, D3 start, D3 end.
    let labels = ["D1↑", "D1↓", "D2↑", "D2↓", "D3↑", "D3↓"];
    for (i, label) in labels.iter().enumerate() {
        parts.push(DrawingPart::Text {
            x: geometry.x(i) + 3.0,
            y: geometry.y(i) - 6.0,
            text: (*label).to_string(),
        });
    }
    // Leg ratios: show the price distance of each drive as text at the midpoint.
    for leg in 0..5 {
        let mid_x = (geometry.x(leg) + geometry.x(leg + 1)) / 2.0;
        let mid_y = (geometry.y(leg) + geometry.y(leg + 1)) / 2.0;
        let price_delta = (geometry.price(leg + 1) - geometry.price(leg)).abs();
        if price_delta > 0.0 {
            parts.push(DrawingPart::Text {
                x: mid_x,
                y: mid_y + 12.0,
                text: format!("{price_delta:.2}"),
            });
        }
    }
    parts
}

/// Triangle pattern: four pivots forming two converging trendlines.
///
/// Points 0→1 are the upper edge (lower highs), points 2→3 are the lower edge
/// (higher lows). Lines extend toward their convergence/apex.
#[must_use]
pub fn triangle_pattern(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let mut parts = Vec::with_capacity(2);
    // Upper trendline (points 0→1, extended right).
    let (ux1, uy1) = (geometry.x(0), geometry.y(0));
    let (ux2, uy2) = (geometry.x(1), geometry.y(1));
    let upper_slope = super::pixel_slope((ux1, uy1), (ux2, uy2));
    let right_x = plot.x + plot.w;
    parts.push(DrawingPart::Segment {
        x1: ux1.min(ux2),
        y1: if ux1 <= ux2 { uy1 } else { uy2 },
        x2: right_x,
        y2: uy1 + upper_slope * (right_x - ux1),
        dashed: false,
    });
    // Lower trendline (points 2→3, extended right).
    let (lx1, ly1) = (geometry.x(2), geometry.y(2));
    let (lx2, ly2) = (geometry.x(3), geometry.y(3));
    let lower_slope = super::pixel_slope((lx1, ly1), (lx2, ly2));
    parts.push(DrawingPart::Segment {
        x1: lx1.min(lx2),
        y1: if lx1 <= lx2 { ly1 } else { ly2 },
        x2: right_x,
        y2: ly1 + lower_slope * (right_x - lx1),
        dashed: false,
    });
    parts
}

/// Shared builder for the harmonic-family patterns (XABCD, ABCD, Cypher).
///
/// Connects the pivots in order with solid segments, labels each one, and
/// annotates the midpoint of each leg with its price distance.
fn harmonic(geometry: Geometry<'_>, labels: &[&str]) -> Vec<DrawingPart> {
    let n = labels.len();
    let mut parts = Vec::with_capacity(n * 3);
    // Legs.
    for i in 0..(n - 1) {
        parts.push(DrawingPart::Segment {
            x1: geometry.x(i),
            y1: geometry.y(i),
            x2: geometry.x(i + 1),
            y2: geometry.y(i + 1),
            dashed: false,
        });
    }
    // Pivot labels.
    for (i, label) in labels.iter().enumerate() {
        parts.push(DrawingPart::Text {
            x: geometry.x(i) + 3.0,
            y: geometry.y(i) - 8.0,
            text: (*label).to_string(),
        });
    }
    // Leg ratio labels at midpoints.
    for i in 1..(n - 1) {
        let prev_leg = (geometry.price(i) - geometry.price(i - 1)).abs();
        let this_leg = (geometry.price(i + 1) - geometry.price(i)).abs();
        let mid_x = (geometry.x(i) + geometry.x(i + 1)) / 2.0;
        let mid_y = (geometry.y(i) + geometry.y(i + 1)) / 2.0;
        if prev_leg > 0.0 {
            let ratio = (this_leg / prev_leg) * 100.0;
            parts.push(DrawingPart::Text {
                x: mid_x,
                y: mid_y + 14.0,
                text: format!("{ratio:.1}%"),
            });
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{Point, Stage};
    use crate::drawing::DrawingOptions;

    #[test]
    fn xabcd_draws_legs_and_labels() {
        let stage = Stage::ramp(40, 100.0, 5.0);
        let points: Vec<Point> = (0..5)
            .map(|i| (stage.ms(i * 8), 100.0 + (i as f64) * 40.0))
            .collect();
        let opts = DrawingOptions::default();
        let parts = xabcd(stage.geometry(&points, &opts));
        // 4 legs + 5 pivot labels + 3 ratio labels = 12 minimum
        assert!(parts.len() >= 12, "xabcd: {}", parts.len());
    }

    #[test]
    fn abcd_draws_three_legs() {
        let stage = Stage::ramp(40, 100.0, 5.0);
        let points: Vec<Point> = (0..4)
            .map(|i| (stage.ms(i * 10), 100.0 + (i as f64) * 50.0))
            .collect();
        let opts = DrawingOptions::default();
        let parts = abcd(stage.geometry(&points, &opts));
        // 3 legs + 4 labels + 2 ratios = 9 minimum
        assert!(parts.len() >= 9, "abcd: {}", parts.len());
    }

    #[test]
    fn head_shoulders_emits_neckline_and_labels() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![
            (stage.ms(5), 110.0),  // left shoulder
            (stage.ms(12), 125.0), // head
            (stage.ms(20), 108.0), // right shoulder
            (stage.ms(10), 100.0), // neck left
            (stage.ms(17), 100.0), // neck right
        ];
        let opts = DrawingOptions::default();
        let parts = head_shoulders(stage.geometry(&points, &opts));
        // 4 legs + 1 neckline + 5 labels = 10
        assert!(parts.len() >= 10, "head_shoulders: {}", parts.len());
    }

    #[test]
    fn three_drives_has_five_legs() {
        let stage = Stage::ramp(40, 100.0, 5.0);
        let points: Vec<Point> = (0..6)
            .map(|i| (stage.ms(i * 6), 100.0 + (i as f64) * 30.0))
            .collect();
        let opts = DrawingOptions::default();
        let parts = three_drives(stage.geometry(&points, &opts));
        // 5 legs + 6 labels + 5 ratio texts = 16
        assert!(parts.len() >= 16, "three_drives: {}", parts.len());
    }

    #[test]
    fn triangle_pattern_emits_two_trendlines() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![
            (stage.ms(5), 130.0),  // upper line start
            (stage.ms(20), 115.0), // upper line end (descending)
            (stage.ms(5), 80.0),   // lower line start
            (stage.ms(20), 95.0),  // lower line end (ascending)
        ];
        let opts = DrawingOptions::default();
        let parts = triangle_pattern(stage.geometry(&points, &opts));
        assert_eq!(parts.len(), 2);
    }
}
