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
use analytics_core::types::Candle;

pub mod channels;
pub mod cyclic;
pub mod elliott;
pub mod fibonacci;
pub mod gann;
pub mod lines;
pub mod measurement;
pub mod patterns;
pub mod pitchforks;
pub mod positions;
pub mod shapes;
pub mod annotation;

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
    /// The traded bars, ascending by open time.
    ///
    /// The **real** series, not the drawn one — the same rule [`crate::scene`]'s
    /// region bands follow. A regression line, a volume profile and an anchored
    /// VWAP are facts about prices that traded, so switching the chart to
    /// Heikin-Ashi must not move them.
    ///
    /// Some kinds read every bar in it, some read none, and a tool that computed
    /// its own idea of "the bars in this range" from the frame's width would be
    /// a second price feed. Passing the series is what keeps one.
    pub candles: &'a [Candle],
    /// The drawing's text content, for annotation kinds that render a label.
    pub text: Option<&'a str>,
}

impl<'a> Geometry<'a> {
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

    /// The traded bars whose open time falls between two points, inclusive.
    ///
    /// Order-independent — a drag left-to-right and the same drag right-to-left
    /// select the same bars, because a range of time does not have a direction
    /// — and empty rather than panicking when the window holds none. A tool that
    /// reads the market returns what the market actually printed, and a caller
    /// with nothing to read says so in its own geometry rather than inventing a
    /// line from two anchors.
    pub fn bars_between(self, a: Point, b: Point) -> &'a [Candle] {
        let (from, to) = if a.0 <= b.0 { (a.0, b.0) } else { (b.0, a.0) };
        let start = self.candles.partition_point(|candle| ms(candle) < from);
        let end = self.candles.partition_point(|candle| ms(candle) <= to);
        &self.candles[start..end]
    }
}

/// A candle's open time in **milliseconds** — the unit a drawing's point uses.
///
/// The conversion is here rather than at each call site because the two units
/// meet constantly in this module: bars arrive on the platform's nanosecond
/// clock and every anchor in `draw/` is a millisecond. A silent `* 1e6` in one
/// group and `/ 1_000_000` in another is the same unit bug the scene's two `x_`
/// methods exist to prevent.
#[must_use]
pub fn ms(candle: &Candle) -> f64 {
    (candle.open_time / 1_000_000) as f64
}

/// The slope of the line through two pixel positions.
///
/// A zero run is reported as a vertical line rather than as a division clamped to a
/// positive epsilon: a band or a fork whose anchors were placed right-to-left has a
/// *negative* run, and clamping it flips the sign of the slope, which mirrors the
/// shape -- and a mirrored trendline reads as the chart disagreeing about the
/// direction of the trader's own analysis.
#[must_use]
pub(crate) fn pixel_slope(from: (f64, f64), to: (f64, f64)) -> f64 {
    let dx = to.0 - from.0;
    if dx == 0.0 {
        (to.1 - from.1) / f64::EPSILON
    } else {
        (to.1 - from.1) / dx
    }
}

/// A ray from `apex` at `slope`, run out as `extend` says.
///
/// The fan-shaped tools in two groups — Fibonacci's fans and wedges, Gann's fan
/// and corner angles — all ask the same question: where does this stroke stop?
/// The answer must be one function, because a ray that stops at the swing's box
/// in one group and at the plot's edge in another is the same tool disagreeing
/// with itself. `stop` is the point the stroke would end at with no extension,
/// and `default` is what the kind does when the user has set nothing: a fan's
/// reference default is forward, a wedge's too.
pub(crate) fn ray_to(
    geometry: Geometry<'_>,
    apex: (f64, f64),
    slope: f64,
    stop: f64,
    default: Extend,
) -> DrawingPart {
    let plot = geometry.frame.plot;
    let (mut left, mut right) = (apex.0.min(stop), apex.0.max(stop));
    match geometry.options.extend.unwrap_or(default) {
        Extend::None => {}
        Extend::Left => left = plot.x,
        Extend::Right => right = plot.x + plot.w,
        Extend::Both => {
            left = plot.x;
            right = plot.x + plot.w;
        }
    }
    let y = |x: f64| apex.1 + slope * (x - apex.0);
    DrawingPart::Segment {
        x1: left,
        y1: y(left),
        x2: right,
        y2: y(right),
        dashed: false,
    }
}

/// A stage: one frame, one traded series, and the points a drawing sits on.
#[cfg(test)]
pub(crate) struct Stage {
    pub(crate) frame: Frame,
    pub(crate) candles: Vec<Candle>,
}

#[cfg(test)]
impl Stage {
    /// `bars` M5 candles, each closing one `step` above the last from `first`,
    /// over a price window from the first close to the last.
    ///
    /// A ramp is the fixture that catches a fit that is not a fit: every point is
    /// on the line, so a regression drawn over it is the closes themselves and
    /// its *R* is exactly 1.00 — and a stage whose window *is* that range puts
    /// the first close at the bottom edge and the last at the top, which is a
    /// pixel position a test can name without inverting the mapping.
    pub(crate) fn ramp(bars: usize, first: f64, step: f64) -> Self {
        Self::over(bars, |index| first + index as f64 * step, first, first + (bars as f64 - 1.0) * step)
    }

    /// `bars` candles that closed at one price, under a window wide enough to
    /// make the scale a variable rather than a constant.
    pub(crate) fn flat(bars: usize, price: f64) -> Self {
        Self::over(bars, |_| price, price - 100.0, price + 100.0)
    }

    pub(crate) fn over(bars: usize, close: impl Fn(usize) -> f64, low: f64, high: f64) -> Self {
        let candles = (0..bars)
            .map(|index| {
                let price = close(index);
                Candle {
                    symbol: "BTCUSDT".into(),
                    timeframe: analytics_core::types::Timeframe::M5,
                    open_time: T0_NS + index as i64 * BAR_NS,
                    open: price,
                    high: price + 0.25,
                    low: price - 0.25,
                    close: price,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect();
        Self {
            frame: Frame {
                plot: crate::scene::Plot {
                    x: 0.0,
                    y: 0.0,
                    w: 1_000.0,
                    h: 500.0,
                },
                from: T0_NS,
                to: T0_NS + bars as i64 * BAR_NS,
                price_min: low,
                price_max: high,
                bar_nanos: BAR_NS,
                scale: crate::scene::ScaleMap::Linear,
            },
            candles,
        }
    }

    /// The open time of bar `index`, in the milliseconds a drawing carries.
    pub(crate) fn ms(&self, index: usize) -> f64 {
        ms(&self.candles[index])
    }

    /// What the group function is handed.
    pub(crate) fn geometry<'a>(
        &'a self,
        points: &'a [Point],
        options: &'a crate::drawing::DrawingOptions,
    ) -> Geometry<'a> {
        Geometry {
            frame: &self.frame,
            points,
            options,
            candles: &self.candles,
            text: None,
        }
    }
}

/// One M5 bar, in the two units the engine keeps apart.
#[cfg(test)]
pub(crate) const BAR_NS: i64 = 300_000_000_000;
/// The instant the fixtures count from, in nanoseconds.
#[cfg(test)]
pub(crate) const T0_NS: i64 = 1_700_000_000_000_000_000;

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
        DrawingKind::Hray => lines::horizontal_ray(geometry),
        DrawingKind::Vline => lines::vertical(geometry),
        DrawingKind::Crossline => lines::cross(geometry),
        DrawingKind::Infoline => lines::info(geometry),
        DrawingKind::Regression => lines::regression(geometry),
        DrawingKind::Arrow => lines::arrow(geometry),
        DrawingKind::ArrowMarkUp => lines::mark(geometry, true),
        DrawingKind::ArrowMarkDown => lines::mark(geometry, false),
        // Channels.
        DrawingKind::Channel => channels::parallel(geometry),
        DrawingKind::DisjointChannel => channels::disjoint(geometry),
        DrawingKind::FlatTopBottom => channels::flat_top_bottom(geometry),
        // Pitchforks: one builder, four templates.
        DrawingKind::Pitchfork => pitchforks::fork(geometry, pitchforks::Template::Andrews),
        DrawingKind::SchiffPitchfork => pitchforks::fork(geometry, pitchforks::Template::Schiff),
        DrawingKind::ModifiedSchiffPitchfork => {
            pitchforks::fork(geometry, pitchforks::Template::ModifiedSchiff)
        }
        DrawingKind::InsidePitchfork => pitchforks::fork(geometry, pitchforks::Template::Inside),
        // Shapes.
        DrawingKind::Rect => shapes::rect(geometry),
        DrawingKind::Triangle => shapes::triangle(geometry),
        DrawingKind::Circle => shapes::circle(geometry),
        DrawingKind::Arc => shapes::arc(geometry),
        DrawingKind::RotatedRect => shapes::rotated_rect(geometry),
        DrawingKind::EllipseTool => shapes::ellipse(geometry),
        DrawingKind::Polyline => shapes::polyline(geometry),
        DrawingKind::Curve => shapes::curve(geometry),
        DrawingKind::DoubleCurve => shapes::double_curve(geometry),
        DrawingKind::Brush => shapes::brush(geometry),
        DrawingKind::Highlighter => shapes::highlighter(geometry),
        // Fibonacci.
        DrawingKind::Fib => fibonacci::retracement(geometry),
        DrawingKind::FibExtension => fibonacci::extension(geometry),
        DrawingKind::TrendFibExtension => fibonacci::trend_extension(geometry),
        DrawingKind::TrendFibTime => fibonacci::trend_time(geometry),
        DrawingKind::FibTimeZone => fibonacci::time_zones(geometry),
        DrawingKind::FibChannel => fibonacci::channel(geometry),
        DrawingKind::FibFan => fibonacci::fan(geometry),
        DrawingKind::Pitchfan => fibonacci::pitchfan(geometry),
        DrawingKind::FibArcs => fibonacci::arcs(geometry),
        DrawingKind::FibCircles => fibonacci::circles(geometry),
        DrawingKind::FibSpiral => fibonacci::spiral(geometry),
        DrawingKind::FibWedge => fibonacci::wedge(geometry),
        // Gann.
        DrawingKind::GannBox => gann::boxed(geometry),
        DrawingKind::GannFan => gann::fan(geometry),
        DrawingKind::GannSquare => gann::square(geometry),
        DrawingKind::GannSquareFixed => gann::square_fixed(geometry),
        // Patterns.
        DrawingKind::Xabcd => patterns::xabcd(geometry),
        DrawingKind::AbcdPattern => patterns::abcd(geometry),
        DrawingKind::Cypher => patterns::cypher(geometry),
        DrawingKind::HeadShoulders => patterns::head_shoulders(geometry),
        DrawingKind::ThreeDrives => patterns::three_drives(geometry),
        DrawingKind::TrianglePattern => patterns::triangle_pattern(geometry),
        // Elliott.
        DrawingKind::ElliottImpulse => elliott::impulse(geometry),
        DrawingKind::ElliottCorrection => elliott::correction(geometry),
        DrawingKind::ElliottTriangle => elliott::triangle(geometry),
        DrawingKind::ElliottDoubleCombo => elliott::double_combo(geometry),
        DrawingKind::ElliottTripleCombo => elliott::triple_combo(geometry),
        // Cyclic.
        DrawingKind::CyclicLines => cyclic::lines(geometry),
        DrawingKind::TimeCycles => cyclic::bands(geometry),
        DrawingKind::SineLine => cyclic::sine(geometry),
        // Measurement and forecast.
        DrawingKind::PriceRange => measurement::price_band(geometry),
        DrawingKind::DateRange => measurement::time_band(geometry),
        DrawingKind::Forecast => measurement::forecast(geometry),
        DrawingKind::FixedRangeVP => measurement::fixed_vp(geometry),
        DrawingKind::AnchoredVP => measurement::anchored_vp(geometry),
        DrawingKind::AnchoredVWAP => measurement::avwap(geometry),
        // Measurement.
        DrawingKind::Measure => measurement::ruler(geometry),
        DrawingKind::DatepriceRange => measurement::range(geometry),
        // Positions.
        DrawingKind::PositionLong => positions::risk_box(geometry, true),
        DrawingKind::PositionShort => positions::risk_box(geometry, false),
        // Annotations.
        DrawingKind::TextNote => annotation::text_note(geometry),
        DrawingKind::StickyNote => annotation::sticky_note(geometry),
        DrawingKind::Callout => annotation::callout(geometry),
        DrawingKind::PriceLabel => annotation::price_label(geometry),
        DrawingKind::FlagMark => annotation::flag_mark(geometry),
        DrawingKind::IconMark => annotation::icon_mark(geometry),
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
