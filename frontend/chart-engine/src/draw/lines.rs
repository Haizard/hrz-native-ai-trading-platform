//! The lines group: a straight stroke through two points, clipped however the
//! tool says it should be, and the marks that put a point on the chart.
//!
//! Six kinds share [`stroked`] because what differs between a trendline, a ray,
//! an extended line and an angle is only *where the stroke stops*, and the two
//! degenerate cases where the plot itself is the extent are [`horizontal`] and
//! [`vertical`]. The rest of the group — a half-line, a cross, a labelled span, a
//! fitted line and three arrow forms — each needs its own rule, so each has one.
//!
//! The clipping is parametric rather than edge-tested: the line is
//! `p1 + k * (p2 - p1)`, and each stop is a value of `k` (`0` at the first point,
//! `1` at the second), so the four cases are four `k` ranges computed the same
//! way. That matters because the answer must be the *visible* span: a stroke that
//! runs past the plot is clipped by the canvas and its dashed pattern restarts,
//! and one that stops at the second anchor is a trendline wearing another name.
//!
//! ## Which of these read the market
//!
//! [`regression`] and [`info`] count or fit the bars between their points. That is
//! the only place in this module that does, and the reason they are lines tools
//! rather than indicators: the *shape* is the user's two points, and the market
//! only chooses where the straight line through them lands.

use crate::drawing::{DrawingPart, Extend};

use super::Geometry;

/// The pixel length of an arrow's head.
///
/// TradingView's arrow marks say plainly that *"size … is fixed and cannot be
/// changed"*, and its arrow keeps a constant head while the shaft stretches. A
/// head proportional to the span would be a triangle that happens to have two
/// points; the mark is a pointer at something, and a pointer stays the size of a
/// pointer however far it reaches.
const HEAD_LENGTH: f64 = 12.0;
/// Half the width of an arrow's head, in pixels.
const HEAD_WIDTH: f64 = 5.0;
/// How far an arrow mark's body runs from its tip, in pixels.
const MARK_HEIGHT: f64 = 8.0;

/// The stroke between the two points, extended as `default` or as the user asked.
///
/// The kind's own default is a parameter because each line tool's reference
/// states a different one: TradingView's trendline stops at its anchors, its ray
/// "extends to the right of the second point", and its extended line "passes
/// through both points and extends infinitely in both directions". The
/// [`Extend`] option then overrides it -- which is the *Extend Left/Right*
/// checkbox on all three pages -- and an absent option means "the tool's own".
#[must_use]
pub fn stroked(geometry: Geometry<'_>, default: Extend) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let extent = geometry.options.extend.unwrap_or(default);
    let (from, to) = match extent {
        Extend::None => ((x1, y1), (x2, y2)),
        Extend::Right => ((x1, y1), forward(geometry, x1, y1, x2 - x1, y2 - y1)),
        Extend::Left => (forward(geometry, x2, y2, x1 - x2, y1 - y2), (x2, y2)),
        Extend::Both => both(geometry, x1, y1, x2 - x1, y2 - y1),
    };
    vec![DrawingPart::Segment {
        x1: from.0,
        y1: from.1,
        x2: to.0,
        y2: to.1,
        dashed: false,
    }]
}

/// A horizontal level across the whole plot, at the first point's price.
///
/// Across the plot rather than between two anchors because a horizontal line *is*
/// a price: one that stopped at wherever the user happened to click would be a
/// segment with an unfinished claim. The price is written at the left end for the
/// same reason the axis is there -- to be read without a crosshair.
#[must_use]
pub fn horizontal(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let y = geometry.y(0);
    let price = geometry.price(0);
    vec![
        DrawingPart::Segment {
            x1: plot.x,
            y1: y,
            x2: plot.x + plot.w,
            y2: y,
            dashed: false,
        },
        DrawingPart::Text {
            x: plot.x + 4.0,
            y: y - 4.0,
            text: format!("{price:.2}"),
        },
    ]
}

/// A vertical line down the whole plot, at the first point's time.
///
/// The mirror of [`horizontal`], and the same argument for spanning the plot.
#[must_use]
pub fn vertical(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x = geometry.x(0);
    vec![DrawingPart::Segment {
        x1: x,
        y1: plot.y,
        x2: x,
        y2: plot.y + plot.h,
        dashed: false,
    }]
}

/// A horizontal level from its anchor to the plot's right edge.
///
/// The half of [`horizontal`] that marks a breakout: the price is the claim, and
/// the chart to the *left* of the click is evidence about how the price got there
/// rather than part of the level. TradingView's horizontal ray runs left to right
/// from its anchor for exactly that reason.
///
/// `extend` chooses which way it runs, and [`Extend::None`] runs it right: a ray
/// with no direction is a dot, and a tool named for a direction that defaults to
/// drawing nothing is a tool that appears broken.
#[must_use]
pub fn horizontal_ray(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let (x, y) = (geometry.x(0), geometry.y(0));
    let (from, to) = match geometry.options.extend {
        Some(Extend::Left) => (plot.x, x),
        Some(Extend::Both) => (plot.x, plot.x + plot.w),
        _ => (x, plot.x + plot.w),
    };
    vec![
        DrawingPart::Segment {
            x1: from,
            y1: y,
            x2: to,
            y2: y,
            dashed: false,
        },
        DrawingPart::Text {
            x: x + 4.0,
            y: y - 4.0,
            text: format!("{:.2}", geometry.price(0)),
        },
    ]
}

/// The two full-plot lines that cross at one anchor.
///
/// A crosshair the user placed and that stays: the price *and* the time of one
/// moment, marked without two tools. Both lines span the plot because both are
/// claims about the whole window — "this price" and "this bar" — and a quadrant
/// cut short of an edge is no longer a quadrant.
#[must_use]
pub fn cross(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let (x, y) = (geometry.x(0), geometry.y(0));
    vec![
        DrawingPart::Segment {
            x1: plot.x,
            y1: y,
            x2: plot.x + plot.w,
            y2: y,
            dashed: false,
        },
        DrawingPart::Segment {
            x1: x,
            y1: plot.y,
            x2: x,
            y2: plot.y + plot.h,
            dashed: false,
        },
    ]
}

/// The segment between two anchors, with both prices and the bars between.
///
/// The shape is a trendline; the tool is the reading. A trader dragging between
/// two pivots wants the *gap* — from what price to what price, over how many
/// bars — which is why the count comes from the traded series rather than from
/// the pixel distance divided by a bar width the user never set.
#[must_use]
pub fn info(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let bars = geometry.bars_between(geometry.points[0], geometry.points[1]).len();
    vec![
        DrawingPart::Segment {
            x1,
            y1,
            x2,
            y2,
            dashed: false,
        },
        DrawingPart::Text {
            x: x1 + 4.0,
            y: y1 - 4.0,
            text: format!("{:.2}", geometry.price(0)),
        },
        DrawingPart::Text {
            x: x2 + 4.0,
            y: y2 - 4.0,
            text: format!("{:.2}", geometry.price(1)),
        },
        DrawingPart::Text {
            x: (x1 + x2) / 2.0,
            y: (y1 + y2) / 2.0 + 12.0,
            text: format!("{bars} bars"),
        },
    ]
}

/// The least-squares line through the closes of the bars between two anchors.
///
/// Fitted against the **bar index**, not the timestamp: the two are the same
/// thing on a fixed timeframe and differ only in magnitude, and a timestamp is
/// past 10^12, so `n * Σt² − (Σt)²` subtracts two numbers of ~10^26 for a result
/// of ~10^2. That is not a precision problem to tune, it is the answer lost. The
/// index is small, exact, and what the reference fits over.
///
/// Pearson's *R* is written beside it, because a trend line drawn over a range
/// says "this is the direction" and the only honest completion of that sentence
/// is how much of the range's movement the direction explains.
#[must_use]
pub fn regression(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let bars = geometry.bars_between(
        geometry.point(0).unwrap_or_default(),
        geometry.point(1).unwrap_or_default(),
    );
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    if bars.len() < 2 {
        // Nothing to fit. The line through the two anchors is then the only
        // straight thing available over the range, and it is drawn without an
        // *R*, which would be a number invented from empty data.
        return vec![DrawingPart::Segment {
            x1,
            y1,
            x2,
            y2,
            dashed: false,
        }];
    }

    let n = bars.len() as f64;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = bars.iter().map(|bar| bar.close).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut syy = 0.0;
    for (index, bar) in bars.iter().enumerate() {
        let dx = index as f64 - mean_x;
        let dy = bar.close - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // `sxx` is zero only when the range holds one bar, and `len() < 2` returned
    // above; `syy` is zero when every close is identical, which is a flat line
    // with no correlation to report.
    let slope = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    let r = if syy == 0.0 || sxx == 0.0 {
        f64::NAN
    } else {
        sxy / (sxx * syy).sqrt()
    };

    let first = super::ms(&bars[0]);
    let last = super::ms(&bars[bars.len() - 1]);
    let start = mean_y - slope * mean_x;
    let mid = start + slope * (n - 1.0) / 2.0;
    let mut parts = vec![DrawingPart::Segment {
        x1: geometry.frame.x_at_ms(first),
        y1: geometry.frame.y_at(start),
        x2: geometry.frame.x_at_ms(last),
        y2: geometry.frame.y_at(start + slope * (n - 1.0)),
        dashed: false,
    }];
    // The *R* is written only when there is one. A flat range — every close
    // identical — has a direction (horizontal) and no correlation, and printing
    // `R = 0.00` there would be a number the arithmetic did not produce.
    if r.is_finite() {
        parts.push(DrawingPart::Text {
            x: geometry.frame.x_at_ms((first + last) / 2.0),
            y: geometry.frame.y_at(mid) - 8.0,
            text: format!("R = {r:.2}"),
        });
    }
    parts
}

/// The segment between two anchors with an arrowhead on the second.
///
/// The head sits at the anchor the user ended on — which is why the shaft is not
/// run through [`stroked`]: a line extended to the plot's edge with a head parked
/// at the anchor points somewhere other than where it ends.
#[must_use]
pub fn arrow(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let mut parts = vec![DrawingPart::Segment {
        x1,
        y1,
        x2,
        y2,
        dashed: false,
    }];
    if let Some(head) = head(x1, y1, x2, y2) {
        parts.push(head);
    }
    parts
}

/// A fixed-size arrow mark at one point, up or down.
///
/// The tip *is* the anchor: a mark that pointed near the price the trader clicked
/// would be a decoration rather than a claim about that bar. The body runs away
/// from the tip, so an up mark sits under its price and a down mark over it,
/// which is where both references put them.
#[must_use]
pub fn mark(geometry: Geometry<'_>, up: bool) -> Vec<DrawingPart> {
    let (x, y) = (geometry.x(0), geometry.y(0));
    let body = if up { MARK_HEIGHT } else { -MARK_HEIGHT };
    vec![DrawingPart::Polygon {
        points: vec![(x, y), (x - HEAD_WIDTH, y + body), (x + HEAD_WIDTH, y + body)],
        filled: true,
    }]
}

/// The triangular head at the end of the shaft from `(x1, y1)` to `(x2, y2)`:
/// apex on the end point, wings `HEAD_LENGTH` back along the shaft and
/// `HEAD_WIDTH` to either side of it.
///
/// `None` for a zero-length shaft, which `scene::place` already refuses for a
/// two-point kind — but the arrowhead is the one part of this group whose
/// direction comes from a subtraction, so it says what it does with a degenerate
/// pair rather than handing the canvas a `NaN` polygon.
fn head(x1: f64, y1: f64, x2: f64, y2: f64) -> Option<DrawingPart> {
    let (dx, dy) = (x2 - x1, y2 - y1);
    let length = (dx * dx + dy * dy).sqrt();
    if length == 0.0 || !length.is_finite() {
        return None;
    }
    let (ux, uy) = (dx / length, dy / length);
    let back = length.min(HEAD_LENGTH);
    Some(DrawingPart::Polygon {
        points: vec![
            (x2, y2),
            (x2 - ux * back - uy * HEAD_WIDTH, y2 - uy * back + ux * HEAD_WIDTH),
            (x2 - ux * back + uy * HEAD_WIDTH, y2 - uy * back - ux * HEAD_WIDTH),
        ],
        filled: true,
    })
}

/// How far a ray from `(x, y)` travelling `(dx, dy)` runs before it leaves the
/// plot -- the parametric `k` at which it stops, clamped to the axis that gets
/// there first.
fn forward(geometry: Geometry<'_>, x: f64, y: f64, dx: f64, dy: f64) -> (f64, f64) {
    let plot = geometry.frame.plot;
    let mut k = f64::INFINITY;
    if dx != 0.0 {
        let edge = if dx > 0.0 {
            plot.x + plot.w
        } else {
            plot.x
        };
        k = k.min((edge - x) / dx);
    }
    if dy != 0.0 {
        let edge = if dy > 0.0 {
            plot.y + plot.h
        } else {
            plot.y
        };
        k = k.min((edge - y) / dy);
    }
    if !k.is_finite() {
        // Should be unreachable: both deltas zero is the same-point case, which
        // `scene::place` refused. Guarded anyway, because a `NaN` from `0/0` is
        // a canvas that silently drops the shape.
        k = 1.0;
    }
    (x + dx * k, y + dy * k)
}

/// The visible span of the infinite line through `(x, y)` with direction
/// `(dx, dy)`: the two parametric crossings of the plot rectangle, low first.
fn both(geometry: Geometry<'_>, x: f64, y: f64, dx: f64, dy: f64) -> ((f64, f64), (f64, f64)) {
    let plot = geometry.frame.plot;
    let mut k_low = f64::NEG_INFINITY;
    let mut k_high = f64::INFINITY;
    if dx != 0.0 {
        let left = (plot.x - x) / dx;
        let right = (plot.x + plot.w - x) / dx;
        k_low = k_low.max(left.min(right));
        k_high = k_high.min(left.max(right));
    }
    if dy != 0.0 {
        let top = (plot.y - y) / dy;
        let bottom = (plot.y + plot.h - y) / dy;
        k_low = k_low.max(top.min(bottom));
        k_high = k_high.min(top.max(bottom));
    }
    if !(k_low.is_finite() && k_high.is_finite()) {
        k_low = 0.0;
        k_high = 1.0;
    }
    (
        (x + dx * k_low, y + dy * k_low),
        (x + dx * k_high, y + dy * k_high),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::DrawingOptions;
    use crate::draw::Stage;
    use DrawingPart::{Polygon, Segment, Text};

    fn segments(parts: &[DrawingPart]) -> Vec<(f64, f64, f64, f64)> {
        parts
            .iter()
            .filter_map(|part| match part {
                Segment { x1, y1, x2, y2, .. } => Some((*x1, *y1, *x2, *y2)),
                _ => None,
            })
            .collect()
    }

    fn texts(parts: &[DrawingPart]) -> Vec<String> {
        parts
            .iter()
            .filter_map(|part| match part {
                Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn polygons(parts: &[DrawingPart]) -> Vec<Vec<(f64, f64)>> {
        parts
            .iter()
            .filter_map(|part| match part {
                Polygon { points, .. } => Some(points.clone()),
                _ => None,
            })
            .collect()
    }

    /// The right-hand plot edge of every fixture: 1000 logical pixels wide.
    const RIGHT: f64 = 1_000.0;
    /// And the bottom.
    const BOTTOM: f64 = 500.0;

    #[test]
    fn a_horizontal_ray_runs_from_its_anchor_to_the_right_edge_and_no_further_left() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let at = (stage.ms(40), 140.0);
        let options = DrawingOptions::default();
        let parts = horizontal_ray(stage.geometry(&[at], &options));

        // One segment, starting where the user clicked and stopping at the
        // window's right edge -- the claim is about what happens *after* this
        // price was set, so the left half of the plot is not part of it.
        assert_eq!(segments(&parts).len(), 1, "{parts:?}");
        let (x1, y1, x2, y2) = segments(&parts)[0];
        assert_eq!((x1, x2, y1, y2), (stage.frame.x_at_ms(at.0), RIGHT, y1, y1));
        assert!(x1 > 0.0, "the ray reached the left edge: {x1}");
        assert!(
            texts(&parts).iter().any(|text| text == "140.00"),
            "the level is not labelled: {parts:?}"
        );
    }

    #[test]
    fn a_horizontal_ray_told_to_extend_left_reaches_the_left_edge_instead() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let at = (stage.ms(40), 140.0);
        let options = DrawingOptions {
            extend: Some(Extend::Left),
            ..DrawingOptions::default()
        };
        let (x1, _, x2, _) = segments(&horizontal_ray(
            stage.geometry(&[at], &options),
        ))[0];
        assert_eq!((x1, x2), (0.0, stage.frame.x_at_ms(at.0)));
    }

    #[test]
    fn a_cross_is_the_two_full_plot_lines_through_one_point() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let at = (stage.ms(25), 125.0);
        let parts = cross(stage.geometry(&[at], &DrawingOptions::default()));

        // Two, and each spans the whole plot: the point of one click is to cut
        // the chart into quadrants, and a line that stops short is half a cut.
        let lines = segments(&parts);
        assert_eq!(lines.len(), 2, "{parts:?}");
        let (x, y) = (stage.frame.x_at_ms(at.0), stage.frame.y_at(at.1));
        assert!(
            lines.contains(&(0.0, y, RIGHT, y)),
            "no full-width price line at {y}: {lines:?}"
        );
        assert!(
            lines.contains(&(x, 0.0, x, BOTTOM)),
            "no full-height time line at {x}: {lines:?}"
        );
    }

    #[test]
    fn an_info_line_writes_both_prices_and_the_bars_between_them() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let from = (stage.ms(10), 110.0);
        let to = (stage.ms(20), 120.0);
        let parts = info(stage.geometry(&[from, to], &DrawingOptions::default()));

        let lines = segments(&parts);
        assert_eq!(
            lines[0],
            (
                stage.frame.x_at_ms(from.0),
                stage.frame.y_at(110.0),
                stage.frame.x_at_ms(to.0),
                stage.frame.y_at(120.0),
            ),
            "{lines:?}"
        );
        // Counted off the traded series, inclusive of both ends: 11 bars is the
        // answer a trader reads off the axis, and a pixel estimate is not.
        let words = texts(&parts);
        assert!(words.contains(&"110.00".to_owned()) && words.contains(&"120.00".to_owned()), "{words:?}");
        assert!(words.contains(&"11 bars".to_owned()), "{words:?}");
    }

    #[test]
    fn an_info_line_counts_the_bars_whichever_way_it_was_dragged() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let forward = info(
            stage.geometry(
                &[(stage.ms(10), 110.0), (stage.ms(20), 120.0)],
                &DrawingOptions::default(),
            ),
        );
        let back = info(
            stage.geometry(
                &[(stage.ms(20), 120.0), (stage.ms(10), 110.0)],
                &DrawingOptions::default(),
            ),
        );
        let count = |parts: &[DrawingPart]| {
            texts(parts)
                .into_iter()
                .find(|text| text.ends_with("bars"))
        };
        assert_eq!(count(&forward).as_deref(), Some("11 bars"), "a range of time has no direction");
        assert_eq!(count(&back).as_deref(), Some("11 bars"), "a range of time has no direction");
    }

    #[test]
    fn a_regression_line_is_the_fit_over_the_bars_its_anchors_span() {
        // A series that is a ramp only *inside* the anchors: bars 10 to 30 rise
        // one price a bar and the rest sit far above. A fit over the whole
        // series would slope elsewhere and read an R below 1, so this asserts
        // both that the range is the anchors' and that the line is the fit.
        let stage = Stage::over(51, |i| if (10..=30).contains(&i) { 100.0 + i as f64 } else { 900.0 }, 100.0, 900.0);
        let points = [(stage.ms(10), 500.0), (stage.ms(30), 500.0)];
        let parts = regression(stage.geometry(&points, &DrawingOptions::default()));

        let (x1, y1, x2, y2) = segments(&parts)[0];
        assert_eq!(
            (x1, y1, x2, y2),
            (
                stage.frame.x_at_ms(stage.ms(10)),
                stage.frame.y_at(110.0),
                stage.frame.x_at_ms(stage.ms(30)),
                stage.frame.y_at(130.0),
            ),
            "the fitted line does not run through the range's own closes: {x1} {y1} {x2} {y2}"
        );
        assert!(
            texts(&parts).contains(&"R = 1.00".to_owned()),
            "a perfect ramp with no correlation written: {parts:?}"
        );
    }

    #[test]
    fn a_regression_over_no_bars_is_a_line_and_no_invented_readout() {
        let stage = Stage::ramp(20, 100.0, 1.0);
        // Both anchors before bar zero, so nothing traded between them.
        let bar_ms = 300_000.0;
        let before = stage.ms(0) - 10.0 * bar_ms;
        let points = [(before - bar_ms, 100.0), (before, 105.0)];
        let parts = regression(stage.geometry(&points, &DrawingOptions::default()));
        assert_eq!(segments(&parts).len(), 1, "{parts:?}");
        assert!(texts(&parts).is_empty(), "an R from no data: {parts:?}");
    }

    #[test]
    fn a_flat_range_has_no_correlation_to_report() {
        let stage = Stage::flat(30, 500.0);
        let points = [(stage.ms(0), 500.0), (stage.ms(29), 500.0)];
        let parts = regression(stage.geometry(&points, &DrawingOptions::default()));
        // The line is still true -- every close *is* the fit -- but no number
        // exists that says how well it explains the movement, because there was
        // none. `NaN` would reach the canvas as the text "NaN".
        assert_eq!(segments(&parts).len(), 1, "{parts:?}");
        assert!(texts(&parts).is_empty(), "{parts:?}");
    }

    #[test]
    fn an_arrow_head_apex_sits_on_the_second_point() {
        let stage = Stage::ramp(100, 100.0, 1.0);
        let from = (stage.ms(10), 110.0);
        let to = (stage.ms(60), 160.0);
        let parts = arrow(stage.geometry(&[from, to], &DrawingOptions::default()));

        assert_eq!(segments(&parts).len(), 1, "{parts:?}");
        let head = &polygons(&parts)[0];
        let tip = (stage.frame.x_at_ms(to.0), stage.frame.y_at(to.1));
        assert_eq!(head[0], tip, "the head points somewhere the shaft does not end");
        assert_eq!(head.len(), 3, "a triangular head: {head:?}");
    }

    #[test]
    fn an_arrow_mark_is_the_same_pixels_whatever_the_price_scale() {
        // Two windows, one 99 prices wide and one 9,999 -- so the anchor lands on
        // the same y in both, and a mark that scaled its glyph to the price range
        // would come out two different sizes. The mark is a pointer, not a shape.
        let narrow = Stage::ramp(100, 100.0, 1.0);
        let wide = Stage::ramp(100, 100.0, 100.0);
        let at = |stage: &Stage| (stage.ms(50), stage.frame.price_min + (stage.frame.price_max - stage.frame.price_min) / 2.0);

        for up in [true, false] {
            let small = mark(narrow.geometry(&[at(&narrow)], &DrawingOptions::default()), up);
            let large = mark(wide.geometry(&[at(&wide)], &DrawingOptions::default()), up);
            assert_eq!(
                polygons(&small),
                polygons(&large),
                "a mark that grows with the price scale (up: {up})"
            );
            let (x, y) = (narrow.frame.x_at_ms(at(&narrow).0), narrow.frame.y_at(at(&narrow).1));
            let body = &polygons(&small)[0];
            assert_eq!(body[0], (x, y), "the tip is not the anchor");
            let height = if up { y + MARK_HEIGHT } else { y - MARK_HEIGHT };
            assert_eq!(body[1], (x - HEAD_WIDTH, height));
            assert_eq!(body[2], (x + HEAD_WIDTH, height));
        }
    }
}
