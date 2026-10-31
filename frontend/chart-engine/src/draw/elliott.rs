//! Elliott Wave labels: the multi-pivot wave counts a trader overlays on a
//! chart to mark impulse sequences and corrections.
//!
//! All five kinds in this module share one builder — connect N pivots with
//! segments, label each with its wave number or letter — and differ only in
//! the labels array and the point count the kind declares. Elliott's doctrine
//! is about the *shape of the zigzag*, not about fills or curves, so segments
//! and text are the complete representation.

use crate::drawing::DrawingPart;

use super::Geometry;

/// Five-wave impulse: labels "1", "2", "3", "4", "5" at each pivot.
#[must_use]
pub fn impulse(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    wave(&geometry, &["1", "2", "3", "4", "5"])
}

/// Three-wave correction: labels "A", "B", "C".
#[must_use]
pub fn correction(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    wave(&geometry, &["A", "B", "C"])
}

/// Five-wave triangle: labels "A", "B", "C", "D", "E".
#[must_use]
pub fn triangle(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    wave(&geometry, &["A", "B", "C", "D", "E"])
}

/// Double zigzag (WXY): labels "W", "X", "Y", "X", "Z".
#[must_use]
pub fn double_combo(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    wave(&geometry, &["W", "X", "Y", "X", "Z"])
}

/// Triple zigzag (WXYXZ): labels "W", "X", "Y", "X", "Z", "X", "A".
#[must_use]
pub fn triple_combo(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    wave(&geometry, &["W", "X", "Y", "X", "Z", "X", "A"])
}

/// Shared builder: segments connecting each pivot in order, with the label
/// placed slightly above-left of each vertex so they don't sit on the line.
fn wave(geometry: &Geometry<'_>, labels: &[&str]) -> Vec<DrawingPart> {
    let n = labels.len();
    let mut parts = Vec::with_capacity(n * 2);
    for i in 0..(n - 1) {
        parts.push(DrawingPart::Segment {
            x1: geometry.x(i),
            y1: geometry.y(i),
            x2: geometry.x(i + 1),
            y2: geometry.y(i + 1),
            dashed: false,
        });
    }
    for (i, label) in labels.iter().enumerate() {
        parts.push(DrawingPart::Text {
            x: geometry.x(i) - 4.0,
            y: geometry.y(i) - 10.0,
            text: (*label).to_string(),
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{Point, Stage};
    use crate::drawing::DrawingOptions;

    #[test]
    fn impulse_draws_four_legs_and_five_labels() {
        let stage = Stage::ramp(40, 100.0, 5.0);
        let points: Vec<Point> = (0..5)
            .map(|i| (stage.ms(i * 8), 100.0 + (i as f64) * 30.0))
            .collect();
        let opts = DrawingOptions::default();
        let parts = impulse(stage.geometry(&points, &opts));
        assert_eq!(parts.len(), 9); // 4 segments + 5 texts
    }

    #[test]
    fn correction_draws_two_legs_and_three_labels() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![
            (stage.ms(5), 120.0),
            (stage.ms(15), 80.0),
            (stage.ms(25), 110.0),
        ];
        let opts = DrawingOptions::default();
        let parts = correction(stage.geometry(&points, &opts));
        assert_eq!(parts.len(), 5); // 2 segments + 3 texts
    }

    #[test]
    fn triple_combo_has_six_legs_and_seven_labels() {
        let stage = Stage::ramp(40, 100.0, 5.0);
        let points: Vec<Point> = (0..7)
            .map(|i| (stage.ms(i * 5), 100.0 + (i as f64) * 20.0))
            .collect();
        let opts = DrawingOptions::default();
        let parts = triple_combo(stage.geometry(&points, &opts));
        assert_eq!(parts.len(), 13); // 6 segments + 7 texts
    }
}
