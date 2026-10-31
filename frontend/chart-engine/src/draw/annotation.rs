//! Annotation marks: text notes, sticky notes, callouts, price tags, flags, icons.
//!
//! Every annotation reads [`Geometry::text`] for its content. A drawing with no
//! label gets a placeholder so the tool is visible even before the user types.

use super::Geometry;
use crate::drawing::DrawingPart;

/// A bare text label at one point.
#[must_use]
pub fn text_note(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.is_empty() {
        return Vec::new();
    }
    let x = geometry.x(0);
    let y = geometry.y(0);
    let label = geometry.text.unwrap_or("Text");
    vec![DrawingPart::Text {
        x,
        y,
        text: label.to_string(),
    }]
}

/// A filled rectangle with text: the sticky note.
#[must_use]
pub fn sticky_note(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.is_empty() {
        return Vec::new();
    }
    let x = geometry.x(0);
    let y = geometry.y(0);
    let label = geometry.text.unwrap_or("Note");
    let w = (label.len() as f64) * 8.0 + 16.0;
    let h = 28.0;
    vec![
        DrawingPart::Rect {
            x: x - 4.0,
            y: y - h / 2.0,
            w,
            h,
            filled: true,
        },
        DrawingPart::Text {
            x: x + 4.0,
            y,
            text: label.to_string(),
        },
    ]
}

/// An arrow from point 0 to point 1, with a label at the arrowhead end.
#[must_use]
pub fn callout(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let label = geometry.text.unwrap_or("Callout");
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    let head_len = 10.0;
    let parts = if len > 1.0 {
        let ux = dx / len;
        let uy = dy / len;
        vec![
            DrawingPart::Segment {
                x1,
                y1,
                x2,
                y2,
                dashed: false,
            },
            DrawingPart::Segment {
                x1: x2,
                y1: y2,
                x2: x2 - head_len * ux + head_len * 0.4 * uy,
                y2: y2 - head_len * uy - head_len * 0.4 * ux,
                dashed: false,
            },
            DrawingPart::Segment {
                x1: x2,
                y1: y2,
                x2: x2 - head_len * ux - head_len * 0.4 * uy,
                y2: y2 - head_len * uy + head_len * 0.4 * ux,
                dashed: false,
            },
            DrawingPart::Text {
                x: x2 + 6.0,
                y: y2 - 6.0,
                text: label.to_string(),
            },
        ]
    } else {
        vec![DrawingPart::Text {
            x: x2 + 6.0,
            y: y2 - 6.0,
            text: label.to_string(),
        }]
    };
    parts
}

/// A labelled horizontal line extending to the right edge, at one price.
#[must_use]
pub fn price_label(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.is_empty() {
        return Vec::new();
    }
    let x1 = geometry.x(0);
    let y = geometry.y(0);
    let x2 = geometry.frame.plot.x + geometry.frame.plot.w;
    let label = geometry.text.unwrap_or("Tag");
    vec![
        DrawingPart::Segment {
            x1,
            y1: y,
            x2,
            y2: y,
            dashed: false,
        },
        DrawingPart::Rect {
            x: x2 - 60.0,
            y: y - 10.0,
            w: 60.0,
            h: 20.0,
            filled: true,
        },
        DrawingPart::Text {
            x: x2 - 54.0,
            y,
            text: label.to_string(),
        },
    ]
}

/// A flag marker: a vertical line with a small triangular pennant.
#[must_use]
pub fn flag_mark(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.is_empty() {
        return Vec::new();
    }
    let x = geometry.x(0);
    let y = geometry.y(0);
    let pole_h = 30.0;
    let flag_w = 14.0;
    let flag_h = 10.0;
    vec![
        DrawingPart::Segment {
            x1: x,
            y1: y,
            x2: x,
            y2: y - pole_h,
            dashed: false,
        },
        DrawingPart::Polygon {
            points: vec![
                (x, y - pole_h),
                (x + flag_w, y - pole_h + flag_h / 2.0),
                (x, y - pole_h + flag_h),
            ],
            filled: true,
        },
    ]
}

/// An icon marker: a circle at one point with a letter inside.
#[must_use]
pub fn icon_mark(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.is_empty() {
        return Vec::new();
    }
    let x = geometry.x(0);
    let y = geometry.y(0);
    let r = 9.0;
    let letter = geometry.text.unwrap_or("i");
    let ch = letter.chars().next().unwrap_or('i');
    vec![
        DrawingPart::Ellipse {
            cx: x,
            cy: y,
            rx: r,
            ry: r,
            half: false,
            filled: true,
        },
        DrawingPart::Text {
            x: x - 3.0,
            y: y + 3.0,
            text: ch.to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{Stage, BAR_NS, T0_NS};
    use crate::drawing::DrawingOptions;

    fn stage() -> Stage {
        Stage::flat(40, 125.0)
    }

    #[test]
    fn text_note_produces_one_text_part() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [(T0_NS as f64 / 1e6, 125.0)];
        let mut g = s.geometry(&pts, &opts);
        g.text = Some("Hello");
        let parts = text_note(g);
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], DrawingPart::Text { text, .. } if text == "Hello"));
    }

    #[test]
    fn sticky_note_produces_rect_and_text() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [(T0_NS as f64 / 1e6, 125.0)];
        let mut g = s.geometry(&pts, &opts);
        g.text = Some("Buy");
        let parts = sticky_note(g);
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], DrawingPart::Rect { filled: true, .. }));
        assert!(matches!(&parts[1], DrawingPart::Text { text, .. } if text == "Buy"));
    }

    #[test]
    fn callout_produces_segments_and_text() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [
            (T0_NS as f64 / 1e6, 120.0),
            ((T0_NS + BAR_NS * 10) as f64 / 1e6, 130.0),
        ];
        let mut g = s.geometry(&pts, &opts);
        g.text = Some("W!");
        let parts = callout(g);
        assert!(parts.len() >= 3);
        assert!(parts.iter().any(|p| matches!(p, DrawingPart::Segment { .. })));
        assert!(matches!(&parts[parts.len()-1], DrawingPart::Text { text, .. } if text == "W!"));
    }

    #[test]
    fn price_label_produces_line_and_tag() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [(T0_NS as f64 / 1e6, 125.0)];
        let mut g = s.geometry(&pts, &opts);
        g.text = Some("125");
        let parts = price_label(g);
        assert_eq!(parts.len(), 3);
        assert!(matches!(parts[0], DrawingPart::Segment { .. }));
        assert!(matches!(parts[1], DrawingPart::Rect { filled: true, .. }));
    }

    #[test]
    fn flag_mark_produces_pole_and_pennant() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [(T0_NS as f64 / 1e6, 125.0)];
        let g = s.geometry(&pts, &opts);
        let parts = flag_mark(g);
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], DrawingPart::Segment { .. }));
        assert!(matches!(&parts[1], DrawingPart::Polygon { filled: true, points } if points.len() == 3));
    }

    #[test]
    fn icon_mark_produces_circle_and_letter() {
        let s = stage();
        let opts = DrawingOptions::default();
        let pts = [(T0_NS as f64 / 1e6, 125.0)];
        let mut g = s.geometry(&pts, &opts);
        g.text = Some("A");
        let parts = icon_mark(g);
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], DrawingPart::Ellipse { filled: true, .. }));
        assert!(matches!(&parts[1], DrawingPart::Text { text, .. } if text == "A"));
    }
}
