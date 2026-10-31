//! The shapes a user draws on the chart (`docs/14-FRONTEND-CHART-ENGINE.md`).
//!
//! ## A list of points, and the kind says how many
//!
//! Every shape a trader draws is a set of points and a rule for what goes
//! between them, so that is the whole vocabulary: a [`DrawingKind`], a list of
//! [`Anchor`]s whose length the kind declares ([`AnchorCount`]), and the
//! [`DrawingOptions`] that kind reads. A trendline is the segment between two, a
//! rectangle is the area two span, a Fibonacci is the ladder across one impulse,
//! a horizontal line is the degenerate case that needs one, a parallel channel is
//! two for the line plus one for its width, and a polyline is any number.
//!
//! The arity is *declared* rather than discovered, because one rule with two
//! spellings is the rule that eventually disagrees: the engine's validator, the
//! toolbar's click count, and storage's column shape all read the same
//! [`DrawingKind::anchors`], and a registry row that contradicts its kind is a
//! compile-time-free error the tests catch instead.
//!
//! The kinds are an enum rather than free text because a kind nobody can draw
//! must be **refused**, not stored: a drawing that saves and then never appears
//! is worse than one that will not save. The 2026-09 parity extension added the
//! TradingView staples (channel, angle, arc, circle, triangle, long/short
//! position boxes, date/price range); each was one variant plus an arm in the
//! scene's `drawing_parts`.
//!
//! ## The shell says where on the screen, the engine says what that is
//!
//! An [`Anchor`] is either `absolute` -- a millisecond timestamp and a price --
//! or `fraction`, a position over the plot rectangle. The shell sends fractions
//! while a drawing is being placed or dragged, and the scene reports every
//! anchor back as absolute. So the shell never turns a pointer position into a
//! price or a time.
//!
//! That is the interaction model's rule generalised. It is also why a drag needs
//! no gesture type of its own: **a drag is a request whose anchor is a fraction**,
//! so it folds into the same one-rebuild-per-frame coalescing a wheel does.
//!
//! The two forms use different field names -- `x`/`y` against `time`/`price` --
//! and the difference is load-bearing. One pair of names for both would make "the
//! shell sent a fraction where the engine expected a timestamp" a mistake nothing
//! catches: the numbers are the same shape, the units are not, and the result is
//! a drawing somewhere in 1970.
//!
//! ## The time unit is milliseconds
//!
//! The platform stores nanoseconds, and everything else crossing to the browser
//! is nanoseconds. An anchor is the one value that has to make the round trip
//! *back* into a database, and JSON numbers are doubles: a nanosecond timestamp
//! is past 2^53, so one read into JavaScript and written back is a different
//! number. Milliseconds fit exactly. The conversion is in `db::drawings`, at the
//! boundary, and nowhere else.

use serde::{Deserialize, Serialize};

/// How many points a kind takes.
///
/// The declaration the whole object model hangs on: the engine's validator, the
/// toolbar's click count, the shell's placement machine and storage's list
/// length all read this, so "how many points does a channel take" has one
/// answer in the crate.
///
/// Two shapes, because there are two kinds of tool. [`AnchorCount::Fixed`] is
/// the count a shape needs — a trendline is two points and a third would make it
/// a different drawing. [`AnchorCount::Many`] is the open-ended one: a polyline,
/// a brush stroke, a harmonic with an extra pivot — at least `min`, no upper
/// bound, finished by the shell's Enter or double-click rather than by a count.
///
/// On the wire it is an object rather than a number, and that is the difference
/// between a tool the toolbar can place and one it cannot: `2` says nothing about
/// whether 3 is allowed, so `Many` needs a field of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum AnchorCount {
    /// Exactly `points` of them, no more and no fewer.
    Fixed {
        /// How many the shape needs.
        points: u8,
    },
    /// At least `min` of them, however many the user places.
    Many {
        /// The fewest that still makes a drawing rather than a dot.
        min: u8,
    },
}

impl AnchorCount {
    /// A kind that takes exactly `n` points.
    #[must_use]
    pub const fn fixed(n: u8) -> Self {
        Self::Fixed { points: n }
    }

    /// Whether a list of `n` points is this kind's arity.
    #[must_use]
    pub const fn accepts(self, n: usize) -> bool {
        match self {
            Self::Fixed { points } => n == points as usize,
            Self::Many { min } => n >= min as usize,
        }
    }

    /// The exact count, when there is one.
    ///
    /// `None` for [`AnchorCount::Many`], which has a floor and no ceiling: a
    /// caller that wanted "the length to allocate" must not get `min` and believe
    /// the drawing cannot grow.
    #[must_use]
    pub const fn points_for(self) -> Option<usize> {
        match self {
            Self::Fixed { points } => Some(points as usize),
            Self::Many { .. } => None,
        }
    }

    /// The fewest points this kind can draw with — the number the toolbar's
    /// click counter starts from, and the one a refusal names.
    #[must_use]
    pub const fn minimum(self) -> usize {
        match self {
            Self::Fixed { points } => points as usize,
            Self::Many { min } => min as usize,
        }
    }
}

/// The kinds this engine can draw.
///
/// This is the **object vocabulary** of the chart (`docs/21-CHART-OBJECT-ENGINE.md`):
/// every kind is a structured object -- a list of points whose length it declares,
/// and a rule for what goes between them -- that the AI can read, the storage can
/// persist, and the toolbar can be *built from*, rather than a button someone
/// typed twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawingKind {
    /// The segment between the two anchors.
    Trendline,
    /// A horizontal line across the plot, at the first anchor's price.
    Hline,
    /// A vertical line across the plot, at the first anchor's time.
    Vline,
    /// The segment from the first anchor **through** the second, extended to
    /// the plot's edge in the direction of the drag.
    Ray,
    /// The line through both anchors, extended in **both** directions.
    Extended,
    /// A horizontal level that runs from its anchor to the plot's right edge.
    ///
    /// The half of [`DrawingKind::Hline`] that a trader marks a breakout with:
    /// the price is the claim, and the part of the chart before the click is
    /// evidence against it rather than part of the level.
    Hray,
    /// One anchor, and the two full-plot lines that cross through it.
    Crossline,
    /// The segment between two anchors, labelled with both prices and the bars
    /// between them.
    Infoline,
    /// The least-squares line through the closes of the bars between two
    /// anchors, with its Pearson's *R* written beside it.
    Regression,
    /// The segment between two anchors, with an arrowhead at the second.
    ///
    /// The head is a fixed number of **pixels**, not a fraction of the span:
    /// TradingView's arrow keeps a constant head as you stretch it, because the
    /// mark is a pointer at something rather than a shape that grows.
    Arrow,
    /// A fixed-size up arrow at one point.
    ArrowMarkUp,
    /// A fixed-size down arrow at one point.
    ArrowMarkDown,
    /// The rectangle the two anchors span.
    Rect,
    /// Two parallel lines whose edges start and end independently.
    ///
    /// Four points, not two: the whole point of the disjoint channel is that a
    /// channel drawn over two impulses that did not begin together can still be
    /// one band.
    DisjointChannel,
    /// An angled line with a horizontal boundary, and the region between.
    FlatTopBottom,
    /// Andrews' median line through three pivots, with its two parallel tines.
    Pitchfork,
    /// The fork whose handle is shifted halfway to the second pivot in price.
    ///
    /// TradingView's own two Schiff pages swap this pair against the classic
    /// construction; we ship the classic one, matching the platform's tooltip, and
    /// the discrepancy is recorded in `reports/drawing-tools/ledger/` rather than
    /// left for the next reader to rediscover.
    SchiffPitchfork,
    /// The fork whose handle is shifted halfway to the second pivot, in price and
    /// in time.
    ModifiedSchiffPitchfork,
    /// The fork shrunk toward its handle: tines through the midpoints of
    /// handle-to-second and handle-to-third, so its median is the plain fork's line.
    InsidePitchfork,
    /// Fibonacci retracement levels between the two anchors.
    Fib,
    /// Fibonacci **extension** levels projected beyond the swing: the 0 and 100
    /// sit on the swing itself and the 1.272 / 1.618 / 2.0 / 2.618 targets are
    /// drawn past the second anchor, in the direction of the drag.
    FibExtension,
    /// The **trend-based** extension: the impulse's start, its end, and the
    /// pullback's end. The targets are projected from the third anchor along the
    /// impulse's own price/time slope, so the ladder walks with the trend instead
    /// of sitting flat on the swing.
    TrendFibExtension,
    /// The same three anchors with the ratios laid out on the **time** axis:
    /// vertical lines at when each projection is due, not at what it prices.
    TrendFibTime,
    /// Vertical lines at the ladder's ratios of the anchors' time distance,
    /// counted from the first anchor: a forecast of *when*.
    FibTimeZone,
    /// A channel whose width is divided by the ladder: the line through the first
    /// two anchors, its parallel through the third, and the ratios between them.
    FibChannel,
    /// The speed-resistance fan: lines from the swing's origin through the
    /// 38.2 / 50 / 61.8 divisions of its price range.
    FibFan,
    /// The fan's curved counterpart: arcs at the same divisions, centred on each
    /// end of the swing.
    FibArcs,
    /// Concentric circles at the ladder's ratios of the swing's price distance.
    FibCircles,
    /// A logarithmic spiral from the swing's origin, its radius multiplied by φ
    /// every quarter turn.
    FibSpiral,
    /// Lines on each side of the swing's own line, opening as the ladder's ratios
    /// of its price distance: a trend with a widening band.
    FibWedge,
    /// Rays from a pivot at the ladder's coefficients of the slope through the
    /// second anchor — the fan read as a slope rather than as a price range,
    /// which is why its lines can run past the swing's box.
    Pitchfan,
    /// A box divided into eighths on both axes, with the corner's 1×1 angle.
    ///
    /// The box's own drag *is* its scale: the two corners state how many price
    /// units go with how many bars, and every other Gann tool on the chart is
    /// read against that one number.
    GannBox,
    /// Nine rays from a pivot — 1×1, its quarters and its doubles — at the scale
    /// the second point declares.
    GannFan,
    /// The box's eight-by-eight grid with the fan through its centre and the
    /// arcs at the eighths of its side.
    GannSquare,
    /// The square repeated: levels 0 through 5, one unit square per level, the
    /// second point setting how large a unit is.
    GannSquareFixed,
    // Patterns: harmonic and other multi-leg shapes
    /// Gartley / Bat / Crab / Butterfly / Shark with leg ratios and ✓/✗ badges.
    Xabcd,
    /// Four-point ABCD pattern drawn on custom ratios.
    AbcdPattern,
    /// Cypher pattern with 0.382–0.618 XA, 1.13–1.414 XA, 0.786 XC.
    Cypher,
    /// Head-and-shoulders with neckline. Five points: left shoulder high, head, right shoulder, plus two for the line.
    HeadShoulders,
    /// Three drives: six points across three impulse legs.
    ThreeDrives,
    /// Symmetrical triangle: four points marking converging higher lows and lower highs.
    TrianglePattern,
    /// Elliott: five-wave impulse labeled 1..5.
    ElliottImpulse,
    /// Elliott: three-wave correction labeled A..C.
    ElliottCorrection,
    /// Elliott: triangle labeled A..E.
    ElliottTriangle,
    /// Elliott: double three / W-X-Y correction.
    ElliottDoubleCombo,
    /// Elliott: triple three / W-X-Y-X-Z correction.
    ElliottTripleCombo,
    // Cyclic: time-based oscillations
    /// Vertical lines repeating at a fixed interval into the future.
    CyclicLines,
    /// Alternating shaded bands in time, period set by two anchors.
    TimeCycles,
    /// Sine wave: second anchor sets cycle length and amplitude.
    SineLine,
    // Forecast and data-driven measurement.
    /// A horizontal band between two price levels, spanning the full plot width.
    PriceRange,
    /// A vertical band between two time points, spanning the full plot height.
    DateRange,
    /// A projected target: origin point with a future time/price target, drawn as
    /// a crosshair with connecting diagonal.
    Forecast,
    /// Fixed-range volume profile histogram between two time anchors.
    #[serde(rename = "fixed_range_vp")]
    FixedRangeVP,
    /// Volume profile from one anchor to the right edge.
    #[serde(rename = "anchored_vp")]
    AnchoredVP,
    /// Anchored VWAP line from one anchor through all subsequent bars.
    #[serde(rename = "anchored_vwap")]
    AnchoredVWAP,
    /// The price/time delta between the two anchors: a box, dashed guides to
    /// both axes, and the delta as a label.
    Measure,
    /// The line through the first two anchors and its parallel through the third:
    /// a band, both edges spanning the window. Three anchors.
    Channel,
    /// A straight line through the first anchor at an arbitrary angle; the
    /// second anchor sets the slope and the line extends from the first anchor
    /// through it. Two anchors, but unlike a ray it has no fixed endpoints rule.
    Angle,
    /// A half ellipse: the first anchor is the centre, the second sets the
    /// horizontal radius, the third the vertical. Three anchors.
    Arc,
    /// A filled circle: the first anchor is the centre, the second sits on the
    /// radius. Two anchors.
    Circle,
    /// The triangle through the three anchors. Three anchors.
    Triangle,
    /// A long position: an entry line at the first anchor's price with a
    /// profit band above and a stop band below, the second anchor setting the
    /// extent along time.
    PositionLong,
    /// The short counterpart: profit band below, stop band above.
    PositionShort,
    /// The price and time range between two anchors, drawn as the measure's
    /// box with the range written as a percentage as well as a delta.
    DatepriceRange,
}

impl DrawingKind {
    /// Every kind, in toolbar order: group by group, the way the references group
    /// them.
    ///
    /// The order is the flyout order, so a kind listed here in the wrong block is
    /// a tool that sits in the wrong menu -- and a kind missing is one the toolbar
    /// cannot offer at all.
    pub const ALL: [Self; 64] = [
        // Lines.
        Self::Trendline,
        Self::Ray,
        Self::Extended,
        Self::Hline,
        Self::Hray,
        Self::Vline,
        Self::Crossline,
        Self::Infoline,
        Self::Regression,
        Self::Arrow,
        Self::ArrowMarkUp,
        Self::ArrowMarkDown,
        Self::Angle,
        // Channels.
        Self::Channel,
        Self::DisjointChannel,
        Self::FlatTopBottom,
        // Pitchforks.
        Self::Pitchfork,
        Self::SchiffPitchfork,
        Self::ModifiedSchiffPitchfork,
        Self::InsidePitchfork,
        // Shapes.
        Self::Rect,
        Self::Triangle,
        Self::Circle,
        Self::Arc,
        // Fibonacci.
        Self::Fib,
        Self::FibExtension,
        Self::TrendFibExtension,
        Self::TrendFibTime,
        Self::FibTimeZone,
        Self::FibChannel,
        Self::FibFan,
        Self::Pitchfan,
        Self::FibArcs,
        Self::FibCircles,
        Self::FibSpiral,
        Self::FibWedge,
        // Gann.
        Self::GannBox,
        Self::GannFan,
        Self::GannSquare,
        Self::GannSquareFixed,
        // Patterns.
        Self::Xabcd,
        Self::AbcdPattern,
        Self::Cypher,
        Self::HeadShoulders,
        Self::ThreeDrives,
        Self::TrianglePattern,
        // Elliott.
        Self::ElliottImpulse,
        Self::ElliottCorrection,
        Self::ElliottTriangle,
        Self::ElliottDoubleCombo,
        Self::ElliottTripleCombo,
        // Cyclic.
        Self::CyclicLines,
        Self::TimeCycles,
        Self::SineLine,
        // Forecast and measurement.
        Self::PriceRange,
        Self::DateRange,
        Self::Forecast,
        Self::FixedRangeVP,
        Self::AnchoredVP,
        Self::AnchoredVWAP,

        // Measurement.
        Self::Measure,
        Self::DatepriceRange,
        // Positions.
        Self::PositionLong,
        Self::PositionShort,
    ];

    /// The wire name, which is also the storage's value and the shell's colour
    /// key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Trendline => "trendline",
            Self::Hline => "hline",
            Self::Vline => "vline",
            Self::Ray => "ray",
            Self::Extended => "extended",
            Self::Hray => "hray",
            Self::Crossline => "crossline",
            Self::Infoline => "infoline",
            Self::Regression => "regression",
            Self::Arrow => "arrow",
            Self::ArrowMarkUp => "arrow_mark_up",
            Self::ArrowMarkDown => "arrow_mark_down",
            Self::Rect => "rect",
            Self::DisjointChannel => "disjoint_channel",
            Self::FlatTopBottom => "flat_top_bottom",
            Self::Pitchfork => "pitchfork",
            Self::SchiffPitchfork => "schiff_pitchfork",
            Self::ModifiedSchiffPitchfork => "modified_schiff_pitchfork",
            Self::InsidePitchfork => "inside_pitchfork",
            Self::Fib => "fib",
            Self::FibExtension => "fib_extension",
            Self::TrendFibExtension => "trend_fib_extension",
            Self::TrendFibTime => "trend_fib_time",
            Self::FibTimeZone => "fib_time_zone",
            Self::FibChannel => "fib_channel",
            Self::FibFan => "fib_fan",
            Self::Pitchfan => "pitchfan",
            Self::FibArcs => "fib_arcs",
            Self::FibCircles => "fib_circles",
            Self::FibSpiral => "fib_spiral",
            Self::FibWedge => "fib_wedge",
            Self::GannBox => "gann_box",
            Self::GannFan => "gann_fan",
            Self::GannSquare => "gann_square",
            Self::GannSquareFixed => "gann_square_fixed",
            Self::Xabcd => "xabcd",
            Self::AbcdPattern => "abcd_pattern",
            Self::Cypher => "cypher",
            Self::HeadShoulders => "head_shoulders",
            Self::ThreeDrives => "three_drives",
            Self::TrianglePattern => "triangle_pattern",
            Self::ElliottImpulse => "elliott_impulse",
            Self::ElliottCorrection => "elliott_correction",
            Self::ElliottTriangle => "elliott_triangle",
            Self::ElliottDoubleCombo => "elliott_double_combo",
            Self::ElliottTripleCombo => "elliott_triple_combo",
            Self::CyclicLines => "cyclic_lines",
            Self::TimeCycles => "time_cycles",
            Self::SineLine => "sine_line",
            Self::PriceRange => "price_range",
            Self::DateRange => "date_range",
            Self::Forecast => "forecast",
            Self::FixedRangeVP => "fixed_range_vp",
            Self::AnchoredVP => "anchored_vp",
            Self::AnchoredVWAP => "anchored_vwap",
            Self::Measure => "measure",
            Self::Channel => "channel",
            Self::Angle => "angle",
            Self::Arc => "arc",
            Self::Circle => "circle",
            Self::Triangle => "triangle",
            Self::PositionLong => "position_long",
            Self::PositionShort => "position_short",
            Self::DatepriceRange => "dateprice_range",
        }
    }

    /// How many points this kind takes.
    ///
    /// The one arity rule. It lives beside the enum so it cannot drift from the
    /// list of kinds, and both the registry's `anchors` and `db::drawings`' copy
    /// are pinned against it by a test, because a `channel` the route believes
    /// stops at two points is a channel the chart cannot restore.
    #[must_use]
    pub const fn anchors(self) -> AnchorCount {
        // Every kind here takes a fixed count; the open-ended ones arrive with
        // the tools that need them (`Many { min: 2 }` for a polyline, a brush).
        match self {
            Self::Hline | Self::Vline | Self::Hray | Self::Crossline => AnchorCount::fixed(1),
            Self::ArrowMarkUp | Self::ArrowMarkDown => AnchorCount::fixed(1),
            Self::AnchoredVP | Self::AnchoredVWAP => AnchorCount::fixed(1),
            Self::Channel
            | Self::Arc
            | Self::Triangle
            | Self::FlatTopBottom
            | Self::Pitchfork
            | Self::SchiffPitchfork
            | Self::ModifiedSchiffPitchfork
            | Self::InsidePitchfork
            | Self::TrendFibExtension
            | Self::TrendFibTime
            | Self::FibChannel
            | Self::ElliottCorrection => AnchorCount::fixed(3),
            Self::DisjointChannel | Self::AbcdPattern | Self::TrianglePattern => {
                AnchorCount::fixed(4)
            }
            Self::Xabcd
            | Self::Cypher
            | Self::HeadShoulders
            | Self::ElliottImpulse
            | Self::ElliottTriangle
            | Self::ElliottDoubleCombo => AnchorCount::fixed(5),
            Self::ThreeDrives => AnchorCount::fixed(6),
            Self::ElliottTripleCombo => AnchorCount::fixed(7),
            Self::Trendline
            | Self::Ray
            | Self::Extended
            | Self::Infoline
            | Self::Regression
            | Self::Arrow
            | Self::Rect
            | Self::Fib
            | Self::FibExtension
            | Self::FibTimeZone
            | Self::FibFan
            | Self::Pitchfan
            | Self::FibArcs
            | Self::FibCircles
            | Self::FibSpiral
            | Self::FibWedge
            | Self::GannBox
            | Self::GannFan
            | Self::GannSquare
            | Self::GannSquareFixed
            | Self::CyclicLines
            | Self::TimeCycles
            | Self::SineLine
            | Self::PriceRange
            | Self::DateRange
            | Self::Forecast
            | Self::FixedRangeVP
            | Self::Measure
            | Self::Angle
            | Self::Circle
            | Self::PositionLong
            | Self::PositionShort
            | Self::DatepriceRange => AnchorCount::fixed(2),
        }
    }
}

/// The group a tool sits in, which is how the toolbar is organised.
///
/// A vocabulary, not free text: the shell renders one flyout per group in this
/// order, and a tool the shell has never heard of still lands in a flyout it
/// can draw rather than in an unlabeled pile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGroup {
    /// Lines: trendlines, rays, horizontals, verticals.
    Lines,
    /// Channels: two parallel edges and the band between them.
    Channels,
    /// Pitchforks: the median-line forks, all four templates.
    Pitchforks,
    /// Shapes: rectangles and anything with an area.
    Shapes,
    /// Fibonacci: the ladder and everything drawn at its ratios — levels,
    /// projections on the time axis, fans, arcs, spirals.
    Fibonacci,
    /// Gann: price and time held in proportion — the box, the fan, the square.
    Gann,
    /// Patterns: harmonic and structural shapes — XABCD, Head & Shoulders, Triangles.
    Patterns,
    /// Elliott: wave counts — impulse, corrections, triangles, combinations.
    Elliott,
    /// Cyclic: time oscillations — verticals, bands, sine curves.
    Cyclic,
    /// Measurement: the price/time ruler and the date-price range.
    Measurement,
    /// Positions: the long/short risk boxes a trade plan draws.
    Positions,
}

impl ToolGroup {
    /// Every group, in toolbar order.
    pub const ALL: [Self; 11] = [
        Self::Lines,
        Self::Channels,
        Self::Pitchforks,
        Self::Shapes,
        Self::Fibonacci,
        Self::Gann,
        Self::Patterns,
        Self::Elliott,
        Self::Cyclic,
        Self::Measurement,
        Self::Positions,
    ];

    /// The wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lines => "lines",
            Self::Channels => "channels",
            Self::Pitchforks => "pitchforks",
            Self::Shapes => "shapes",
            Self::Fibonacci => "fibonacci",
            Self::Gann => "gann",
            Self::Patterns => "patterns",
            Self::Elliott => "elliott",
            Self::Cyclic => "cyclic",
            Self::Measurement => "measurement",
            Self::Positions => "positions",
        }
    }

    /// What the toolbar calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lines => "Lines",
            Self::Channels => "Channels",
            Self::Pitchforks => "Forks",
            Self::Shapes => "Shapes",
            Self::Fibonacci => "Fibonacci",
            Self::Gann => "Gann",
            Self::Patterns => "Patterns",
            Self::Elliott => "Elliott",
            Self::Cyclic => "Cyclic",
            Self::Measurement => "Measure",
            Self::Positions => "Positions",
        }
    }
}

/// One tool the engine can draw, as the toolbar and the AI both read it.
///
/// The point of the registry (`docs/21`) is that this is **the only place a
/// tool is declared**. The shell builds its buttons from it over the ABI, the
/// storage's kind list is pinned against [`DrawingKind::ALL`] by a test, and
/// the AI's drawing vocabulary is this list -- so adding a tool is adding one
/// variant, one `shapes` arm, and one row here, and every consumer follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// What the tool draws, which is also the wire name.
    pub kind: DrawingKind,
    /// The short label on the button.
    pub label: &'static str,
    /// The tooltip: what a drag with this tool does.
    pub title: &'static str,
    /// Which flyout it sits in.
    pub group: ToolGroup,
    /// What the group's trigger reads. Carried on the row rather than derived
    /// by the shell, so the engine decides every word on the toolbar.
    pub group_label: &'static str,
    /// How many points placing it takes, which the shell's click counter reads.
    ///
    /// Carried on the row rather than asked of the kind, so one registry test can
    /// assert the two agree -- and if they disagree, the toolbar stops a click
    /// short of the drawing the engine would have drawn.
    pub anchors: AnchorCount,
}

/// The tools this engine can draw, in toolbar order.
///
/// The cursor is deliberately absent: it is not an object the engine can draw,
/// it is the shell's "select and pan" state, and a registry of *objects*
/// pretending to contain it would be one more thing to filter out.
pub const REGISTRY: [ToolSpec; 64] = [
    ToolSpec {
        kind: DrawingKind::Trendline,
        label: "Trend",
        title: "Drag from one point to another",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Ray,
        label: "Ray",
        title: "Drag, and the line extends through the second point to the edge",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Extended,
        label: "Ext line",
        title: "Drag; the line runs through both points in both directions",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Hline,
        label: "H-line",
        title: "Click a price to mark it across the chart",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Hray,
        label: "H-ray",
        title: "Click a price; the level runs right from there",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Vline,
        label: "V-line",
        title: "Click a time to mark it down the chart",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Crossline,
        label: "Cross",
        title: "Click once; a price line and a time line cross there",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Infoline,
        label: "Info line",
        title: "Drag between two points; both prices and the bars between them are written",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Regression,
        label: "Regression",
        title: "Drag over a range; the least-squares line through its closes",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Arrow,
        label: "Arrow",
        title: "Drag from one point to another; the head lands on the second",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::ArrowMarkUp,
        label: "Up mark",
        title: "Click to stamp an up arrow at that bar",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::ArrowMarkDown,
        label: "Down mark",
        title: "Click to stamp a down arrow at that bar",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Rect,
        label: "Rect",
        title: "Drag out a rectangle",
        group: ToolGroup::Shapes,
        group_label: "Shapes",
        anchors: AnchorCount::fixed(2),
    },
    // Fibonacci: the ladder, its projections on both axes, and the shapes drawn
    // at its ratios.
    ToolSpec {
        kind: DrawingKind::Fib,
        label: "Fib",
        title: "Drag from one swing to another",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibExtension,
        label: "Fib ext",
        title: "Drag from one swing to another — targets project past the second point",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::TrendFibExtension,
        label: "Trend ext",
        title: "Impulse, its end, then the pullback — targets run on the trend's own slope",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::TrendFibTime,
        label: "Trend time",
        title: "The same three points, with the ratios read on the time axis",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::FibTimeZone,
        label: "Time zones",
        title: "Two points set the interval; vertical lines fall at its ratios",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibChannel,
        label: "Fib channel",
        title: "The line through the first two points, its parallel through the third, divided by the ladder",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::FibFan,
        label: "Fib fan",
        title: "Speed-resistance fan: lines through 38.2, 50 and 61.8 of the swing's range",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Pitchfan,
        label: "Pitchfan",
        title: "Rays from the pivot at the ladder's coefficients of the slope",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibArcs,
        label: "Fib arcs",
        title: "Speed-resistance arcs: the same divisions, curved around each end of the swing",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibCircles,
        label: "Fib circles",
        title: "Concentric circles at the ladder's ratios of the swing's price distance",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibSpiral,
        label: "Fib spiral",
        title: "A logarithmic spiral from the pivot, a quarter turn wider by φ",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FibWedge,
        label: "Fib wedge",
        title: "The swing's line with the ladder opening on each side of it",
        group: ToolGroup::Fibonacci,
        group_label: "Fibonacci",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::GannBox,
        label: "Gann box",
        title: "Drag the two corners; the box is divided into eighths with its 1×1 angle",
        group: ToolGroup::Gann,
        group_label: "Gann",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::GannFan,
        label: "Gann fan",
        title: "Drag to set the 1×1; the fan draws 1×16, 1×8, 1×4, 1×2, 2×1, 4×1, 8×1 and 16×1",
        group: ToolGroup::Gann,
        group_label: "Gann",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::GannSquare,
        label: "Gann sq",
        title: "Drag the square's side; the eighths, the fan through its centre and the arcs",
        group: ToolGroup::Gann,
        group_label: "Gann",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::GannSquareFixed,
        label: "Gann fixed",
        title: "Levels 0 through 5 at the unit square the second point sets",
        group: ToolGroup::Gann,
        group_label: "Gann",
        anchors: AnchorCount::fixed(2),
    },
    // Patterns.
    ToolSpec {
        kind: DrawingKind::Xabcd,
        label: "XABCD",
        title: "Five pivots: harmonic ratios and pattern recognition (Gartley, Bat, Crab, Butterfly, Shark)",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::AbcdPattern,
        label: "ABCD",
        title: "Four pivots: a custom-ratio ABCD pattern",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(4),
    },
    ToolSpec {
        kind: DrawingKind::Cypher,
        label: "Cypher",
        title: "Five pivots: the Cypher harmonic (0.382-0.786 XA, 1.13-1.414 AB)",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::HeadShoulders,
        label: "H&S",
        title: "Five pivots: left shoulder, head, right shoulder, and two neckline points",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::ThreeDrives,
        label: "Drives",
        title: "Six pivots: three impulse legs with their retracements",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(6),
    },
    ToolSpec {
        kind: DrawingKind::TrianglePattern,
        label: "Tri patt",
        title: "Four pivots: converging trendlines from higher lows and lower highs",
        group: ToolGroup::Patterns,
        group_label: "Patterns",
        anchors: AnchorCount::fixed(4),
    },
    // Elliott.
    ToolSpec {
        kind: DrawingKind::ElliottImpulse,
        label: "Impulse",
        title: "Five pivots: a 1-2-3-4-5 impulse wave sequence",
        group: ToolGroup::Elliott,
        group_label: "Elliott",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::ElliottCorrection,
        label: "Zigzag",
        title: "Three pivots: an A-B-C corrective wave",
        group: ToolGroup::Elliott,
        group_label: "Elliott",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::ElliottTriangle,
        label: "E.Tri",
        title: "Five pivots: an A-B-C-D-E triangle (contracting or expanding)",
        group: ToolGroup::Elliott,
        group_label: "Elliott",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::ElliottDoubleCombo,
        label: "Double",
        title: "Five pivots: W-X-Y-X-Z double zigzag",
        group: ToolGroup::Elliott,
        group_label: "Elliott",
        anchors: AnchorCount::fixed(5),
    },
    ToolSpec {
        kind: DrawingKind::ElliottTripleCombo,
        label: "Triple",
        title: "Seven pivots: W-X-Y-X-Z-X-A triple zigzag",
        group: ToolGroup::Elliott,
        group_label: "Elliott",
        anchors: AnchorCount::fixed(7),
    },
    // Cyclic.
    ToolSpec {
        kind: DrawingKind::CyclicLines,
        label: "Cycle L",
        title: "Two anchors: vertical lines repeating at the defined interval into the future",
        group: ToolGroup::Cyclic,
        group_label: "Cyclic",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::TimeCycles,
        label: "Cycle B",
        title: "Two anchors: alternating shaded time bands from the defined period",
        group: ToolGroup::Cyclic,
        group_label: "Cyclic",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::SineLine,
        label: "Sine",
        title: "Two anchors: a sine curve whose period and amplitude are set by the drag",
        group: ToolGroup::Cyclic,
        group_label: "Cyclic",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::PriceRange,
        label: "P band",
        title: "Two prices: a horizontal band spanning the chart",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::DateRange,
        label: "T band",
        title: "Two times: a vertical band spanning the chart height",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Forecast,
        label: "Forecast",
        title: "Drag from origin to target: shows the projected time and price",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::FixedRangeVP,
        label: "VP range",
        title: "Two times: volume profile histogram for the bars in that range",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::AnchoredVP,
        label: "VP anchor",
        title: "Click: volume profile from the anchor bar to the right edge",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::AnchoredVWAP,
        label: "AVWAP",
        title: "Click: anchored VWAP line from that bar through all subsequent closes",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(1),
    },
    ToolSpec {
        kind: DrawingKind::Measure,
        label: "Ruler",
        title: "Drag to measure the price and time delta",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Angle,
        label: "Angle",
        title: "Click the origin, then drag to set the slope",
        group: ToolGroup::Lines,
        group_label: "Lines",
        anchors: AnchorCount::fixed(2),
    },
    // Channels.
    ToolSpec {
        kind: DrawingKind::Channel,
        label: "Channel",
        title: "Drag the line, then click where the parallel runs",
        group: ToolGroup::Channels,
        group_label: "Channels",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::DisjointChannel,
        label: "Disjoint",
        title: "Four clicks: two parallel edges that start and end apart",
        group: ToolGroup::Channels,
        group_label: "Channels",
        anchors: AnchorCount::fixed(4),
    },
    ToolSpec {
        kind: DrawingKind::FlatTopBottom,
        label: "Flat band",
        title: "Drag the angled line, then click the horizontal boundary",
        group: ToolGroup::Channels,
        group_label: "Channels",
        anchors: AnchorCount::fixed(3),
    },
    // Pitchforks.
    ToolSpec {
        kind: DrawingKind::Pitchfork,
        label: "Fork",
        title: "Three pivots; the median runs through the midpoint of the last two",
        group: ToolGroup::Pitchforks,
        group_label: "Forks",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::SchiffPitchfork,
        label: "Schiff",
        title: "The fork with its handle shifted halfway to the second pivot in price",
        group: ToolGroup::Pitchforks,
        group_label: "Forks",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::ModifiedSchiffPitchfork,
        label: "Mod Schiff",
        title: "The fork with its handle shifted halfway to the second pivot in price and time",
        group: ToolGroup::Pitchforks,
        group_label: "Forks",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::InsidePitchfork,
        label: "Inside fork",
        title: "The fork shrunk toward its handle: tines through the midpoints of its own arms",
        group: ToolGroup::Pitchforks,
        group_label: "Forks",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::Arc,
        label: "Arc",
        title: "Click the centre, then set the horizontal and vertical radii",
        group: ToolGroup::Shapes,
        group_label: "Shapes",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::Circle,
        label: "Circle",
        title: "Click the centre, then drag the radius",
        group: ToolGroup::Shapes,
        group_label: "Shapes",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::Triangle,
        label: "Triangle",
        title: "Click each of the three corners",
        group: ToolGroup::Shapes,
        group_label: "Shapes",
        anchors: AnchorCount::fixed(3),
    },
    ToolSpec {
        kind: DrawingKind::PositionLong,
        label: "Long",
        title: "Click the entry, then drag the target extent upward",
        group: ToolGroup::Positions,
        group_label: "Positions",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::PositionShort,
        label: "Short",
        title: "Click the entry, then drag the target extent downward",
        group: ToolGroup::Positions,
        group_label: "Positions",
        anchors: AnchorCount::fixed(2),
    },
    ToolSpec {
        kind: DrawingKind::DatepriceRange,
        label: "Range",
        title: "Drag to measure the price and time range as a percentage",
        group: ToolGroup::Measurement,
        group_label: "Measure",
        anchors: AnchorCount::fixed(2),
    },
];

/// Where one end of a drawing is, and in what unit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Anchor {
    /// A millisecond timestamp and a price: what gets stored.
    Absolute {
        /// Milliseconds since the epoch.
        time: f64,
        /// The price.
        price: f64,
    },
    /// A position on the plot as a fraction of it: `0.0` at the left or top
    /// edge, `1.0` at the right or bottom.
    ///
    /// The same number the viewport gestures carry, and for the same reason: it
    /// is what a pointer position gives you without knowing anything about the
    /// price scale.
    Fraction {
        /// Across the plot.
        x: f64,
        /// Down the plot.
        y: f64,
    },
}

impl Anchor {
    /// The millisecond time and the price, when this is an absolute anchor.
    ///
    /// `None` for a fraction, rather than a guess: a fraction's `x` is not a
    /// time, and the whole point of the two variants is that the difference
    /// cannot be papered over.
    #[must_use]
    pub const fn absolute(self) -> Option<(f64, f64)> {
        match self {
            Self::Absolute { time, price } => Some((time, price)),
            Self::Fraction { .. } => None,
        }
    }
}

/// What the user asked the shape to do, beyond where its points are.
///
/// Typed, closed, and **every field read by at least one geometry arm** -- that
/// is the rule (D4 in the parity spec), and the completeness gate enforces it,
/// because an option nothing reads is a control on a settings panel that does
/// nothing. So a field arrives with the arm that reads it and not before: the
/// fib ratio profile comes with the Fibonacci group, the polyline's `closed`
/// with the open path kinds, and the volume-profile numbers with the profile
/// tools.
///
/// The defaults are the references', not ours: a hand-placed tool that disagrees
/// with TradingView's default output is a tool the trader has to reconfigure
/// every time they use it.
///
/// Serde-defaulted throughout, so a request that sends none of them is the
/// documented behaviour rather than a 422: the shell sends options only when the
/// user has changed them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct DrawingOptions {
    /// Which side of the points the shape runs to.
    ///
    /// TradingView's *Extend Left* / *Extend Right* on the line and channel
    /// tools, where each drawn edge can be pushed to the plot's own edge without
    /// moving the anchor it came from.
    ///
    /// `None` means "whatever this kind's reference does by default": a ray
    /// extends forward, a trendline does not extend at all, and a geometry arm
    /// that has a documented default should not have it contradicted by an
    /// absent field.
    pub extend: Option<Extend>,
    /// Whether to read the impulse the other way round.
    ///
    /// TradingView's *Reverse* on the retracement: the same two points, the
    /// opposite direction, so a 0 and a 100 swap sides instead of the ladder
    /// being re-drawn from a re-dragged swing.
    pub reverse: bool,
    /// Which ladder the Fibonacci tools read.
    ///
    /// The reference packages all spell it the same way — a retracement, an
    /// extension and an optimal-trade-entry grid are three *ratio sets* over one
    /// swing, not three tools. Pass 5 of the concept audit found the same set
    /// drawn as an anonymous dash, so a level the trader cannot name is a level
    /// they cannot use: the profile decides the ratios, and every one of them is
    /// labelled with its ratio and its price.
    ///
    /// `None` is each kind's own default — the retracement reads [`LevelProfile::Classic`],
    /// the projections read [`LevelProfile::Extension`], the time tools read
    /// [`LevelProfile::Time`] — so the field changes the ladder without moving
    /// any kind off its reference.
    pub level_profile: Option<LevelProfile>,
}

/// The ratio set a Fibonacci tool draws.
///
/// Four named ladders rather than a free list of numbers, because the numbers
/// are the claim: a grid drawn at 0.705 says "optimal trade entry", and one
/// drawn at 0.236-to-0.786 says "retracement". A user-typed list would make the
/// engine a spreadsheet, and the audit's finding was that the *named* set is
/// what a trader reads off the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelProfile {
    /// The retracement ladder: [`FIB_LEVELS`].
    Classic,
    /// The projection ladder, past the swing's end: [`FIB_EXTENSION_LEVELS`].
    Extension,
    /// The optimal-trade-entry band: [`FIB_OTE_LEVELS`].
    Ote,
    /// The ladder the time tools read, including the extensions of the leg:
    /// [`FIB_TIME_LEVELS`].
    Time,
}

impl LevelProfile {
    /// Every profile, in the order a settings panel would list them.
    pub const ALL: [Self; 4] = [Self::Classic, Self::Extension, Self::Ote, Self::Time];

    /// The wire name, which is the storage's value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Extension => "extension",
            Self::Ote => "ote",
            Self::Time => "time",
        }
    }

    /// The ladder this profile means.
    #[must_use]
    pub const fn levels(self) -> &'static [(f64, &'static str)] {
        match self {
            Self::Classic => &FIB_LEVELS,
            Self::Extension => &FIB_EXTENSION_LEVELS,
            Self::Ote => &FIB_OTE_LEVELS,
            Self::Time => &FIB_TIME_LEVELS,
        }
    }
}

/// How far a line or level runs past the points that define it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Extend {
    /// Stop at the points -- the plain segment.
    None,
    /// Run to the plot's left edge from the first point.
    Left,
    /// Run to the plot's right edge from the last.
    Right,
    /// Both, which is the infinite line through the points.
    Both,
}

/// One drawing, as a request carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    /// The caller's id for it. Stable across frames, and the row id once saved.
    pub id: String,
    /// What to put between the points.
    pub kind: DrawingKind,
    /// The points, in the order they were placed.
    ///
    /// A list rather than three named fields, because the vocabulary grew past
    /// three: a polyline has as many as the user clicks, and a harmonic pattern
    /// has six. The order is the placement order and the geometry reads it --
    /// `points[0]` is where the drawing started, which is what a ray, a
    /// projection and a pattern's X all mean by "first".
    pub points: Vec<Anchor>,
    /// What the user called it, if anything.
    #[serde(default)]
    pub label: Option<String>,
    /// Whether the caller has this one selected.
    ///
    /// Only the selected drawing gets handles. Every drawing's anchors at once
    /// is visual noise, and it invites a drag nobody can aim.
    #[serde(default)]
    pub selected: bool,
    /// What the user asked the shape to do beyond where it sits.
    #[serde(default)]
    pub options: DrawingOptions,
}

impl Drawing {
    /// Why this drawing's points cannot be drawn, if they cannot.
    ///
    /// Total, and it names the problem rather than answering a `bool`: the
    /// reason goes into the scene's note with the drawing's id, because a
    /// drawing that silently does not appear teaches whoever drew it nothing.
    ///
    /// Points only, and not the id. The id is the shell's and the scene's --
    /// storage mints its own -- so a caller that is validating a request body has
    /// no id to give and should not have to invent one. `place` checks it, which
    /// is the only place that needs it.
    ///
    /// # Errors
    /// Returns the reason as a `String`.
    pub fn validate_anchors(&self) -> Result<(), String> {
        let arity = self.kind.anchors();
        if !arity.accepts(self.points.len()) {
            return Err(match arity {
                AnchorCount::Fixed { points } => format!(
                    "a {} takes {points} point{} and this has {}",
                    self.kind.name(),
                    if points == 1 { "" } else { "s" },
                    self.points.len()
                ),
                AnchorCount::Many { min } => format!(
                    "a {} takes at least {min} points and this has {}",
                    self.kind.name(),
                    self.points.len()
                ),
            });
        }
        // One unit per drawing. The two variants are the same two numbers with
        // different names, so a list that mixes them is not a shape: it is a
        // client that turned a fraction into a timestamp somewhere the engine
        // never agreed to, which is exactly the mistake the names exist to make
        // visible.
        let absolute = self.points[0].absolute().is_some();
        for (index, anchor) in self.points.iter().enumerate() {
            if anchor.absolute().is_some() != absolute {
                return Err(format!(
                    "point {} of this {} is a screen position among chart points; \
                     send every point in one unit",
                    index + 1,
                    self.kind.name()
                ));
            }
            let ok = match anchor {
                // `is_finite` is false for both `NaN` and the infinities, which
                // is the whole check: a `NaN` price produces a `NaN` y and the
                // canvas silently ignores it, so the drawing would simply be
                // absent with nothing anywhere saying why.
                Anchor::Absolute { time, price } => time.is_finite() && price.is_finite(),
                Anchor::Fraction { x, y } => x.is_finite() && y.is_finite(),
            };
            if !ok {
                return Err(format!(
                    "point {} is not a usable position",
                    index + 1
                ));
            }
        }
        Ok(())
    }
}

/// The Fibonacci ratios drawn, and the label each gets.
///
/// The retracement set and nothing else: the extensions (1.272, 1.618) are a
/// different drawing with a different meaning, and shipping them under the same
/// name would be a chart that quietly says something the user did not ask for.
///
/// The labels are strings rather than numbers formatted at draw time, because
/// `61.8` is a name for a level and `0.618` is the arithmetic behind it, and
/// `docs/14` keeps arithmetic out of JavaScript.
pub const FIB_LEVELS: [(f64, &str); 7] = [
    (0.0, "0"),
    (0.236, "23.6"),
    (0.382, "38.2"),
    (0.5, "50"),
    (0.618, "61.8"),
    (0.786, "78.6"),
    (1.0, "100"),
];

/// The projection ladder: the swing marked 0 and 100, then the targets beyond it.
///
/// The four the references agree on (127.2, 161.8, 200, 261.8) plus 361.8, which
/// is the one a trader reaches for when a move runs: the ladder is a geometric
/// series of the leg, so the set keeps going and the tool has to stop somewhere
/// named rather than somewhere arbitrary.
pub const FIB_EXTENSION_LEVELS: [(f64, &str); 7] = [
    (0.0, "0"),
    (1.0, "100"),
    (1.272, "127.2"),
    (1.618, "161.8"),
    (2.0, "200"),
    (2.618, "261.8"),
    (3.618, "361.8"),
];

/// The optimal-trade-entry band: the deep half of a retracement, where a
/// pullback is expected to be bought rather than merely survived.
///
/// 70.5 and 88.6 are not Fibonacci ratios; they are the two levels this school
/// reads off the same grid, and the reason `ote` is a *profile* rather than a
/// longer classic ladder — a trader asking for OTE wants these five and nothing
/// above 50, which is exactly what makes the shape readable.
pub const FIB_OTE_LEVELS: [(f64, &str); 5] = [
    (0.5, "50"),
    (0.618, "61.8"),
    (0.705, "70.5"),
    (0.786, "78.6"),
    (0.886, "88.6"),
];

/// The ladder the time tools read.
///
/// The retracement ratios, then the extensions of the leg: a swing's *duration*
/// projects the same way its price does, and 127.2 / 161.8 bars after the anchor
/// are the two windows a trader watches for the next turn. There is no 0, because
/// a time tool at ratio 0 is the anchor itself — a vertical line the user can
/// already see.
pub const FIB_TIME_LEVELS: [(f64, &str); 7] = [
    (0.236, "23.6"),
    (0.382, "38.2"),
    (0.5, "50"),
    (0.618, "61.8"),
    (0.786, "78.6"),
    (1.272, "127.2"),
    (1.618, "161.8"),
];

/// One positioned shape.
///
/// Geometry, not instructions: every coordinate is already in canvas pixels and
/// every choice is already made, so the shell iterates and strokes. That is
/// `docs/14`'s arrangement for the whole scene, and drawings are not an
/// exception -- it is what keeps the price scale in Rust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum DrawingPart {
    /// A line between two canvas points.
    Segment {
        /// Start, across.
        x1: f64,
        /// Start, down.
        y1: f64,
        /// End, across.
        x2: f64,
        /// End, down.
        y2: f64,
        /// Whether to stroke it dashed.
        dashed: bool,
    },
    /// A rectangle.
    Rect {
        /// Left edge.
        x: f64,
        /// Top edge.
        y: f64,
        /// Width.
        w: f64,
        /// Height.
        h: f64,
        /// Whether to fill it as well as outline it.
        filled: bool,
    },
    /// A label at a canvas point.
    Text {
        /// Left of the text.
        x: f64,
        /// Baseline.
        y: f64,
        /// Ready to draw.
        text: String,
    },
    /// A grab point, at an anchor.
    ///
    /// Emitted so the shell's hit-test is a distance between two screen points
    /// rather than a re-derivation of where the anchors are. It is also why the
    /// shell cannot offer a grab point the engine would not honour.
    Handle {
        /// Across.
        x: f64,
        /// Down.
        y: f64,
        /// Which point, counting from the first in placement order.
        anchor: u8,
    },
    /// An axis-aligned ellipse: a circle when the radii are equal, a half
    /// ellipse when `half` is set (the upper arc only).
    ///
    /// The parity vocabulary's arc and circle are the emitters. A `Segment`
    /// polyline approximating an ellipse would put the curve's arithmetic in
    /// the shell, which is exactly what the scene exists to keep out.
    Ellipse {
        /// Centre, across.
        cx: f64,
        /// Centre, down.
        cy: f64,
        /// Horizontal radius.
        rx: f64,
        /// Vertical radius.
        ry: f64,
        /// Draw only the upper arc rather than the whole ellipse.
        half: bool,
        /// Whether to fill it as well as outline it.
        filled: bool,
    },
    /// A closed polygon through three or more points, in order.
    ///
    /// The triangle is the emitter today. Points are `(x, y)` pairs in canvas
    /// pixels, already ordered the way they should be stroked.
    Polygon {
        /// The vertices, in stroke order.
        points: Vec<(f64, f64)>,
        /// Whether to fill it as well as outline it.
        filled: bool,
    },
}

/// A point in plot fractions: across the plot, and down it.
///
/// The output counterpart of [`Anchor::Fraction`], and a separate type on
/// purpose. An `Anchor` is a position the caller may give in *either* unit; this
/// is always a fraction, so a `SceneDrawing` field of this type cannot report a
/// timestamp where the shell expects a fraction. The two are the same two
/// numbers and would be one type if the units could not be confused, which is
/// exactly what they can be.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fraction {
    /// Across the plot: 0 at the left edge, 1 at the right.
    pub x: f64,
    /// Down the plot: 0 at the top, 1 at the bottom.
    pub y: f64,
}

/// One placed point, in both units the shell can use.
///
/// Both, always, and that is the whole type: the absolute pair is what gets
/// **stored**, and the fraction pair is what a **body drag** adds to. A scene
/// that reported only one of them would leave the shell inverting the price
/// mapping -- which is arithmetic, and `docs/14` says arithmetic lives in Rust.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScenePoint {
    /// Where it is on the chart, which is what outlives this window.
    pub absolute: PlacedTimePrice,
    /// Where it is on the plot, which is what a screen delta is measured in.
    ///
    /// Derived from the resolved numbers rather than passed down from the request,
    /// so a point that arrived absolute and one that arrived as a fraction report
    /// the same thing. That is what makes the round trip -- place with fractions,
    /// save the answer, do not move the drawing -- hold for both, and it is why
    /// there is one `resolve` and not two.
    pub fraction: Fraction,
}

/// A millisecond timestamp and a price, as the scene reports them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlacedTimePrice {
    /// Milliseconds since the epoch.
    pub time: f64,
    /// The price.
    pub price: f64,
}

/// One drawing, positioned, with its points resolved.
///
/// Carries the points as well as the shapes, and that is the point: the shell
/// needs the absolute form to **save** what the user just dragged. A scene that
/// reported only pixels would leave the shell with no way to persist a change
/// except to invert the price mapping, which is arithmetic and is forbidden
/// there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneDrawing {
    /// The caller's id, echoed.
    pub id: String,
    /// What shape it is, which is also the shell's colour key.
    pub kind: DrawingKind,
    /// What the user called it.
    pub label: Option<String>,
    /// Whether it is selected, so the shell can highlight it.
    pub selected: bool,
    /// The points, in placement order, each in both units.
    ///
    /// The same length the kind declares -- `place` refused any other.
    pub points: Vec<ScenePoint>,
    /// The shapes to draw, in order.
    pub parts: Vec<DrawingPart>,
}

/// A level an *answer* put on the chart, as a request carries it.
///
/// ## Why this is not a `Drawing`
///
/// A `Drawing` is the user's mark: it has an id, a selection, a handle to grab,
/// and it is stored. An overlay is a claim the agent made -- "entry", "stop",
/// "target", "a level I cited" -- and it is derived fresh from every answer. It
/// has no id because nothing persists it, and no handles because the user did
/// not place it and must not drag it: dragging the AI's stop would silently
/// change what the answer said.
///
/// ## Why it carries absolute prices
///
/// The shell already draws the thesis, and it does so with its own `y = plot - …`
/// arithmetic -- a second copy of the price scale that is also the one thing
/// `docs/14` says must live in Rust. The copy drifts: resize, zoom, or a mode
/// change and the band is a few pixels off the candles it describes, which reads
/// as "the level moved" rather than "the overlay is stale".
///
/// So the shell sends the *prices* and the engine resolves the coordinates. That
/// is the same split [`crate::drawing::Anchor`] uses for the user's shapes, and
/// it is why an overlay is a price and a name rather than a pair of points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// The price it sits at, in absolute price units.
    pub price: f64,
    /// The label: `entry`, `stop`, `target`, or an answer's own words.
    pub label: String,
    /// The role, which is also the shell's colour key and its z-order.
    ///
    /// A closed vocabulary rather than free text, for the same reason
    /// [`DrawingKind`] is: a role the palette has no colour for is a level that
    /// draws in the fallback colour and means nothing. [`OverlayRole::Other`]
    /// exists so an answer can say something the vocabulary does not cover, and
    /// it is deliberately the *last* colour, not a refusal.
    pub role: OverlayRole,
    /// The far end of a band, when this overlay is an area rather than a line.
    ///
    /// `Some` for a stop-to-target band, `None` for a single level. A separate
    /// field rather than a second overlay so the band cannot be drawn from two
    /// overlays that disagree -- and so the engine can order the two edges
    /// itself, since a short's stop is above its entry and a long's is below.
    #[serde(default)]
    pub band_to: Option<f64>,
    /// Whether to shade the band as well as outline it.
    #[serde(default)]
    pub filled: bool,
}

impl Overlay {
    /// Why this overlay cannot be drawn, if it cannot.
    ///
    /// The same rule [`Drawing::validate_anchors`] applies, for the same reason:
    /// a `NaN` price maps to a `NaN` y and the canvas silently drops the shape,
    /// so the level would simply be absent with nothing saying why. A `NaN` here
    /// is not a far-fetched case -- it is what an answer with a missing field
    /// produces once anything does arithmetic on it.
    ///
    /// # Errors
    /// Returns the reason as a `String`.
    pub fn validate(&self) -> Result<(), String> {
        if !self.price.is_finite() {
            return Err(format!(
                "the `{}` overlay has a price that is not a number",
                self.label
            ));
        }
        if let Some(far) = self.band_to {
            if !far.is_finite() {
                return Err(format!(
                    "the `{}` overlay's band ends at something that is not a number",
                    self.label
                ));
            }
        }
        Ok(())
    }

    /// The two prices this overlay spans, low first.
    ///
    /// Ordered here rather than at draw time, because "which edge is on top" is
    /// a question about the direction of the trade and the engine is who knows
    /// it. A caller that passed a band as `(stop, target)` on a short would get
    /// an inside-out rectangle from any code that assumed `a < b`.
    #[must_use]
    pub fn bounds(&self) -> (f64, f64) {
        match self.band_to {
            Some(far) if far < self.price => (far, self.price),
            Some(far) => (self.price, far),
            None => (self.price, self.price),
        }
    }
}

/// What an overlay *is*, which decides its colour and nothing else.
///
/// The scene never reads this to make a decision -- it carries it so the shell
/// can pick a colour without inventing a mapping from label text. That split is
/// deliberate: a shell that matched on the label would colour "Stop Loss" as
/// `other` the first time an answer phrased it differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayRole {
    /// Where the trade would be entered.
    Entry,
    /// Where it would be abandoned.
    Stop,
    /// Where it would be taken off.
    Target,
    /// A level the answer cited that is not one of the three above.
    Level,
    /// Something the vocabulary does not cover.
    ///
    /// Present rather than refused, and the distinction matters: an answer that
    /// says "watch 101250 for a reclaim" is making a real claim about a real
    /// price, and dropping it because it is not an entry, stop or target would
    /// hide the one line the user was told to watch.
    Other,
}

impl OverlayRole {
    /// The wire name, which is also the shell's colour key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Stop => "stop",
            Self::Target => "target",
            Self::Level => "level",
            Self::Other => "other",
        }
    }

    /// Everything, for a client that wants to build a legend.
    pub const ALL: [Self; 5] = [
        Self::Entry,
        Self::Stop,
        Self::Target,
        Self::Level,
        Self::Other,
    ];
}

/// One overlay, positioned.
///
/// The output counterpart of [`Overlay`], and the same reason [`SceneDrawing`]
/// exists: every coordinate is already in canvas pixels and every ordering
/// decision is already made, so the shell strokes and adds nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneOverlay {
    /// Canvas y of the level itself.
    pub y: f64,
    /// The price it marks, echoed so the shell can label without re-deriving.
    pub price: f64,
    /// The label to draw.
    pub label: String,
    /// The role, for colour.
    pub role: OverlayRole,
    /// Canvas y of the far edge of the band, when there is one.
    ///
    /// `None` for a single level. Present so the shell can shade without
    /// comparing two pixels whose order it would have to guess at.
    pub band_y: Option<f64>,
    /// Whether to shade it.
    pub filled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(price: f64, band_to: Option<f64>) -> Overlay {
        Overlay {
            price,
            label: "entry".into(),
            role: OverlayRole::Entry,
            band_to,
            filled: false,
        }
    }

    #[test]
    fn an_overlay_band_is_ordered_low_first() {
        // A short's stop is above its entry and a long's is below. Both reach
        // `bounds`, and both have to come back low-first or the rectangle is
        // inside out -- which draws a band that shades the wrong region and
        // looks like the answer was wrong rather than the renderer.
        assert_eq!(overlay(100.0, Some(90.0)).bounds(), (90.0, 100.0));
        assert_eq!(overlay(100.0, Some(110.0)).bounds(), (100.0, 110.0));
    }

    #[test]
    fn a_single_level_spans_nothing() {
        // Not `(price, 0.0)` and not `(0.0, price)`: a level with no band has to
        // be distinguishable from a band down to zero, because one of those is
        // a line and the other is a rectangle over the whole chart.
        assert_eq!(overlay(100.0, None).bounds(), (100.0, 100.0));
    }

    #[test]
    fn a_level_that_is_not_a_number_is_refused_by_name() {
        // The failure this prevents: a `NaN` y, which the canvas silently skips.
        // The level would be absent and nothing would say why -- so the answer
        // would look like it had no stop.
        //
        // Reachability: *not* from the shell. JSON has no `NaN`, and a `null`
        // price is rejected by serde before this runs -- see the
        // `nan_reachability` module below, which asserts exactly that. This
        // guards the direct-construction route: Rust code, a test, or a future
        // transport that is not JSON.
        let bad = overlay(f64::NAN, None);
        let error = bad.validate().expect_err("must refuse");
        assert!(error.contains("entry"), "{error}");

        let bad_band = overlay(100.0, Some(f64::INFINITY));
        let error = bad_band.validate().expect_err("must refuse");
        assert!(error.contains("band"), "{error}");
    }

    #[test]
    fn an_overlay_accepts_every_role_and_is_never_refused_for_one() {
        // `Other` is the interesting case. An answer that cites a level the
        // vocabulary does not cover is making a real claim about a real price,
        // and refusing it would hide the one line the user was told to watch.
        for role in OverlayRole::ALL {
            let candidate = Overlay {
                role,
                ..overlay(100.0, None)
            };
            assert_eq!(candidate.validate(), Ok(()), "{role:?}");
        }
    }

    #[test]
    fn every_overlay_role_name_is_a_snake_case_wire_value() {
        // The shell switches on this string for the colour, so it has to be on
        // the wire and it has to match `name()`.
        for role in OverlayRole::ALL {
            let json = serde_json::to_value(role).expect("serializes");
            assert_eq!(json, role.name(), "{role:?} renamed on the wire");
        }
    }

    #[test]
    fn the_overlay_wire_is_pinned() {
        // Four consumers depend on these spellings: the shell that draws them,
        // the shell that *sends* them, the API route that builds them from a
        // thesis, and the tests that read the scene. A rename here is not a
        // compile error anywhere -- it is an overlay that never appears.
        let json = serde_json::to_value(Overlay {
            price: 45_000.0,
            label: "stop".into(),
            role: OverlayRole::Stop,
            band_to: Some(44_500.0),
            filled: true,
        })
        .expect("serializes");
        assert_eq!(json["price"], 45_000.0);
        assert_eq!(json["label"], "stop");
        assert_eq!(json["role"], "stop");
        assert_eq!(json["band_to"], 44_500.0);
        assert_eq!(json["filled"], true);
    }

    #[test]
    fn an_overlay_that_omits_its_optional_fields_still_parses() {
        // `band_to` and `filled` are `default`, and a request that sends only a
        // price and a label must not be a 422. A shell that later drops the
        // optional fields should degrade to a plain line, not to nothing.
        let parsed: Overlay = serde_json::from_value(serde_json::json!({
            "price": 100.0, "label": "vwap", "role": "level"
        }))
        .expect("parses without the optional fields");
        assert_eq!(parsed.price, 100.0);
        assert_eq!(parsed.band_to, None);
        assert!(!parsed.filled);
    }

    #[test]
    fn a_scene_overlay_serializes_with_its_own_field_names() {
        // Distinct from `Overlay` on purpose: this one has `y`, and a shell that
        // read `price` and tried to stroke at it would draw every level at the
        // top-left corner.
        let json = serde_json::to_value(SceneOverlay {
            y: 120.5,
            price: 45_000.0,
            label: "entry".into(),
            role: OverlayRole::Entry,
            band_y: Some(90.0),
            filled: true,
        })
        .expect("serializes");
        assert_eq!(json["y"], 120.5);
        assert_eq!(json["band_y"], 90.0);
        assert_eq!(json["role"], "entry");
        assert!(
            json["price"].is_number(),
            "the price is echoed for the label"
        );
    }

    fn point(index: u8) -> Anchor {
        // Distinct in both coordinates for every index, so a misplaced offset
        // (channel ray, arc ry, fork median) cannot pass a test by coincidence.
        Anchor::Absolute {
            time: 1_767_225_600_000.0 + f64::from(index) * 3_600_000.0,
            price: 45_000.0 + f64::from(index) * 500.0,
        }
    }

    /// A drawing of `kind` with exactly the points its kind declares.
    fn drawing(kind: DrawingKind) -> Drawing {
        let count = kind.anchors().minimum();
        Drawing {
            id: "d1".into(),
            kind,
            points: (0..count as u8).map(point).collect(),
            label: None,
            selected: false,
            options: DrawingOptions::default(),
        }
    }

    /// A drawing of `kind` with `n` points, whatever the kind asked for.
    fn with_points(kind: DrawingKind, n: usize) -> Drawing {
        Drawing {
            points: (0..n as u8).map(point).collect(),
            ..drawing(kind)
        }
    }

    #[test]
    fn a_kind_takes_exactly_the_points_it_declares() {
        // The arity rule in both directions. The old model could only refuse *too
        // few* -- a third anchor simply did not exist -- so a tool that wants a
        // point list has to refuse too many as well, or a `trendline` sent with
        // four points draws a segment through the first two and throws the rest
        // away, which is a drawing that quietly is not what the user placed.
        let trendline = DrawingKind::Trendline;
        for n in [0, 1, 3, 4] {
            let error = with_points(trendline, n)
                .validate_anchors()
                .expect_err("must refuse");
            assert!(error.contains("trendline"), "{error}");
            assert!(error.contains("2 points"), "{error}");
            assert!(error.contains(&n.to_string()), "{error}");
        }
        assert_eq!(with_points(trendline, 2).validate_anchors(), Ok(()));
    }

    #[test]
    fn a_many_kind_accepts_two_five_and_forty() {
        // The open-ended half of `AnchorCount`, and the reason it is a variant
        // rather than a big number: a polyline stops when the user stops, so
        // every count from its minimum up is legitimate, and `Fixed { points: 40 }`
        // would refuse the 41st click.
        let many = AnchorCount::Many { min: 2 };
        assert!(many.accepts(2));
        assert!(many.accepts(5));
        assert!(many.accepts(40));
        assert!(!many.accepts(1));
        // And it has no exact count to hand a caller that wanted one.
        assert_eq!(many.points_for(), None);
        assert_eq!(AnchorCount::fixed(3).points_for(), Some(3));
    }

    #[test]
    fn the_arity_rule_is_the_only_one() {
        // Registry row and kind, one truth. A button that places one click for a
        // two-point tool ships a drawing the engine refuses -- and the user sees
        // the tool do nothing.
        for tool in REGISTRY {
            assert_eq!(
                tool.anchors,
                tool.kind.anchors(),
                "{} disagrees with its own kind",
                tool.kind.name()
            );
            assert!(
                tool.anchors.accepts(tool.kind.anchors().minimum()),
                "{} does not accept its own minimum",
                tool.kind.name()
            );
        }
        // Every kind's declared count is a drawing the validator accepts.
        for kind in DrawingKind::ALL {
            assert_eq!(drawing(kind).validate_anchors(), Ok(()), "{kind:?}");
        }
    }

    #[test]
    fn validate_names_the_problem() {
        // Two failures the point list makes *new*, and one the old model already
        // caught. A list is where a mixed-unit drawing can happen: the old struct
        // could not put a fraction and an absolute in the same drawing by
        // construction, because they were different fields; a list can, and the
        // numbers are the same shape, so nothing else would notice.
        let mixed = Drawing {
            points: vec![
                Anchor::Absolute {
                    time: 1_767_225_600_000.0,
                    price: 45_000.0,
                },
                Anchor::Fraction { x: 0.5, y: 0.5 },
            ],
            ..drawing(DrawingKind::Trendline)
        };
        let error = mixed
            .validate_anchors()
            .expect_err("a mixed drawing must be refused");
        assert!(error.contains("screen position"), "{error}");

        let nan = Drawing {
            points: vec![
                point(0),
                Anchor::Absolute {
                    time: f64::NAN,
                    price: 45_000.0,
                },
            ],
            ..drawing(DrawingKind::Trendline)
        };
        let error = nan
            .validate_anchors()
            .expect_err("a NaN point must be refused");
        assert!(error.contains("point 2"), "{error}");
    }

    #[test]
    fn every_kind_accepts_the_points_it_needs() {
        for kind in DrawingKind::ALL {
            assert_eq!(drawing(kind).validate_anchors(), Ok(()), "{kind:?}");
        }
    }

    #[test]
    fn a_one_point_kind_refuses_the_point_it_does_not_read() {
        // The rule the old model got wrong in the other direction: an `hline` that
        // carried a second anchor was *permitted*, and the drawing then offered
        // one handle for a value nothing reads. Under a point list the kind says
        // how many points it takes and takes exactly that, so a stored
        // two-point `hline` cannot exist and a spare handle cannot be offered.
        let error = with_points(DrawingKind::Hline, 2)
            .validate_anchors()
            .expect_err("must refuse");
        assert!(error.contains("1 point"), "{error}");
        assert_eq!(with_points(DrawingKind::Hline, 1).validate_anchors(), Ok(()));
    }

    #[test]
    fn no_hostile_number_produces_a_drawable_drawing() {
        // A `NaN` price produces a `NaN` y, and a canvas silently ignores a
        // `NaN` -- so the drawing would be absent with nothing saying why. The
        // sweep is over both variants and every position in the list, because a
        // point list has N of them and the old model could only check three.
        let hostile = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN,
        ];
        let mut refused = 0;
        let mut accepted = 0;
        for bad in hostile {
            for anchor in [
                Anchor::Absolute {
                    time: bad,
                    price: 1.0,
                },
                Anchor::Absolute {
                    time: 1.0,
                    price: bad,
                },
                Anchor::Fraction { x: bad, y: 0.5 },
                Anchor::Fraction { x: 0.5, y: bad },
            ] {
                for replace in [0, 1] {
                    let mut candidate = drawing(DrawingKind::Trendline);
                    if candidate.points.len() > replace {
                        candidate.points[replace] = anchor;
                    }
                    match candidate.validate_anchors() {
                        Ok(()) => accepted += 1,
                        Err(_) => refused += 1,
                    }
                }
            }
        }
        // `f64::MAX` and `f64::MIN` are *finite*, so they are legitimate
        // positions with an illegitimate magnitude -- they must be accepted
        // here and dealt with by the mapping, which is where a number too large
        // to draw is a drawing off the canvas rather than an invalid one.
        assert!(refused > 0, "nothing was refused, so this checks nothing");
        assert!(accepted > 0, "everything was refused, which is its own bug");
    }

    #[test]
    fn the_anchor_check_says_nothing_about_the_id() {
        // Deliberately not this function's business. The id is the shell's and
        // the scene's, and storage mints its own -- so a route validating a
        // request body has no id to give, and a check here would force it to
        // invent one. `scene::place` is where the id is required, and it is
        // asserted there.
        let mut candidate = drawing(DrawingKind::Hline);
        candidate.id = String::new();
        assert_eq!(candidate.validate_anchors(), Ok(()));
    }

    #[test]
    fn the_anchor_wire_is_pinned() {
        // Three consumers read this shape: the shell, the API that stores it,
        // and the database's millisecond boundary. A rename here is not a
        // compile error in the shell -- it is a drawing that lands at 1970.
        let json = serde_json::to_value(Anchor::Absolute {
            time: 1_767_225_600_000.0,
            price: 45_000.0,
        })
        .expect("serializes");
        assert_eq!(json["unit"], "absolute");
        assert_eq!(json["time"], 1_767_225_600_000.0);
        assert_eq!(json["price"], 45_000.0);

        let fraction =
            serde_json::to_value(Anchor::Fraction { x: 0.25, y: 0.75 }).expect("serializes");
        assert_eq!(fraction["unit"], "fraction");
        assert_eq!(fraction["x"], 0.25);
        assert_eq!(fraction["y"], 0.75);

        // And a fraction does not deserialize as an absolute one, which is the
        // property the two field-name sets exist for.
        assert!(serde_json::from_value::<Anchor>(serde_json::json!({
            "unit": "fraction", "time": 0.5, "price": 0.5
        }))
        .is_err());
    }

    #[test]
    fn a_fraction_is_not_a_time() {
        assert_eq!(
            Anchor::Fraction { x: 0.5, y: 0.5 }.absolute(),
            None,
            "a fraction's x is not a timestamp, and returning one would be the \
             bug the two variants exist to prevent"
        );
        assert_eq!(
            Anchor::Absolute {
                time: 12.0,
                price: 34.0
            }
            .absolute(),
            Some((12.0, 34.0))
        );
    }

    #[test]
    fn every_kind_name_is_a_snake_case_wire_value() {
        for kind in DrawingKind::ALL {
            let json = serde_json::to_value(kind).expect("serializes");
            assert_eq!(json, kind.name(), "{kind:?} renamed on the wire");
        }
    }

    #[test]
    fn every_level_profile_is_a_snake_case_wire_value_and_a_sorted_ladder() {
        for profile in LevelProfile::ALL {
            let json = serde_json::to_value(profile).expect("serializes");
            assert_eq!(json, profile.name(), "{profile:?} renamed on the wire");
            let ratios: Vec<f64> = profile.levels().iter().map(|(ratio, _)| *ratio).collect();
            assert!(
                ratios.windows(2).all(|pair| pair[0] < pair[1]),
                "{:?} is not ascending, so its labels would read scrambled",
                ratios
            );
            // Every level carries its own name: pass 5's finding was a grid of
            // anonymous dashes, and a ratio the trader cannot read is not a level.
            for (ratio, label) in profile.levels() {
                assert!(!label.is_empty(), "{ratio} has no label");
            }
        }
        // The four ladders are four different claims. Two profiles that resolve to
        // the same set would be a settings panel offering a choice that is none.
        for (index, profile) in LevelProfile::ALL.iter().enumerate() {
            for other in &LevelProfile::ALL[index + 1..] {
                assert_ne!(
                    profile.levels(),
                    other.levels(),
                    "{} and {} draw the same ladder",
                    profile.name(),
                    other.name()
                );
            }
        }
    }

    /// The registry is the one list of tools, and these pin its invariants.
    mod registry {
        use super::*;

        #[test]
        fn every_kind_has_exactly_one_tool() {
            // A kind without a row is a drawing nothing can place; a kind with
            // two rows is a button that lies about which one it is.
            assert_eq!(REGISTRY.len(), DrawingKind::ALL.len());
            for kind in DrawingKind::ALL {
                let rows = REGISTRY.iter().filter(|t| t.kind == kind).count();
                assert_eq!(rows, 1, "{kind:?} appears {rows} times");
            }
        }

        #[test]
        fn every_anchor_count_agrees_with_the_kind_rule() {
            // The registry is the toolbar's vocabulary and `DrawingKind::anchors`
            // is the engine's rule; this is the pin between them. A row that says
            // one and a kind that says another is a button that stops placing
            // before the drawing has the points it needs.
            for tool in REGISTRY {
                assert_eq!(
                    tool.anchors,
                    tool.kind.anchors(),
                    "{} disagrees with its own kind",
                    tool.kind.name()
                );
            }
        }

        #[test]
        fn every_kind_appears_in_exactly_one_group() {
            for kind in DrawingKind::ALL {
                let groups = REGISTRY.iter().filter(|t| t.kind == kind).count();
                assert_eq!(groups, 1, "{kind:?} sits in {groups} groups");
            }
            for group in ToolGroup::ALL {
                assert!(
                    REGISTRY.iter().any(|t| t.group == group),
                    "{group:?} has no tools"
                );
            }
        }

        #[test]
        fn the_registry_serializes_what_the_shell_reads() {
            // The shell builds its flyouts from this JSON over the ABI, so the
            // field names are wire, not internal. The point count is an object
            // rather than a number because `Many` has no number to send: the
            // shell's click counter reads `form` to know whether the third click
            // is the last one or just the latest.
            let json = serde_json::to_value(&REGISTRY as &[ToolSpec]).expect("serializes");
            assert_eq!(json[0]["kind"], "trendline");
            assert_eq!(json[0]["label"], "Trend");
            assert_eq!(json[0]["group"], "lines");
            assert_eq!(json[0]["anchors"]["form"], "fixed");
            assert_eq!(json[0]["anchors"]["points"], 2);
            assert_eq!(json[3]["kind"], "hline");
            assert_eq!(json[3]["anchors"]["points"], 1);
        }
    }

    #[test]
    fn the_drawing_wire_is_a_point_list() {
        // The shape the shell sends and the API stores. `points` is an array
        // because a drawing's arity is the kind's business, not the transport's:
        // a list needs no second and third field added every time a tool grows.
        let json = serde_json::to_value(drawing(DrawingKind::Channel)).expect("serializes");
        assert!(json["points"].is_array(), "{json}");
        assert_eq!(json["points"].as_array().expect("array").len(), 3);
        assert_eq!(json["points"][0]["unit"], "absolute");
        assert_eq!(json["points"][1]["price"], 45_500.0);
        assert!(json["a1"].is_null() && json["a2"].is_null() && json["a3"].is_null());
        // Options are absent from a request that says nothing, and the struct's
        // `#[serde(default)]` means it still parses.
        let back: Drawing =
            serde_json::from_value(serde_json::json!({"id":"d","kind":"hline","points":[
                {"unit":"absolute","time":1.0,"price":2.0}
            ]}))
            .expect("parses without options");
        assert_eq!(back.options, DrawingOptions::default());
    }

    #[test]
    fn a_scene_point_carries_both_units_in_one_object() {
        // The shell reads `absolute` to save and `fraction` to drag, and a point
        // that reported only one of them is the bug `docs/21` names: the shell
        // inverting the price mapping itself.
        let json = serde_json::to_value(ScenePoint {
            absolute: PlacedTimePrice {
                time: 1.0,
                price: 2.0,
            },
            fraction: Fraction { x: 0.25, y: 0.75 },
        })
        .expect("serializes");
        assert_eq!(json["absolute"]["time"], 1.0);
        assert_eq!(json["absolute"]["price"], 2.0);
        assert_eq!(json["fraction"]["x"], 0.25);
        assert_eq!(json["fraction"]["y"], 0.75);
    }

    #[test]
    fn the_fib_levels_are_the_retracement_set() {
        // Pinned because a level set is a claim about what the drawing means.
        // The extensions are deliberately absent: they are a different drawing,
        // and shipping them under this name would say something the user did
        // not ask for.
        let ratios: Vec<f64> = FIB_LEVELS.iter().map(|(ratio, _)| *ratio).collect();
        assert_eq!(ratios, vec![0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0]);
        // Ascending, so the labels read top to bottom in one direction and the
        // drawing cannot appear scrambled.
        assert!(ratios.windows(2).all(|pair| pair[0] < pair[1]));
        // The labels are the percentages, already formatted.
        assert_eq!(FIB_LEVELS[4].1, "61.8");
    }

    #[test]
    fn a_part_serializes_with_its_shape_tag() {
        // The shell switches on `shape`, so it has to be on the wire and it has
        // to be snake_case.
        let json = serde_json::to_value(DrawingPart::Handle {
            x: 10.0,
            y: 20.0,
            anchor: 1,
        })
        .expect("serializes");
        assert_eq!(json["shape"], "handle");
        assert_eq!(json["anchor"], 1);

        let segment = serde_json::to_value(DrawingPart::Segment {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            dashed: false,
        })
        .expect("serializes");
        assert_eq!(segment["shape"], "segment");
        assert_eq!(segment["dashed"], false);
    }

    #[test]
    fn a_fraction_is_a_plain_pair_and_carries_no_unit() {
        // `Anchor::Fraction` is tagged, because an anchor may also be absolute
        // and the tag is how the two are told apart. This is not: it is only
        // ever a fraction, so it is only ever two numbers. A `unit` here would
        // be a field the shell reads and can never find to be anything else.
        let json = serde_json::to_value(Fraction { x: 0.25, y: 0.75 }).expect("serializes");
        assert_eq!(json["x"], 0.25);
        assert_eq!(json["y"], 0.75);
        assert!(json["unit"].is_null(), "{json}");

        let back: Fraction = serde_json::from_value(json).expect("round trips");
        assert_eq!(back, Fraction { x: 0.25, y: 0.75 });
    }

    #[test]
    fn a_fraction_is_not_an_anchor() {
        // The two are the same two numbers and deliberately not one type. This
        // asserts the distinction is real on the wire as well as in the type
        // system: the tagged form and the plain one are different JSON, so a
        // fraction cannot be dropped where an anchor is expected and parse.
        let anchor = serde_json::to_value(Anchor::Fraction { x: 0.25, y: 0.75 }).expect("ok");
        let fraction = serde_json::to_value(Fraction { x: 0.25, y: 0.75 }).expect("ok");
        assert_eq!(anchor["unit"], "fraction");
        assert_ne!(anchor, fraction);
        assert!(
            serde_json::from_value::<Anchor>(fraction.clone()).is_err(),
            "a bare fraction must not parse as an anchor: {fraction}"
        );
    }

    /// Whether a `NaN` price can reach the engine at all.
    ///
    /// `Overlay::validate` refuses a non-finite price, and that check is worth
    /// having *only if something can produce one*. JSON has no NaN literal, so
    /// the only route is a `null` -- and serde rejects that before `validate`
    /// ever runs, which would make the guard unreachable from the wire.
    ///
    /// This asserts which it is, so the guard's own tests are not claiming to
    /// protect a path that does not exist.
    #[test]
    fn a_null_price_never_reaches_the_validator() {
        let from_json: Result<Overlay, _> = serde_json::from_value(serde_json::json!({
            "price": null, "label": "stop", "role": "stop"
        }));
        assert!(
            from_json.is_err(),
            "if this ever parses, the guard below is the only thing standing \
             between a null and a level drawn at the origin"
        );
    }

    /// And the direct-construction route, which is the one `validate` is for.
    #[test]
    fn a_price_that_is_a_nan_is_still_refused_at_the_type() {
        // Rust code -- a test, a future non-JSON transport, an arithmetic slip --
        // can build a `NaN` without touching serde. That is the caller this
        // guard exists for.
        let direct = Overlay {
            price: f64::NAN,
            label: "stop".into(),
            role: OverlayRole::Stop,
            band_to: None,
            filled: false,
        };
        assert!(direct.validate().is_err());
    }
}
