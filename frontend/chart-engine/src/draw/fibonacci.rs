//! The fibonacci group: one ladder, drawn on either axis and in either shape.
//!
//! Every kind here reads the same two things — a ratio set and a unit — and differs
//! in what the unit is measured along and how the levels are stroked. A retracement
//! is the ladder laid across a swing's price; a time zone is the same ladder laid
//! across its *duration*; a fan, an arc, a circle, a spiral and a wedge are the
//! ladder drawn radially from one of the anchors. That is why they are one module:
//! the arithmetic is shared, and a group whose tools each re-derived their own
//! ratios ends up with four of them disagreeing about what 61.8 means.
//!
//! ## The ladder is a profile, not a fixed list
//!
//! [`crate::drawing::LevelProfile`] picks the ratio set, and every builder here
//! takes its default from the reference it copies: a retracement reads the classic
//! ladder, an extension the projection set, the time tools the time set, a fan the
//! three speed-resistance levels. Pass 5 of the concept audit found this grid drawn
//! as anonymous dashes, so each level is labelled with its ratio *and* its price.
//!
//! ## Price on the y axis, time on the x
//!
//! [`trend_time`] and [`time_zones`] put their levels on the **time** axis, which is
//! the whole point of them and the thing a Fibonacci tool most often gets wrong: a
//! ratio of a *duration* drawn as a price is a number about the wrong variable.
//! Their tests assert the axis explicitly.
//!
//! ## What the options do here
//!
//! [`crate::drawing::DrawingOptions::reverse`] swaps which anchor is the swing's
//! zero, and for the radial tools it flips the direction the ladder runs.
//! [`crate::drawing::Extend`] moves the ends of the horizontal levels and the fans;
//! the vertical tools ignore it because a vertical line already spans the plot, and
//! [`crate::drawing::LevelProfile`] changes the ratios.

use crate::drawing::{DrawingPart, Extend, LevelProfile};

use super::{pixel_slope, ray_to, Geometry, Point};

/// The golden ratio, the spiral's growth per quarter turn.
///
/// Written out rather than derived from a ladder so the spiral's geometry says the
/// one thing it claims: a quarter turn multiplies the radius by φ.
const PHI: f64 = 1.618_033_988_749_895;

/// The three levels the speed-resistance tools divide a swing by.
const FAN_LEVELS: [(f64, &str); 3] = [(0.382, "38.2"), (0.5, "50"), (0.618, "61.8")];

/// The pitchfan's coefficients: the fan's three, plus two projections past the
/// swing's own box, which is the only reason the two tools are not one.
const PITCHFAN_LEVELS: [(f64, &str); 5] = [
    (0.382, "38.2"),
    (0.5, "50"),
    (0.618, "61.8"),
    (1.272, "127.2"),
    (1.618, "161.8"),
];

/// The circles' eleven radii: a tenth of the swing out to the swing itself, and
/// then the 161.8 projection, which is the level the reference's own list ends on.
const CIRCLE_LEVELS: [(f64, &str); 11] = [
    (0.1, "10"),
    (0.2, "20"),
    (0.3, "30"),
    (0.4, "40"),
    (0.5, "50"),
    (0.6, "60"),
    (0.7, "70"),
    (0.8, "80"),
    (0.9, "90"),
    (1.0, "100"),
    (1.618, "161.8"),
];

/// How many quarter-turns a spiral runs before it is off any chart worth reading.
const SPIRAL_QUARTERS: u32 = 6;
/// Samples per quarter turn. A logarithmic spiral is smooth to the eye at far fewer
/// points than this, but each sample is one `lineTo`, so the cost is bounded by the
/// tool rather than by the window.
const SPIRAL_SAMPLES: u32 = 12;

/// The retracement ladder, laid across the swing's price.
#[must_use]
pub fn retracement(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    levels(geometry, ladder(geometry, LevelProfile::Classic), |_| false)
}

/// The projection ladder past the swing, with the ray that says which way.
#[must_use]
pub fn extension(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let set = ladder(geometry, LevelProfile::Extension);
    let mut parts = levels(geometry, set, |ratio| ratio > 1.0);
    if geometry.points.len() >= 2 {
        // The projection ray: out from the 100% level's edge in the direction the
        // swing implies, so the direction is on the chart and not only in the
        // level prices. A zero-length swing was refused upstream by the
        // same-point rule, so `signum` is never 0 here.
        let (from, to) = swing(geometry);
        let direction = (to - from).signum();
        let (left, right) = extent(geometry);
        let edge = if direction >= 0.0 { right } else { left };
        let y = geometry.frame.y_at(to);
        parts.push(DrawingPart::Segment {
            x1: edge,
            y1: y,
            x2: edge + direction * (right - left) * 0.35,
            y2: y,
            dashed: true,
        });
    }
    parts
}

/// The trend-based extension: targets projected from the *pullback*, along the
/// impulse's own price **and** time slope.
///
/// Each level is drawn from the time it is due rather than from the swing, so the
/// ladder walks forward with the trend. That is what separates this from
/// [`extension`], and why it takes a third anchor: a target without a *when* is a
/// price the market can ignore indefinitely.
#[must_use]
pub fn trend_extension(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((impulse, pullback)) = impulse(geometry) else {
        return Vec::new();
    };
    let plot = geometry.frame.plot;
    let mut parts = Vec::new();
    for (ratio, label) in ladder(geometry, LevelProfile::Extension) {
        let price = pullback.1 + impulse.price * ratio;
        let due = pullback.0 + impulse.duration * ratio;
        let y = geometry.frame.y_at(price);
        let start = geometry.frame.x_at_ms(due).clamp(plot.x, plot.x + plot.w);
        let right = plot.x + plot.w;
        if start >= right {
            continue;
        }
        // A projection is a projection at every ratio, so the whole ladder is dashed.
        parts.push(DrawingPart::Segment {
            x1: start,
            y1: y,
            x2: right,
            y2: y,
            dashed: true,
        });
        parts.push(DrawingPart::Text {
            x: start + 4.0,
            y: y - 3.0,
            text: format!("{label}%  {price:.2}"),
        });
    }
    parts
}

/// The trend-based fib **time**: the same three anchors, with the ratios read as
/// durations and drawn as vertical lines.
#[must_use]
pub fn trend_time(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((impulse, pullback)) = impulse(geometry) else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    for (ratio, label) in ladder(geometry, LevelProfile::Time) {
        let due = pullback.0 + impulse.duration * ratio;
        parts.extend(meridian(geometry, due, label));
    }
    parts
}

/// Vertical lines at the ladder's ratios of the anchors' time distance, counted
/// from the first anchor.
///
/// The unit is the distance between the two clicks, so the tool is a statement
/// about *how long* a move takes, and the levels run off the end of the chart
/// rather than stopping at the second click: a forecast that stops where the
/// evidence stops is not a forecast.
#[must_use]
pub fn time_zones(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let unit = (geometry.points[1].0 - geometry.points[0].0).abs();
    let mut parts = Vec::new();
    for (ratio, label) in ladder(geometry, LevelProfile::Time) {
        parts.extend(meridian(
            geometry,
            geometry.points[0].0 + unit * ratio,
            label,
        ));
    }
    parts
}

/// The line through the first two anchors, its parallel through the third, and the
/// ladder between them.
///
/// Same construction as [`super::channels::parallel`] — the third click sets the
/// band's width at its own time, so the edge is the line *through* it — with the
/// ratios drawn inside the band instead of only its two edges.
#[must_use]
pub fn channel(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 3 {
        return Vec::new();
    }
    let (first, second, third) = (geometry.points[0], geometry.points[1], geometry.points[2]);
    let plot = geometry.frame.plot;
    let origin = pixel(geometry, first);
    let target = pixel(geometry, second);
    let anchor = pixel(geometry, third);
    let slope = pixel_slope(origin, target);
    // The band's full width, measured at the third click's own time.
    let width = anchor.1 - (origin.1 + slope * (anchor.0 - origin.0));

    let (left, right) = (plot.x, plot.x + plot.w);
    let mut parts = Vec::new();
    for (ratio, label) in ladder(geometry, LevelProfile::Classic) {
        let offset = width * ratio;
        let at = |x: f64| origin.1 + slope * (x - origin.0) + offset;
        let dashed = *ratio > 0.0 && *ratio < 1.0;
        parts.push(DrawingPart::Segment {
            x1: left,
            y1: at(left),
            x2: right,
            y2: at(right),
            dashed,
        });
        parts.push(DrawingPart::Text {
            x: right - 64.0,
            y: at(right) - 3.0,
            text: format!("{label}%"),
        });
    }
    parts
}

/// The speed-resistance fan: lines from the swing's origin through the ladder's
/// divisions of its price range.
#[must_use]
pub fn fan(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    rays(geometry, &FAN_LEVELS)
}

/// The pitchfan: the same rays, at the ladder's *coefficients* of the slope, which
/// is the same arithmetic read as a gradient — and the reason its set keeps going
/// past the swing's box while [`fan`] stops at it.
#[must_use]
pub fn pitchfan(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    rays(geometry, &PITCHFAN_LEVELS)
}

/// The arcs and the circles, both measured off the swing's price distance.
///
/// Two families, one builder, because the reference draws both from the same
/// measurement and differs in the radii and in what it strokes: the arcs at the
/// ladder's divisions around *both* ends of the swing, the circles at eleven radii
/// around the pivot. A radius is a *price distance* read in pixels, which is why
/// these two follow the vertical scale rather than the time axis.
#[must_use]
pub fn arcs(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    rings(
        geometry,
        ladder(geometry, LevelProfile::Classic),
        Radial::Arcs,
    )
}

/// The circles' own ladder: eleven radii, the swing's distance as the tenth.
#[must_use]
pub fn circles(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    rings(geometry, &CIRCLE_LEVELS, Radial::Circles)
}

/// How the radial ladder is stroked, which is the whole difference between the two
/// tools that share it.
///
/// An *arc* is half a circle and answers at both pivots; a *circle* is whole and
/// answers at the one the trader clicked first. A kind named for one shape and
/// drawn as the other is the defect the audit keeps finding, in a tool that would
/// have passed a test that only counted parts.
enum Radial {
    /// Half a circle, centred on each end of the swing.
    Arcs,
    /// A whole circle, centred on the pivot.
    Circles,
}

fn rings(geometry: Geometry<'_>, set: &[(f64, &str)], form: Radial) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let first = pixel(geometry, geometry.points[0]);
    let second = pixel(geometry, geometry.points[1]);
    let unit = (second.1 - first.1).abs();
    let (centres, half) = match form {
        Radial::Arcs => (vec![first, second], true),
        Radial::Circles => (vec![first], false),
    };
    let mut parts = Vec::with_capacity(set.len() * centres.len());
    for (ratio, _) in set {
        let radius = unit * ratio;
        if radius <= 0.0 {
            continue;
        }
        for centre in &centres {
            parts.push(DrawingPart::Ellipse {
                cx: centre.0,
                cy: centre.1,
                rx: radius,
                ry: radius,
                half,
                filled: false,
            });
        }
    }
    parts
}

/// A logarithmic spiral from the first anchor, its radius multiplied by φ every
/// quarter turn.
///
/// The initial radius is the price distance to the second anchor, so the spiral is
/// the same measurement as [`circles`] unwound around the pivot. `reverse` winds it
/// the other way, which is the reference's *Counterclockwise* switch and the reason
/// one builder serves both readings.
#[must_use]
pub fn spiral(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let origin = pixel(geometry, geometry.points[0]);
    let outer = pixel(geometry, geometry.points[1]);
    let radius = (outer.1 - origin.1).abs();
    if radius == 0.0 {
        return Vec::new();
    }
    let sense = if geometry.options.reverse { -1.0 } else { 1.0 };
    let steps = SPIRAL_QUARTERS * SPIRAL_SAMPLES;
    let mut points = Vec::with_capacity(steps as usize + 1);
    for step in 0..=steps {
        let quarters = f64::from(step) / f64::from(SPIRAL_SAMPLES);
        let r = radius * PHI.powf(quarters);
        let theta = sense * quarters * std::f64::consts::FRAC_PI_2;
        points.push((origin.0 + r * theta.cos(), origin.1 + r * theta.sin()));
    }
    vec![DrawingPart::Polygon {
        points,
        filled: false,
    }]
}

/// The swing's own line, with the ladder opening on each side of it.
#[must_use]
pub fn wedge(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let apex = pixel(geometry, geometry.points[0]);
    let tip = pixel(geometry, geometry.points[1]);
    let unit = (tip.1 - apex.1).abs();
    let mut parts = Vec::new();
    // The axis first, so the wedge reads as one shape with a spine.
    parts.push(ray_to(geometry, apex, pixel_slope(apex, tip), tip.0, Extend::Right));
    for (ratio, _) in ladder(geometry, LevelProfile::Classic) {
        if *ratio == 0.0 {
            continue;
        }
        for side in [1.0, -1.0] {
            let at_tip = tip.1 + side * unit * ratio;
            let slope = pixel_slope(apex, (tip.0, at_tip));
            parts.push(ray_to(geometry, apex, slope, tip.0, Extend::Right));
        }
    }
    parts
}

/// The rays themselves, from the first anchor through the ladder's divisions of the
/// swing's price range at the second anchor's time.
fn rays(geometry: Geometry<'_>, default: &'static [(f64, &'static str)]) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let apex = pixel(geometry, geometry.points[0]);
    let tip = pixel(geometry, geometry.points[1]);
    let (from, to) = swing(geometry);
    let mut parts = Vec::new();
    for (ratio, label) in set_or_profile(geometry, default) {
        let price = from + (to - from) * ratio;
        let slope = pixel_slope(apex, (tip.0, geometry.frame.y_at(price)));
        parts.push(ray_to(geometry, apex, slope, tip.0, Extend::Right));
        parts.push(DrawingPart::Text {
            x: tip.0 + 4.0,
            y: geometry.frame.y_at(price) - 3.0,
            text: label.to_string(),
        });
    }
    parts
}

/// A vertical level spanning the plot, with its ratio written at the top.
///
/// The one place the group puts a level on the **x** axis, and the only way a time
/// tool can draw one — which is the point: a ratio of a *duration* drawn as a
/// price is a number about the wrong variable, and the audit found exactly that in
/// the concept version of this grid.
fn meridian(geometry: Geometry<'_>, time: f64, label: &str) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x = geometry.frame.x_at_ms(time);
    vec![
        DrawingPart::Segment {
            x1: x,
            y1: plot.y,
            x2: x,
            y2: plot.y + plot.h,
            dashed: false,
        },
        DrawingPart::Text {
            x: x + 3.0,
            y: plot.y + 11.0,
            text: format!("{label}%"),
        },
    ]
}

/// The impulse: its price leg, its duration, and the anchor the projections run
/// from. `None` until the three anchors exist.
///
/// `reverse` reads the impulse the other way round, which flips both legs: the
/// projections then run backwards in time as well as in price, because a swing
/// read in the opposite direction is a different *forecast*, not a mirrored one.
fn impulse(geometry: Geometry<'_>) -> Option<(Leg, Point)> {
    if geometry.points.len() < 3 {
        return None;
    }
    let (first, second, third) = (geometry.points[0], geometry.points[1], geometry.points[2]);
    let sign = if geometry.options.reverse { -1.0 } else { 1.0 };
    Some((
        Leg {
            price: (second.1 - first.1) * sign,
            duration: (second.0 - first.0) * sign,
        },
        third,
    ))
}

/// One leg of an impulse, in the units the tool measures.
struct Leg {
    price: f64,
    duration: f64,
}

/// The ladder to draw: the user's profile when they named one, this kind's
/// reference default when they did not.
fn set_or_profile(
    geometry: Geometry<'_>,
    default: &'static [(f64, &'static str)],
) -> &'static [(f64, &'static str)] {
    match geometry.options.level_profile {
        Some(profile) => profile.levels(),
        None => default,
    }
}

/// The same, for the four named profiles.
fn ladder(
    geometry: Geometry<'_>,
    default: LevelProfile,
) -> &'static [(f64, &'static str)] {
    geometry.options.level_profile.unwrap_or(default).levels()
}

/// The levels themselves: one segment and one label per ratio.
///
/// `dashed` is decided per ratio rather than per kind because the two sets differ
/// exactly there -- a retracement's levels are all facts, an extension's are facts
/// up to 100 and projections after it.
fn levels(
    geometry: Geometry<'_>,
    set: &[(f64, &str)],
    dashed: impl Fn(f64) -> bool,
) -> Vec<DrawingPart> {
    if geometry.points.len() < 2 {
        return Vec::new();
    }
    let (from, to) = swing(geometry);
    let (left, right) = extent(geometry);
    let mut parts = Vec::with_capacity(set.len() * 2);
    for (ratio, label) in set {
        let price = from + (to - from) * ratio;
        let y = geometry.frame.y_at(price);
        parts.push(DrawingPart::Segment {
            x1: left,
            y1: y,
            x2: right,
            y2: y,
            dashed: dashed(*ratio),
        });
        // The percentage *and* the price, formatted here: turning 0.618 into
        // "61.8" is arithmetic, and `docs/14` keeps arithmetic out of JavaScript.
        parts.push(DrawingPart::Text {
            x: left + 4.0,
            y: y - 3.0,
            text: format!("{label}%  {price:.2}"),
        });
    }
    parts
}

/// The swing's zero and its unit, in the direction the user asked for.
///
/// With `reverse` unset this is the first point to the second, which is what every
/// drawing stored before the option existed already means.
fn swing(geometry: Geometry<'_>) -> (f64, f64) {
    let first = geometry.price(0);
    let second = geometry.price(1);
    if geometry.options.reverse {
        (second, first)
    } else {
        (first, second)
    }
}

/// How far across the plot the levels run: between the anchors unless an option
/// says one or both ends go to the plot's edge.
fn extent(geometry: Geometry<'_>) -> (f64, f64) {
    let plot = geometry.frame.plot;
    let left = geometry.x(0).min(geometry.x(1));
    let right = geometry.x(0).max(geometry.x(1));
    match geometry.options.extend.unwrap_or(Extend::None) {
        Extend::None => (left, right),
        Extend::Left => (plot.x, right),
        Extend::Right => (left, plot.x + plot.w),
        Extend::Both => (plot.x, plot.x + plot.w),
    }
}

/// A point's pixel position.
fn pixel(geometry: Geometry<'_>, point: Point) -> (f64, f64) {
    (
        geometry.frame.x_at_ms(point.0),
        geometry.frame.y_at(point.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::{
        DrawingOptions, FIB_EXTENSION_LEVELS, FIB_LEVELS, FIB_OTE_LEVELS, FIB_TIME_LEVELS,
    };
    use crate::draw::Stage;

    /// A rising swing over a 100..290 window on a 1000x500 plot: ten bars of price
    /// distance, so a ratio reads as a price a test can name (61.8% is 181.8).
    fn swing() -> (Stage, Vec<Point>) {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let points = vec![(stage.ms(2), 120.0), (stage.ms(12), 220.0)];
        (stage, points)
    }

    /// An impulse of +40 over four bars, pulled back at the eighth.
    fn leg() -> (Stage, Vec<Point>) {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let points = vec![
            (stage.ms(1), 120.0),
            (stage.ms(5), 160.0),
            (stage.ms(8), 140.0),
        ];
        (stage, points)
    }

    fn segments(parts: &[DrawingPart]) -> Vec<(f64, f64, f64, f64, bool)> {
        parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Segment {
                    x1,
                    y1,
                    x2,
                    y2,
                    dashed,
                } => Some((*x1, *y1, *x2, *y2, *dashed)),
                _ => None,
            })
            .collect()
    }

    fn levels_of(parts: &[DrawingPart]) -> Vec<f64> {
        let mut ys: Vec<f64> = segments(parts)
            .into_iter()
            .filter(|(_, y1, _, y2, _)| *y1 == *y2)
            .map(|(_, y1, ..)| y1)
            .collect();
        ys.sort_by(f64::total_cmp);
        ys
    }

    fn texts(parts: &[DrawingPart]) -> Vec<String> {
        parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn ellipses(parts: &[DrawingPart]) -> Vec<(f64, f64, f64, bool)> {
        parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Ellipse {
                    cx,
                    cy,
                    rx,
                    half,
                    ..
                } => Some((*cx, *cy, *rx, *half)),
                _ => None,
            })
            .collect()
    }

    /// Where a stroke runs at one x, so two strokes of different length compare as
    /// the *lines* they are rather than as the segments they were clipped to.
    fn y_at(line: (f64, f64, f64, f64), x: f64) -> f64 {
        line.1 + (line.3 - line.1) / (line.2 - line.0) * (x - line.0)
    }

    fn stroke(part: &(f64, f64, f64, f64, bool)) -> (f64, f64, f64, f64) {
        (part.0, part.1, part.2, part.3)
    }

    #[test]
    fn a_tool_with_one_anchor_has_no_swing_to_divide() {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let options = DrawingOptions::default();
        let points = vec![(stage.ms(3), 130.0)];
        let geometry = stage.geometry(&points, &options);

        for parts in [
            retracement(geometry),
            extension(geometry),
            time_zones(geometry),
            fan(geometry),
            arcs(geometry),
            spiral(geometry),
            wedge(geometry),
        ] {
            assert!(parts.is_empty(), "a single anchor was drawn as a ladder");
        }
    }

    #[test]
    fn a_retracement_lays_the_classic_ladder_across_the_swings_price() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let parts = retracement(geometry);
        assert_eq!(levels_of(&parts).len(), FIB_LEVELS.len());

        // The ladder's ends are the anchors themselves, and every level between them
        // is that ratio of the way across -- in pixels, which is the same ratio
        // because the price mapping is linear.
        let ys = levels_of(&parts);
        let (zero, hundred) = (geometry.y(0), geometry.y(1));
        assert!(
            (ys.first().unwrap() - hundred).abs() < 1e-9,
            "the top of the ladder is not the swing's far end"
        );
        assert!(
            (ys.last().unwrap() - zero).abs() < 1e-9,
            "the bottom of the ladder is not the swing's origin"
        );
        let expected = zero + (hundred - zero) * 0.618;
        assert!(
            ys.iter().any(|y| (y - expected).abs() < 1e-6),
            "61.8 is not 61.8% of the way from the origin"
        );

        // Every level is a fact, so none of them is dashed.
        assert!(
            segments(&parts).iter().all(|(.., dashed)| !*dashed),
            "a retracement projected something"
        );
    }

    #[test]
    fn every_level_names_its_ratio_and_the_price_it_lands_on() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let labels = texts(&retracement(geometry));
        assert!(
            labels.iter().any(|text| text == "61.8%  181.80"),
            "the grid was drawn anonymously: {labels:?}"
        );
        // Pass 5 found the concept grid unreadable for the same reason: the ratio
        // alone tells a trader nothing about where on the chart to look.
        assert!(
            labels.iter().all(|text| text.contains('%') && text.contains('.')),
            "a label is missing its ratio or its price: {labels:?}"
        );
    }

    #[test]
    fn the_projection_ladder_marks_every_target_beyond_the_swing_as_a_target() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let parts = extension(geometry);
        let strokes = segments(&parts);
        // Seven levels plus the direction ray.
        assert_eq!(strokes.len(), FIB_EXTENSION_LEVELS.len() + 1);

        for (ratio, _) in FIB_EXTENSION_LEVELS {
            let price = 120.0 + 100.0 * ratio;
            let y = geometry.frame.y_at(price);
            // A level spans the swing; the direction ray starts at its far edge and
            // runs past it, so the two are told apart by where they begin.
            let (left, right) = extent(geometry);
            let at_level = strokes
                .iter()
                .find(|(x1, y1, x2, y2, _)| *x1 == left && *y1 == y && *y2 == y && *x2 == right)
                .unwrap_or_else(|| panic!("{ratio} has no level at {price} spanning {left}..{right}"));
            assert_eq!(
                at_level.4,
                ratio > 1.0,
                "{ratio} is dashed in the wrong set"
            );
        }
    }

    #[test]
    fn a_profile_replaces_the_ladder_rather_than_adding_to_it() {
        let (stage, points) = swing();
        let options = DrawingOptions {
            level_profile: Some(LevelProfile::Ote),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);

        let parts = retracement(geometry);
        assert_eq!(levels_of(&parts).len(), FIB_OTE_LEVELS.len());
        let labels = texts(&parts);
        for ratio in ["70.5", "88.6"] {
            assert!(
                labels.iter().any(|text| text.starts_with(&format!("{ratio}%"))),
                "the OTE band lost {ratio}: {labels:?}"
            );
        }
        assert!(
            !labels.iter().any(|text| text.starts_with("23.6%")),
            "the classic ladder is still in there"
        );
    }

    #[test]
    fn a_reversed_swing_reads_the_ladder_from_the_other_end() {
        let (stage, points) = swing();
        let forward = DrawingOptions::default();
        let back = DrawingOptions {
            reverse: true,
            ..DrawingOptions::default()
        };

        let one = retracement(stage.geometry(&points, &forward));
        let other = retracement(stage.geometry(&points, &back));

        // 0% moves from the first click to the second, and the price written beside
        // it with it -- the same grid counted the other way round.
        let has_zero_at = |parts: &[DrawingPart], price: f64| {
            texts(parts).iter().any(|text| {
                text.starts_with("0%") && text.ends_with(&format!("{price:.2}"))
            })
        };
        assert!(
            has_zero_at(&one, 120.0),
            "forward, 0% is not the swing's origin: {:?}",
            texts(&one)
        );
        assert!(
            has_zero_at(&other, 220.0),
            "reversed, 0% is not the swing's far end: {:?}",
            texts(&other)
        );
        assert_eq!(levels_of(&one).len(), levels_of(&other).len());
    }

    #[test]
    fn the_time_tools_put_their_levels_on_the_time_axis() {
        let (stage, points) = leg();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let zones = time_zones(stage.geometry(
            &[(stage.ms(2), 120.0), (stage.ms(12), 220.0)],
            &options,
        ));
        for name in ["trend time", "time zones"] {
            let parts = if name == "trend time" {
                trend_time(geometry)
            } else {
                zones.clone()
            };
            for (x1, y1, x2, y2, _) in segments(&parts) {
                // A ratio of a *duration* is a vertical line spanning the plot. If
                // this ever reads as a horizontal one, the tool is describing price.
                assert_eq!(x1, x2, "{name} drew a level across the wrong axis");
                assert_eq!(y1, stage.frame.plot.y);
                assert_eq!(y2, stage.frame.plot.y + stage.frame.plot.h);
            }
            assert!(!parts.is_empty(), "{name} drew nothing");
        }
    }

    #[test]
    fn a_time_zone_is_the_ladder_of_the_two_clicks_own_distance() {
        let stage = Stage::ramp(20, 100.0, 10.0);
        let options = DrawingOptions::default();
        let points = vec![(stage.ms(2), 120.0), (stage.ms(12), 220.0)];
        let geometry = stage.geometry(&points, &options);

        let parts = time_zones(geometry);
        let unit = stage.ms(12) - stage.ms(2);
        let at = |ratio: f64| stage.frame.x_at_ms(stage.ms(2) + unit * ratio);

        let xs: Vec<f64> = segments(&parts).into_iter().map(|(x1, ..)| x1).collect();
        for (ratio, _) in FIB_TIME_LEVELS {
            assert!(
                xs.iter().any(|x| (x - at(ratio)).abs() < 1e-6),
                "no line at {ratio} of the interval"
            );
        }
        // The forecast runs off the end of the evidence: the 161.8 window is past
        // the second click, and a tool that stopped there would predict nothing.
        assert!(
            at(1.618) > geometry.x(1),
            "the far projection is not past the swing"
        );
    }

    #[test]
    fn a_trend_extension_draws_each_target_from_the_bar_it_is_due() {
        let (stage, points) = leg();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let parts = trend_extension(geometry);
        let strokes = segments(&parts);
        // +40 over four bars, pulled back at the eighth: 100% is due at the twelfth.
        let due = |ratio: f64| stage.frame.x_at_ms(stage.ms(8) + (stage.ms(5) - stage.ms(1)) * ratio);
        let price = |ratio: f64| geometry.frame.y_at(140.0 + 40.0 * ratio);

        for ratio in [0.0, 1.0, 1.618] {
            assert!(
                strokes
                    .iter()
                    .any(|(x1, y1, ..)| (x1 - due(ratio)).abs() < 1e-6
                        && (y1 - price(ratio)).abs() < 1e-6),
                "no target at ratio {ratio}"
            );
        }
        assert!(
            strokes.iter().all(|(.., dashed)| *dashed),
            "a projection was drawn as a fact"
        );
        assert!(
            texts(&parts)
                .iter()
                .any(|text| text == "161.8%  204.72"),
            "the targets lost their prices: {:?}",
            texts(&parts)
        );
    }

    #[test]
    fn a_target_due_after_the_last_bar_is_not_drawn() {
        let (stage, points) = leg();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let drawn = trend_extension(geometry);
        let count = segments(&drawn).len();
        assert_eq!(
            count,
            FIB_EXTENSION_LEVELS.len() - 1,
            "361.8 is due off the chart and should not be stretched onto it"
        );

        // The same tool with a long impulse has almost nothing to show, and says so
        // by drawing less rather than by inventing room.
        let far = vec![points[0], (stage.ms(19), 160.0), points[2]];
        let few = trend_extension(stage.geometry(&far, &options));
        assert!(
            segments(&few).len() < count,
            "a slower projection drew the same grid"
        );
    }

    #[test]
    fn reversing_a_trend_extension_runs_it_backwards_in_time() {
        let (stage, points) = leg();
        let options = DrawingOptions {
            reverse: true,
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);

        let strokes = segments(&trend_extension(geometry));
        let pullback = geometry.x(2);
        // A swing read the other way round is a different forecast, so the ladder
        // walks *back* from the pullback instead of forward off the right edge.
        assert!(
            strokes
                .iter()
                .filter(|(x1, ..)| *x1 != pullback)
                .all(|(x1, ..)| *x1 < pullback),
            "a reversed extension still ran forward: {strokes:?}"
        );
    }

    #[test]
    fn a_fib_channel_holds_the_third_anchor_on_its_own_edge() {
        let (stage, points) = leg();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let parts = channel(geometry);
        let strokes = segments(&parts);
        let (origin, target, anchor) = (
            pixel(geometry, points[0]),
            pixel(geometry, points[1]),
            pixel(geometry, points[2]),
        );
        let base = pixel_slope(origin, target);

        // 0% is the line through the first two clicks; 100% is the parallel through
        // the third, which is the whole claim of the tool.
        let through = |x: f64, y: f64| {
            strokes
                .iter()
                .any(|part| (y_at(stroke(part), x) - y).abs() < 1e-6)
        };
        assert!(through(anchor.0, anchor.1), "the band does not hold its anchor");
        assert!(
            through(target.0, origin.1 + base * (target.0 - origin.0)),
            "the band lost the line it was drawn from"
        );

        let interiors = strokes.iter().filter(|(.., dashed)| *dashed).count();
        let edges = strokes.len() - interiors;
        assert_eq!(edges, 2, "a channel's edges are not its only solid lines");
    }

    #[test]
    fn a_fan_is_rays_and_a_pitchfan_is_rays_past_the_swings_box() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let apex = (geometry.x(0), geometry.y(0));
        let tip_x = geometry.x(1);
        let right = stage.frame.plot.x + stage.frame.plot.w;

        let fan_lines = segments(&fan(geometry));
        let pitch_lines = segments(&pitchfan(geometry));
        assert_eq!(fan_lines.len(), FAN_LEVELS.len());
        assert_eq!(pitch_lines.len(), PITCHFAN_LEVELS.len());

        for line in fan_lines.iter().chain(pitch_lines.iter()) {
            // Rays, not segments: every one starts at the pivot and runs off the
            // right edge of the plot, which is the only reason a fan is a fan.
            assert_eq!(line.0, apex.0, "a ray did not start at the pivot");
            assert_eq!(line.2, right, "a ray stopped at the swing");
        }

        // The pitchfan's extra coefficients are the two that leave the box.
        let above = |line: &(f64, f64, f64, f64, bool)| y_at(stroke(line), tip_x) < geometry.y(1);
        assert!(
            pitch_lines.iter().filter(|line| above(line)).count() >= 2,
            "the pitchfan never left the swing"
        );
        assert!(
            fan_lines.iter().all(|line| {
                let y = y_at(stroke(line), tip_x);
                y <= geometry.y(0).max(geometry.y(1)) && y >= geometry.y(0).min(geometry.y(1))
            }),
            "a fan ray left the box the fan divides"
        );
    }

    #[test]
    fn arcs_ring_both_ends_of_the_swing_and_circles_only_the_first() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);

        let arc_rings = ellipses(&arcs(geometry));
        let circle_rings = ellipses(&circles(geometry));

        // The arcs use the classic ladder without its zero -- a zero-radius arc is
        // not a shape -- and answer at both anchors.
        assert_eq!(arc_rings.len(), (FIB_LEVELS.len() - 1) * 2);
        assert_eq!(circle_rings.len(), CIRCLE_LEVELS.len());

        // And the two are different *shapes*, not just different counts: a kind named
        // for an arc that strokes a circle is the defect this group exists to avoid.
        assert!(
            arc_rings.iter().all(|(_, _, _, half)| *half),
            "an arc stroked a whole circle"
        );
        assert!(
            circle_rings.iter().all(|(_, _, _, half)| !*half),
            "a circle was cut in half"
        );

        let unit = (geometry.y(1) - geometry.y(0)).abs();
        assert!(
            circle_rings
                .iter()
                .any(|(_, _, rx, _)| (rx - unit).abs() < 1e-6),
            "the circles never reach the swing's own distance"
        );
        assert!(
            circle_rings.iter().all(|(cx, cy, _, _)| (*cx, *cy) == (geometry.x(0), geometry.y(0))),
            "a circle drifted off the pivot"
        );
        let centres: Vec<(f64, f64)> = arc_rings
            .iter()
            .map(|(cx, cy, _, _)| (*cx, *cy))
            .collect();
        assert!(
            centres.contains(&(geometry.x(0), geometry.y(0)))
                && centres.contains(&(geometry.x(1), geometry.y(1))),
            "the arcs abandoned one end of the swing"
        );
    }

    #[test]
    fn a_spiral_grows_by_the_golden_ratio_every_quarter_turn() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let origin = (geometry.x(0), geometry.y(0));

        let parts = spiral(geometry);
        assert_eq!(parts.len(), 1, "a spiral is one stroke");
        let DrawingPart::Polygon { points: trace, filled } = &parts[0] else {
            panic!("a spiral was not drawn as a path");
        };
        assert!(!*filled, "a spiral filled the chart");

        let radius = |point: &(f64, f64)| ((point.0 - origin.0).powi(2) + (point.1 - origin.1).powi(2)).sqrt();
        let start = radius(&trace[0]);
        assert!((start - (geometry.y(1) - geometry.y(0)).abs()).abs() < 1e-6);
        let quarter = radius(&trace[SPIRAL_SAMPLES as usize]);
        assert!(
            (quarter / start - PHI).abs() < 1e-9,
            "a quarter turn multiplied the radius by {}",
            quarter / start
        );
        // Unwound for six turns, so the trace is long enough to be a spiral and not
        // an arc -- and monotonic, which is what makes it a growth series.
        assert_eq!(trace.len(), (SPIRAL_QUARTERS * SPIRAL_SAMPLES + 1) as usize);
        assert!(trace.windows(2).all(|pair| radius(&pair[1]) > radius(&pair[0])));
    }

    #[test]
    fn reversing_a_spiral_winds_it_the_other_way() {
        let (stage, points) = swing();
        let forward = DrawingOptions::default();
        let back = DrawingOptions {
            reverse: true,
            ..DrawingOptions::default()
        };

        let one = spiral(stage.geometry(&points, &forward));
        let other = spiral(stage.geometry(&points, &back));
        let DrawingPart::Polygon { points: a, .. } = &one[0] else {
            unreachable!()
        };
        let DrawingPart::Polygon { points: b, .. } = &other[0] else {
            unreachable!()
        };

        // Both leave the pivot the same way; a quarter turn later they are on
        // opposite sides of it, which is the reference's clockwise switch.
        assert!((a[0].1 - b[0].1).abs() < 1e-6);
        let step = SPIRAL_SAMPLES as usize;
        assert!(a[step].1 > b[step].1, "both windings turn the same way");
    }

    #[test]
    fn a_wedge_opens_symmetrically_about_the_swings_own_line() {
        let (stage, points) = swing();
        let options = DrawingOptions::default();
        let geometry = stage.geometry(&points, &options);
        let tip_x = geometry.x(1);

        let parts = wedge(geometry);
        let strokes = segments(&parts);
        let spine = stroke(strokes.first().unwrap());
        assert!(
            (y_at(spine, geometry.x(0)) - geometry.y(0)).abs() < 1e-6
                && (y_at(spine, tip_x) - geometry.y(1)).abs() < 1e-6,
            "the spine is not the swing"
        );

        // Every other line is a mirrored pair about that spine, at the ratio's own
        // distance -- which is the shape a trader calls a wedge. The builder opens
        // each pair below the spine first, so the two arms come in a fixed order.
        let arms = &strokes[1..];
        assert_eq!(arms.len(), (FIB_LEVELS.len() - 1) * 2);
        let unit = (geometry.y(1) - geometry.y(0)).abs();
        let centre = y_at(spine, tip_x);
        for index in 0..FIB_LEVELS.len() - 1 {
            let ratio = FIB_LEVELS[index + 1].0;
            let lower = y_at(stroke(&arms[index * 2]), tip_x);
            let upper = y_at(stroke(&arms[index * 2 + 1]), tip_x);
            assert!(
                (lower - (centre + unit * ratio)).abs() < 1e-6
                    && (upper - (centre - unit * ratio)).abs() < 1e-6,
                "{ratio} is not a symmetric pair about the spine"
            );
        }
    }

    #[test]
    fn extend_moves_the_levels_and_the_rays_rather_than_the_verticals() {
        let (stage, points) = swing();
        let options = DrawingOptions {
            extend: Some(Extend::Both),
            ..DrawingOptions::default()
        };
        let geometry = stage.geometry(&points, &options);

        let plot = stage.frame.plot;
        for (x1, _, x2, _, _) in segments(&retracement(geometry)) {
            assert_eq!((x1, x2), (plot.x, plot.x + plot.w));
        }
        // The time tools already span the plot, so the option cannot move them and
        // the module says so by ignoring it rather than by half-applying it.
        let zones = segments(&time_zones(stage.geometry(&points, &options)));
        assert!(zones.iter().all(|(x1, _, x2, _, _)| x1 == x2));
        assert!(zones.iter().all(|(_, y1, _, y2, _)| *y1 == plot.y && *y2 == plot.y + plot.h));
    }
}
