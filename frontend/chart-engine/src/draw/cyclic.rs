//! Cyclic tools: time-based oscillations that project forward from two anchors.
//!
//! All three kinds use the same structure — anchor 0 is the origin, anchor 1
//! sets the period (one full cycle in time) — and differ in what they draw:
//! vertical lines, alternating shaded bands, or a sine curve.
//!
//! The period is always the *time* distance between the two anchors. The cycle
//! repeats forward from anchor 0 (the user's intent: "this oscillation continues
//! into the future"), and the repetition stops at the plot's right edge.

use crate::drawing::DrawingPart;

use super::Geometry;

/// Maximum repetitions before the plot edge, to guard against a zero period.
const MAX_REPS: usize = 200;

/// Vertical lines repeating at the defined interval into the future.
///
/// Each line spans the full plot height, at x positions from the origin + k * period.
#[must_use]
pub fn lines(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x0 = geometry.x(0);
    let x1 = geometry.x(1);
    let period = (x1 - x0).abs();
    if period == 0.0 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    for k in 1..=MAX_REPS {
        let x = x0 + period * k as f64;
        if x > plot.x + plot.w {
            break;
        }
        parts.push(DrawingPart::Segment {
            x1: x,
            y1: plot.y,
            x2: x,
            y2: plot.y + plot.h,
            dashed: false,
        });
    }
    parts
}

/// Alternating shaded bands in time, period set by two anchors.
///
/// Each band is one half-period wide; odd bands are filled rectangles spanning
/// the plot height, even bands are blank. This creates the visual "heartbeat" of
/// a time cycle.
#[must_use]
pub fn bands(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x0 = geometry.x(0);
    let x1 = geometry.x(1);
    let half_period = (x1 - x0).abs() / 2.0;
    if half_period == 0.0 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    for k in 0..MAX_REPS {
        let x_start = x0 + half_period * k as f64;
        let x_end = x_start + half_period;
        if x_start > plot.x + plot.w {
            break;
        }
        // Only draw shaded bands on even-indexed slots.
        if k % 2 == 0 {
            let clamped_end = x_end.min(plot.x + plot.w);
            parts.push(DrawingPart::Rect {
                x: x_start,
                y: plot.y,
                w: clamped_end - x_start,
                h: plot.h,
                filled: true,
            });
        }
    }
    parts
}

/// A sine wave: period from the time distance, amplitude from the price distance.
///
/// The curve is approximated with line segments (one per quarter-period subdivision)
/// so it stays within the DrawingPart vocabulary. The origin is anchor 0; the first
/// anchor is the center of the wave vertically (its price), and the second anchor's
/// price sets the peak amplitude.
#[must_use]
pub fn sine(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x0 = geometry.x(0);
    let x1 = geometry.x(1);
    let period = (x1 - x0).abs();
    if period == 0.0 {
        return Vec::new();
    }
    let amplitude = (geometry.y(1) - geometry.y(0)).abs();
    let center_y = geometry.y(0);
    let end_x = plot.x + plot.w;
    let segments_per_cycle = 16;
    let mut parts = Vec::new();
    let mut prev_x = x0;
    let mut prev_y = center_y;
    let step = period / segments_per_cycle as f64;
    let mut k: usize = 0;
    loop {
        let t = x0 + step * k as f64;
        if t > end_x {
            break;
        }
        let phase = std::f64::consts::TAU * (t - x0) / period;
        let y = center_y - amplitude * phase.sin();
        parts.push(DrawingPart::Segment {
            x1: prev_x,
            y1: prev_y,
            x2: t,
            y2: y,
            dashed: false,
        });
        prev_x = t;
        prev_y = y;
        k += 1;
        if k > MAX_REPS * segments_per_cycle {
            break;
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
    fn cyclic_lines_repeats_forward() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![(stage.ms(0), 100.0), (stage.ms(5), 100.0)];
        let opts = DrawingOptions::default();
        let parts = lines(stage.geometry(&points, &opts));
        assert!(!parts.is_empty());
        // All parts are vertical segments.
        for part in &parts {
            if let DrawingPart::Segment { x1, x2, .. } = part {
                assert_eq!(x1, x2, "each line must be vertical");
            } else {
                panic!("expected Segment");
            }
        }
    }

    #[test]
    fn time_cycles_emits_filled_bands() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![(stage.ms(0), 100.0), (stage.ms(10), 100.0)];
        let opts = DrawingOptions::default();
        let parts = bands(stage.geometry(&points, &opts));
        assert!(!parts.is_empty());
        for part in &parts {
            assert!(
                matches!(part, DrawingPart::Rect { filled: true, .. }),
                "expected filled rects"
            );
        }
    }

    #[test]
    fn sine_curve_emits_segments() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![(stage.ms(0), 100.0), (stage.ms(8), 110.0)];
        let opts = DrawingOptions::default();
        let parts = sine(stage.geometry(&points, &opts));
        assert!(parts.len() > 4, "sine should emit many segments, got {}", parts.len());
    }

    #[test]
    fn zero_period_produces_nothing() {
        let stage = Stage::flat(40, 100.0);
        let points: Vec<Point> = vec![(stage.ms(5), 100.0), (stage.ms(5), 100.0)];
        let opts = DrawingOptions::default();
        assert!(lines(stage.geometry(&points, &opts)).is_empty());
        assert!(bands(stage.geometry(&points, &opts)).is_empty());
        assert!(sine(stage.geometry(&points, &opts)).is_empty());
    }
}
