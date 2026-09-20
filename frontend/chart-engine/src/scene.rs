//! Candles, volume profile and footprint to **positioned rectangles**
//! (`docs/14-FRONTEND-CHART-ENGINE.md`).
//!
//! ## Why a scene and not drawing instructions
//!
//! `docs/14`: "Do not render each footprint cell or volume-profile bar as an
//! individual DOM/UI component. Build a compact in-memory scene representation
//! (arrays of positioned rectangles/bars with color) and draw it directly via
//! Canvas 2D."
//!
//! So this module's output is *geometry*: every rectangle already has its pixel
//! coordinates, every colour is already chosen. The shell's job is to iterate
//! and fill -- it makes no decisions about what a candle looks like, which is
//! what keeps all of this testable without a browser.
//!
//! ## The math is not here either
//!
//! Volume profile, VWAP and the POC/VAH/VAL levels come from `analytics-core`.
//! This module only decides *where* they go. That is `docs/14`'s hard rule: the
//! chart engine calls the same Rust analytics code the backend does, and a
//! second implementation of the trading math in JavaScript must never exist.
//! There is no JavaScript implementation, because there is no JavaScript
//! arithmetic over market data at all -- and that includes the Heikin-Ashi
//! transform below, which is the kind of thing a chart library normally does in
//! the client.
//!
//! ## What "footprint" means here, honestly
//!
//! A true footprint is built from **trades** -- buy-aggressed and sell-aggressed
//! volume at each price. This deployment has no tick data (`trades` is empty),
//! so the footprint mode renders the **candle-derived** volume-by-price
//! histogram instead: the same `analytics-core` profile, with its buy/sell
//! split. That is a real chart of a real quantity, and the scene says which one
//! it is in [`Scene::note`] rather than letting the label imply tick data that
//! does not exist.

use analytics_core::concepts::{detect as detect_concept, validate as validate_concept, Concept};
use analytics_core::regions::{detect_zones, Region, RegionOrigin, ZoneConfig};
use analytics_core::types::{Candle, Timeframe};
use analytics_core::volume_profile::{calculate_volume_profile_from_candles, VolumeProfile};
use analytics_core::vwap::calculate_vwap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::drawing::{
    Anchor, Drawing, DrawingKind, DrawingPart, Fraction, Overlay, SceneDrawing, SceneOverlay,
    FIB_LEVELS,
};
use crate::indicator::{IndicatorOutput, MarkerKind, ZoneState};

/// Layout constants, in CSS pixels of the scene's own coordinate space.
///
/// The shell scales the canvas for device pixel ratio; the scene is always in
/// logical pixels, so a test's expectations do not depend on the display.
const PAD_LEFT: f64 = 10.0;
const PAD_TOP: f64 = 10.0;
const PAD_RIGHT: f64 = 96.0;
const PAD_BOTTOM: f64 = 26.0;
/// How much of a slot a candle body occupies.
const BODY_FRACTION: f64 = 0.7;
/// Widest a volume-profile bar may be.
const PROFILE_WIDTH: f64 = 74.0;
/// Rows a volume profile aims for over the visible range.
const PROFILE_ROWS: f64 = 40.0;
/// Height of the per-candle summary strip below the footprint.
const SUMMARY_HEIGHT: f64 = 22.0;

/// What the chart is showing.
///
/// Deliberately the shapes a trader actually switches between, not every chart
/// that exists. Each is a *rendering* of the same candles -- the numbers
/// underneath do not change -- which is why the choice lives here rather than in
/// the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Candlesticks.
    #[default]
    Candles,
    /// Heikin-Ashi: averaged candles that smooth a trend.
    HeikinAshi,
    /// OHLC bars: a vertical range with an open tick and a close tick.
    Bars,
    /// A line through the closes.
    Line,
    /// A line through the closes, filled to the bottom of the plot.
    Area,
    /// Volume by price, with the buy/sell split.
    Footprint,
}

impl Mode {
    /// Every mode, for a client that wants to build a selector.
    pub const ALL: [Self; 6] = [
        Self::Candles,
        Self::HeikinAshi,
        Self::Bars,
        Self::Line,
        Self::Area,
        Self::Footprint,
    ];

    /// Whether this mode draws a shape per candle.
    ///
    /// Line and area are one path for the whole series; footprint is a grid of
    /// price levels. The shell must not iterate those as bars, and this is how
    /// it knows.
    #[must_use]
    pub const fn draws_bars(self) -> bool {
        matches!(self, Self::Candles | Self::HeikinAshi | Self::Bars)
    }

    /// Whether the volume profile is overlaid.
    ///
    /// Not on line or area, where it fights the fill, and not on footprint,
    /// which *is* a volume-by-price view.
    #[must_use]
    pub const fn shows_profile(self) -> bool {
        matches!(self, Self::Candles | Self::HeikinAshi | Self::Bars)
    }
}

/// What the shell asks for.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// Candles, ascending by open time.
    pub candles: Vec<Candle>,
    /// Canvas width in logical pixels.
    pub width: f64,
    /// Canvas height in logical pixels.
    pub height: f64,
    /// Which view.
    #[serde(default)]
    pub mode: Mode,
    /// Price bucket for the profile. Derived from the range when absent.
    #[serde(default)]
    pub bucket_size: Option<f64>,
    /// Per-candle trade-level ladders, when the caller has them.
    ///
    /// Empty when the window has no trades -- and then [`Mode::Footprint`] falls
    /// back to the candle-derived histogram with a note saying so, because a
    /// fabricated ladder is worse than an honest profile.
    #[serde(default)]
    pub footprint: Vec<crate::footprint::Column>,
    /// Trades behind `footprint`, for the footer.
    #[serde(default)]
    pub footprint_trades: usize,
    /// Overlay levels to draw.
    ///
    /// `serde(default = "default_lines")` rather than a bare `#[serde(default)]`:
    /// the latter fills in `Vec::default()`, which is *empty*, so a request that
    /// omits the field would draw no levels at all -- while [`Request::default`]
    /// promises the standard four. Two defaults disagreeing is a trap, and the
    /// ABI check found it.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
    /// Whether to detect and draw supply/demand zones.
    ///
    /// Detection runs **here**, on the candles the request already carries,
    /// rather than in the shell and rather than behind a route of its own. That
    /// is the same arrangement the volume profile and VWAP already use: this
    /// crate calls `analytics-core` and reimplements none of it, so the browser
    /// never runs a detector and `docs/14`'s no-arithmetic-in-the-shell rule
    /// holds by construction.
    ///
    /// A flag rather than a list of concepts because this is the built-in
    /// detector. Geometry defined by a *strategy document* arrives through
    /// [`Request::concepts`].
    #[serde(default)]
    pub zones: bool,
    /// Concept documents to compute and draw.
    ///
    /// This is where the chart learns about a measurement it was never taught. A
    /// concept is a pattern -- a window of candles and a band, defined by
    /// whoever wanted it -- and the built-in [`Request::zones`] detector is the
    /// same idea without the document: it is not privileged, it is simply the one
    /// that ships.
    ///
    /// `#[serde(default)]` filling in an empty `Vec` is *correct* here, unlike
    /// `lines` above: there is no standard set of concepts, so a request that
    /// does not name one gets none. The two fields look alike and mean opposite
    /// things, which is why both say so.
    ///
    /// A concept that fails validation is **not drawn** and its message is
    /// reported in [`Scene::note`], because half a pattern is worse than none and
    /// a silent refusal is worse than both.
    #[serde(default)]
    pub concepts: Vec<Concept>,
    /// Which slice of the series is visible, and at what price range.
    ///
    /// Absent means "everything, fitted", which is where a chart starts and what
    /// [`crate::viewport::Gesture::Fit`] returns it to. The shell holds the
    /// *resolved* form of this — [`Scene::viewport`] — and sends it back
    /// unchanged, so a reload cannot lose the view.
    #[serde(default)]
    pub viewport: crate::viewport::Viewport,
    /// What the user just did, if anything.
    ///
    /// A gesture rather than a viewport so that the clamping, the minimum bar
    /// count and the anchor arithmetic have one implementation. A wheel that
    /// arrives a hundred times a second cannot accumulate a drift the engine
    /// would have prevented, and a hostile factor cannot invert the axis.
    #[serde(default)]
    pub gesture: Option<crate::viewport::Gesture>,
    /// The shapes the user has drawn on this symbol.
    ///
    /// Sent every frame rather than fetched by the engine, because the engine is
    /// wasm with no I/O and the shell already holds them -- it read them from
    /// `/drawings` on the symbol change.
    ///
    /// An anchor may be a [`crate::drawing::Anchor::Fraction`] here, which is how
    /// placing and dragging work: the shell sends a pointer position and reads
    /// back a time and a price. The scene always reports
    /// [`crate::drawing::Anchor::Absolute`].
    ///
    /// A drawing that cannot be resolved is **not drawn** and the reason goes
    /// into [`Scene::note`] with its id, the same arrangement a refused concept
    /// document gets -- and for the same reason: half a shape is worse than
    /// none, and a silent refusal teaches whoever drew it nothing.
    #[serde(default)]
    pub drawings: Vec<Drawing>,
    /// Levels an *answer* put on the chart.
    ///
    /// The thesis's entry, stop and target, plus any level the answer cited. The
    /// shell already holds these -- it read them off the frame the agent sent --
    /// so they arrive with every rebuild the same way the user's drawings do.
    ///
    /// They are sent as **prices**, never as pixels, and this is the whole point
    /// of the field. The shell used to map them itself with its own copy of the
    /// price scale, which is a second implementation of the one thing `docs/14`
    /// keeps in Rust -- and the copy drifts from the real one on every resize,
    /// zoom and mode change. A band a few pixels off the candles it describes
    /// reads as "the AI's level moved", which is a lie about the answer.
    ///
    /// An overlay whose price is not a number is **not drawn** and the reason
    /// goes into [`Scene::note`], the same arrangement a refused drawing gets.
    #[serde(default)]
    pub overlays: Vec<crate::drawing::Overlay>,
    /// The output from one validated generated indicator revision.
    ///
    /// This remains optional while the workspace has no indicator attached. A
    /// module output is positioned here, not in JavaScript, so source revisions
    /// cannot drift from the chart's own time and price transforms.
    #[serde(default)]
    pub indicator: Option<IndicatorOutput>,
}

/// The levels drawn when a request does not say.
fn default_lines() -> Vec<String> {
    vec!["vwap".into(), "poc".into(), "vah".into(), "val".into()]
}

impl Default for Request {
    fn default() -> Self {
        Self {
            candles: Vec::new(),
            width: 800.0,
            height: 400.0,
            mode: Mode::Candles,
            bucket_size: None,
            footprint: Vec::new(),
            footprint_trades: 0,
            lines: default_lines(),
            zones: false,
            concepts: Vec::new(),
            viewport: crate::viewport::Viewport::default(),
            gesture: None,
            drawings: Vec::new(),
            overlays: Vec::new(),
            indicator: None,
        }
    }
}

/// One candle, already positioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Left edge of the body.
    pub x: f64,
    /// Body width.
    pub w: f64,
    /// Top of the body, in canvas coordinates (y grows downward).
    pub body_top: f64,
    /// Bottom of the body.
    pub body_bottom: f64,
    /// Top of the range.
    pub wick_top: f64,
    /// Bottom of the range.
    pub wick_bottom: f64,
    /// Where it opened, for a bar chart's left tick.
    pub open_y: f64,
    /// Where it closed, for a bar chart's right tick.
    pub close_y: f64,
    /// Whether it closed up.
    pub up: bool,
}

/// A point on a line or area chart.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Canvas x.
    pub x: f64,
    /// Canvas y.
    pub y: f64,
}

/// One volume-profile bucket, positioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileBar {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width, proportional to volume.
    pub w: f64,
    /// Height, the bucket's price thickness.
    pub h: f64,
    /// Volume traded in this bucket.
    pub volume: f64,
    /// Buy-aggressed share, 0..1.
    pub buy_ratio: f64,
    /// Whether this bucket is inside the value area.
    pub in_value_area: bool,
}

/// A horizontal level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Canvas y.
    pub y: f64,
    /// The price it marks.
    pub price: f64,
    /// What to call it: `vwap`, `poc`, `vah`, `val`.
    pub kind: String,
}

/// What put a region on the chart, in the scene's own vocabulary.
///
/// A mirror of `analytics_core::regions::RegionOrigin` rather than that type
/// itself, for the same reason [`Side::name`] exists: `Region`'s wire is a typed
/// analytics message, where a `BreakKind` travels as `"Bos"`, and the scene's
/// wire is `snake_case` throughout because the shell switches on the strings.
/// Carrying the analytics type here would put a `"Bos"` beside a `"buy"` in the
/// same object.
///
/// [`Side::name`]: analytics_core::types::Side::name
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum SceneOrigin {
    /// The impulse that broke structure, so this band is the move's origin.
    StructureBreak {
        /// `bos` (continuation) or `choch` (reversal).
        kind: String,
        /// The swing level the impulse closed through.
        level: f64,
    },
    /// A candle pattern a concept document defined.
    ///
    /// Carries nothing else, exactly as the analytics type does: the pattern is
    /// in the document, and a second copy here would be a second thing to keep
    /// in step.
    Pattern,
}

impl From<RegionOrigin> for SceneOrigin {
    fn from(origin: RegionOrigin) -> Self {
        match origin {
            RegionOrigin::StructureBreak { kind, level } => Self::StructureBreak {
                kind: kind.name().to_owned(),
                level,
            },
            RegionOrigin::Pattern => Self::Pattern,
        }
    }
}

impl SceneOrigin {
    /// The broken swing level, when there was one.
    ///
    /// A pattern has none, and says so by returning `None` rather than by
    /// carrying a zero -- a tooltip that reads "level 0.00" off a band that
    /// never had one is worse than one that reads nothing.
    #[must_use]
    pub fn broken_level(&self) -> Option<f64> {
        match self {
            Self::StructureBreak { level, .. } => Some(*level),
            Self::Pattern => None,
        }
    }
}

/// A supply/demand zone, a fair value gap, or any band a client defined,
/// positioned.
///
/// The only geometry in the scene that is an **area**. Everything else is a
/// point or a rectangle standing for one price at one time -- a candle, a
/// horizontal level, a profile bar, a footprint cell. This is a price band over
/// a span of time, which is the shape every one of those concepts shares, and
/// the reason a new concept needs no new drawing code.
///
/// It carries the prices as well as the pixels, for the same reason
/// [`ProfileBar`] carries `volume` and [`Level`] carries `price`: a tooltip has
/// to be able to say *which* band this is without the shell re-deriving it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneRegion {
    /// What to call it: `demand`, `supply`, or whatever a client named their
    /// concept. Also the shell's colour key.
    ///
    /// A `String` rather than the built-in `RegionKind`, because the whole point
    /// is that the vocabulary is not closed. A client's concept has no place in
    /// an enum we ship.
    pub name: String,
    /// Which side is expected to react from this band: `"buy"` or `"sell"`.
    ///
    /// The shell's fallback: a concept it has no colour for is still coloured by
    /// direction, so an unfamiliar band reads correctly instead of grey.
    pub side: String,
    /// Left edge, in canvas x.
    pub x: f64,
    /// Width in canvas pixels -- the band's time extent.
    pub w: f64,
    /// Top of the band, in canvas y: the dearer price.
    pub y_top: f64,
    /// Height of the band, in canvas pixels.
    ///
    /// Carried rather than left as `y_bottom - y_top` so the shell's fill is
    /// `fillRect(x, y_top, w, h)` with nothing to compute -- the same shape
    /// [`ProfileBar`] has, and the reason `docs/14` can say the shell performs no
    /// arithmetic over market data.
    pub h: f64,
    /// Cheapest price in the band.
    pub price_low: f64,
    /// Dearest price in the band.
    pub price_high: f64,
    /// How much of the band price has since traded back through, `0.0..=1.0`.
    pub mitigated: f64,
    /// Whether price has not touched it at all.
    ///
    /// The distinction the whole concept rests on: a mitigated band has already
    /// been consumed, and drawing it like a fresh one is how a chart teaches
    /// someone to buy a level that no longer exists.
    pub fresh: bool,
    /// What put it there -- a structure break, or a pattern someone defined.
    pub origin: SceneOrigin,
    /// Ready to draw: `"demand (fresh)"`, `"bullish gap (79% mitigated)"`.
    ///
    /// Formatted here rather than in the shell because turning `0.79` into
    /// `79%` is arithmetic, and `docs/14` keeps arithmetic out of JavaScript.
    pub label: String,
}

/// A generated indicator revision after its evidence graph is positioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneIndicator {
    /// Immutable source revision that produced this scene.
    pub revision_id: String,
    /// Stateful price bands.
    pub zones: Vec<SceneIndicatorZone>,
    /// Evidence nodes visible as chart markers.
    pub markers: Vec<SceneIndicatorMarker>,
    /// Causal connectors between evidence nodes.
    pub links: Vec<SceneEvidenceLink>,
}

/// A generated zone mapped into the chart's coordinate system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneIndicatorZone {
    /// Stable generated id.
    pub id: String,
    /// Left canvas coordinate.
    pub x: f64,
    /// Width in canvas pixels.
    pub w: f64,
    /// Upper canvas coordinate.
    pub y_top: f64,
    /// Height in canvas pixels.
    pub h: f64,
    /// Ready-to-display label.
    pub label: String,
    /// Lifecycle state controlling presentation.
    pub state: ZoneState,
}

/// A generated evidence marker mapped into the chart's coordinate system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneIndicatorMarker {
    /// Stable generated id.
    pub id: String,
    /// Evidence node represented by this marker.
    pub evidence_id: String,
    /// Canvas coordinate.
    pub x: f64,
    /// Canvas coordinate.
    pub y: f64,
    /// Ready-to-display label.
    pub label: String,
    /// Semantic style key.
    pub kind: MarkerKind,
    /// Client-readable reason the marker exists.
    pub explanation: String,
}

/// A causal edge in the evidence graph, positioned at both ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneEvidenceLink {
    /// Stable generated id.
    pub id: String,
    /// Start coordinate.
    pub from_x: f64,
    /// Start coordinate.
    pub from_y: f64,
    /// The curve's control coordinate, selected by the engine to keep the
    /// browser shell from deriving geometry from market-anchored points.
    pub control_x: f64,
    /// The curve's control coordinate.
    pub control_y: f64,
    /// End coordinate.
    pub to_x: f64,
    /// End coordinate.
    pub to_y: f64,
}

/// A price-axis tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    /// Canvas y.
    pub y: f64,
    /// The price.
    pub price: f64,
}

/// The plot rectangle the scene was laid out in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Plot {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// Everything the shell needs to draw one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Canvas width.
    pub width: f64,
    /// Canvas height.
    pub height: f64,
    /// Where the candles live.
    pub plot: Plot,
    /// Which shape to draw. The engine decides, so the shell does not have to.
    pub style: Mode,
    /// Lowest price shown.
    pub price_min: f64,
    /// Highest price shown.
    pub price_max: f64,
    /// First visible candle's open time.
    pub from: i64,
    /// One past the last visible candle's close time.
    pub to: i64,
    /// The view that was drawn, expressed as the request that would reproduce it.
    ///
    /// Reported because the shell's zoom state *is* this value: it holds it, sends
    /// it back with the next request, and applies no arithmetic to it.
    ///
    /// A [`Viewport`] rather than the resolved [`Window`] the engine sliced with,
    /// and the difference is the whole point. A resolved window has every field
    /// decided, so echoing one back would say "these five hundred bars" where the
    /// user had asked for "all of them" -- the same view on the frame it is sent,
    /// and then a chart that has quietly stopped following the market. See
    /// [`Window::as_viewport`].
    ///
    /// It is the *resolved* view and not the requested one, which is what makes it
    /// safe to echo: the engine may have clamped a pan at the oldest bar or a zoom
    /// at the minimum bar count, and a shell that assumed its own arithmetic had
    /// been honoured would drift out of step with what is on screen.
    ///
    /// [`Viewport`]: crate::viewport::Viewport
    /// [`Window`]: crate::viewport::Window
    /// [`Window::as_viewport`]: crate::viewport::Window::as_viewport
    pub viewport: crate::viewport::Viewport,
    /// Candles, in time order. Empty for line, area and footprint.
    pub candles: Vec<Bar>,
    /// The close path, for line and area.
    pub line: Vec<Point>,
    /// Volume-profile bars, cheapest first.
    pub profile: Vec<ProfileBar>,
    /// Overlay levels.
    pub levels: Vec<Level>,
    /// Supply/demand zones, when the request asked for them.
    ///
    /// Always present and usually empty: a client that did not ask gets `[]`
    /// rather than a missing key, so the shell's draw loop needs no null check
    /// and an older shell keeps working against a newer engine.
    pub regions: Vec<SceneRegion>,
    /// Price-axis ticks.
    pub ticks: Vec<Tick>,
    /// The trade-level footprint grid, when one could be built.
    ///
    /// `None` for every other mode, and for [`Mode::Footprint`] without trades
    /// -- in which case [`Scene::cells`] carries the candle-derived fallback.
    pub footprint: Option<crate::footprint::Grid>,
    /// The shapes the user drew, positioned.
    ///
    /// Last in the struct because it is drawn last: a drawing is the user's own
    /// annotation, and one that the volume profile or a zone could cover would
    /// be an annotation they cannot see. Always present and usually empty, like
    /// [`Scene::regions`], so the shell's draw loop needs no null check.
    pub drawings: Vec<SceneDrawing>,
    /// The levels an answer cited, positioned.
    ///
    /// Drawn *under* the user's own drawings and *over* everything the engine
    /// derived. The z-order is the claim being made: a volume profile is a
    /// measurement, an answer's entry is an opinion, and the user's own mark is
    /// the last word. An overlay that covered a drawing the user placed would
    /// hide their annotation behind somebody else's.
    ///
    /// Always present and usually empty, like [`Scene::regions`].
    pub overlays: Vec<SceneOverlay>,
    /// The attached generated indicator, when its output was valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicator: Option<SceneIndicator>,
    /// A caveat about what is being shown, when there is one.
    pub note: Option<String>,
}

/// Map a price into canvas y, given the range and the plot.
fn price_to_y(price: f64, price_min: f64, price_max: f64, plot: &Plot) -> f64 {
    let span = price_max - price_min;
    if span <= 0.0 {
        // A flat series: draw everything on the middle line rather than
        // dividing by zero and producing NaN, which a canvas silently ignores
        // -- the chart would just be blank.
        return plot.y + plot.h / 2.0;
    }
    // y grows downward, price grows upward.
    plot.y + plot.h - (price - price_min) / span * plot.h
}

/// Choose a bucket size that yields a readable number of rows.
///
/// The rule itself lives in `analytics-core`, because the footprint route needs
/// the same one and two copies of a rounding rule drift.
fn choose_bucket(price_min: f64, price_max: f64) -> f64 {
    analytics_core::volume_profile::round_bucket(price_max - price_min, PROFILE_ROWS as usize)
}

/// The price range a slice of candles would produce on its own.
///
/// This is the range the axis *fits* to, and the thing a price gesture scales
/// from. It is deliberately un-padded: the padding is a drawing decision made
/// once in [`build`], and baking it in here would make the padding compound every
/// time the user zoomed.
///
/// A slice that cannot produce a usable range -- empty, or every candle at one
/// price -- yields `min == max`, which [`PriceRange::is_usable`] rejects. Callers
/// must check rather than divide: a flat market is a real state, and inventing a
/// range for it would draw a chart of a move that did not happen.
fn fitted_range(candles: &[Candle]) -> crate::viewport::PriceRange {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for candle in candles {
        min = min.min(candle.low);
        max = max.max(candle.high);
    }
    if !min.is_finite() || !max.is_finite() {
        return crate::viewport::PriceRange { min: 0.0, max: 0.0 };
    }
    crate::viewport::PriceRange { min, max }
}

/// Build the scene.
///
/// Total: an empty request produces an empty scene with the plot already laid
/// out, so the shell never has to special-case "nothing to draw".
#[must_use]
pub fn build(request: &Request) -> Scene {
    let width = request.width.max(1.0);
    let height = request.height.max(1.0);
    let plot = Plot {
        x: PAD_LEFT,
        y: PAD_TOP,
        w: (width - PAD_LEFT - PAD_RIGHT).max(1.0),
        h: (height - PAD_TOP - PAD_BOTTOM).max(1.0),
    };

    let mut scene = Scene {
        width,
        height,
        plot,
        style: request.mode,
        price_min: 0.0,
        price_max: 0.0,
        from: 0,
        to: 0,
        viewport: crate::viewport::Viewport::default(),
        candles: Vec::new(),
        line: Vec::new(),
        profile: Vec::new(),
        levels: Vec::new(),
        regions: Vec::new(),
        ticks: Vec::new(),
        footprint: None,
        drawings: Vec::new(),
        overlays: Vec::new(),
        indicator: None,
        note: None,
    };

    if request.candles.is_empty() {
        // Resolved against zero bars rather than left at the literal above, so
        // the shell's price zoom survives a window that momentarily holds no
        // candles -- a live feed sitting between the backfill and its next tick.
        // Echoing the user's own range back is the entire reason the resolved
        // window carries one.
        scene.viewport = request.viewport.resolve(0).as_viewport();
        scene.note = Some("no candles in this window".into());
        return scene;
    }

    // Heikin-Ashi candles are derived from the series, so the range has to be
    // measured from the *drawn* values: an HA candle can sit outside every real
    // high or low in the window, and clamping to the raw range would push it off
    // the plot.
    //
    // It is also why the transform runs on the **whole** series and the visible
    // slice is taken afterwards. HA is recursive from the first candle, so
    // slicing first would restart the averaging at the left edge of the window --
    // and the same candle would then have a different open depending on how far
    // the user had scrolled. The window must not change the data.
    let plotted: Vec<Candle> = match request.mode {
        Mode::HeikinAshi => heikin_ashi(&request.candles),
        _ => request.candles.clone(),
    };

    // The range of what is visible *before* the gesture, because that is what a
    // price zoom starts from: it anchors on a price the user can actually see.
    let before = request.viewport.resolve(plotted.len());
    let fitted = fitted_range(&plotted[before.from..before.end()]);

    let viewport = match request.gesture {
        Some(gesture) => request.viewport.apply(gesture, plotted.len(), fitted),
        None => request.viewport,
    };
    let window = viewport.resolve(plotted.len());
    scene.viewport = window.as_viewport();

    let visible = &plotted[window.from..window.end()];
    if visible.is_empty() {
        scene.note = Some("no candles in this window".into());
        return scene;
    }

    // The drawn range is the visible candles' own range, padded a little so a
    // wick touching the edge is not clipped -- unless the user has moved the
    // price axis, in which case their range is used exactly and unpadded.
    let visible_fit = fitted_range(visible);
    if !visible_fit.is_usable() {
        scene.note = Some("candles have no usable prices".into());
        return scene;
    }
    let drawn = match viewport
        .price
        .filter(crate::viewport::PriceRange::is_usable)
    {
        Some(range) => range,
        None => {
            let pad = (visible_fit.span() * 0.04).max(f64::EPSILON);
            crate::viewport::PriceRange {
                min: visible_fit.min - pad,
                max: visible_fit.max + pad,
            }
        }
    };
    scene.price_min = drawn.min;
    scene.price_max = drawn.max;

    let first = &visible[0];
    let last = &visible[visible.len() - 1];
    let width_nanos = first.timeframe.nanos().max(1);
    scene.from = first.open_time;
    scene.to = last.open_time + width_nanos;

    // The slot is `plot.w / visible.len()`, so a candle keeps its width in *bars*
    // and grows in pixels as the user zooms in. That is what zooming in means,
    // and it is why the slot cannot be computed from the whole series.
    let slot = plot.w / visible.len() as f64;
    if request.mode.draws_bars() {
        scene.candles = candle_bars(visible, slot, &plot, scene.price_min, scene.price_max);
    }
    if matches!(request.mode, Mode::Line | Mode::Area) {
        scene.line = close_path(visible, slot, &plot, scene.price_min, scene.price_max);
    }

    // The profile and the levels describe **what is on screen**, so they follow
    // the visible slice too. A volume profile over the whole series beside
    // candles showing a tenth of it is a chart that disagrees with itself, and
    // VWAP is period-dependent: a VWAP line that ignored the window would mark a
    // price nobody in the window traded at.
    //
    // Indexing `request.candles` with the window is sound because `plotted` is
    // the same length -- the Heikin-Ashi transform maps one candle to one candle.
    let visible_real = &request.candles[window.from..window.end()];
    let bucket_size = request
        .bucket_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or_else(|| choose_bucket(visible_fit.min, visible_fit.max));
    // From the **real** series, not `plotted`. A volume profile assigns each
    // candle's volume to price levels, and a Heikin-Ashi candle's high and low
    // are averages -- prices nobody traded at. The profile would still look
    // plausible, which is exactly why it is worth being explicit: the note for
    // that mode promises the real series, and this is where that promise is
    // kept. `levels()` is handed the same slice for the same reason.
    let profile = calculate_volume_profile_from_candles(visible_real, bucket_size);

    if request.mode.shows_profile() {
        scene.profile = profile_bars(&profile, &plot, scene.price_min, scene.price_max);
    }

    match request.mode {
        Mode::HeikinAshi => {
            scene.note = Some(
                "Heikin-Ashi: each candle is averaged from its predecessor, so its open is not \
                 the real open and its close is not the real close. The levels and the profile \
                 are computed from the real series."
                    .into(),
            );
        }
        Mode::Footprint => {
            if request.footprint.is_empty() {
                // No trades for this window, so there is no ladder to draw. What
                // is drawn instead is the **volume profile** -- horizontal bars
                // sized by volume, anchored to the right -- which is a real
                // chart of a real quantity.
                //
                // What is *not* drawn is a fake ladder. A footprint built from
                // candles would reproduce each candle's aggregate ratio at every
                // price level, so a 3:1 candle would look like a stack of
                // imbalances it never had. That is why `analytics-core` refuses
                // to build one, and why this falls back to a different chart
                // rather than a worse version of the same one.
                scene.profile = profile_bars(&profile, &plot, scene.price_min, scene.price_max);
                scene.note = Some(
                    "no trades are stored for this window, so there is no ladder to draw. This is \
                     the volume profile instead -- a real chart, but not a footprint. Run \
                     `xtask backfill-trades` for this window to get the real ladder."
                        .into(),
                );
            } else {
                // The value area comes from the window profile, which is
                // `analytics-core`'s own calculation rather than a second one
                // invented here.
                let value_area = if profile.is_empty() {
                    None
                } else {
                    Some((profile.val, profile.vah))
                };
                scene.footprint = crate::footprint::layout(
                    &request.footprint,
                    plot,
                    value_area,
                    SUMMARY_HEIGHT,
                    request.footprint_trades,
                );
                // The caveat comes off the grid rather than being recomputed here:
                // the row cap depends on the plot height, and working it out a
                // second time is a second answer waiting to disagree.
                scene.note = scene
                    .footprint
                    .as_ref()
                    .and_then(|grid| grid.note.as_ref())
                    .map(|note| note.message.clone());
                if scene.footprint.is_none() {
                    scene.note = Some("the footprint for this window holds no price levels".into());
                }
            }
        }
        Mode::Candles | Mode::Bars | Mode::Line | Mode::Area => {}
    }

    // Concept documents are checked before anything is drawn. A document that
    // fails is **not drawn** -- half a pattern is worse than none -- and its
    // message goes into the note, because a silent refusal teaches the client
    // nothing about the document they wrote.
    let mut concepts = Vec::new();
    let mut refused = Vec::new();
    for concept in &request.concepts {
        match validate_concept(concept) {
            Ok(()) => concepts.push(concept.clone()),
            Err(error) => refused.push(format!("`{}`: {error}", concept.name)),
        }
    }

    scene.levels = levels(
        request,
        visible_real,
        &profile,
        &plot,
        scene.price_min,
        scene.price_max,
    );
    // The zones keep the **whole** series, unlike the profile and the levels: a
    // zone that formed before the window is still a zone, and clipping it to the
    // window would erase exactly the bands a trader scrolled back to look at.
    // The frame carries the window, so a zone outside it lands off-plot and the
    // canvas clips it, which is the correct rendering of "not in view".
    //
    // The frame is built once and shared with the drawings below, because they
    // need the same mapping and -- for a fraction anchor -- its inverse. Two
    // frames would be two chances for one of them to be built from the wrong
    // window, and the drawing would then sit a slot away from its own candle.
    let frame = Frame {
        plot,
        from: scene.from,
        to: scene.to,
        price_min: scene.price_min,
        price_max: scene.price_max,
    };
    scene.regions = region_rects(request.zones, &concepts, &request.candles, &frame);
    scene.ticks = ticks(&plot, scene.price_min, scene.price_max);

    // Drawings last, and positioned last, because they are the user's own
    // annotation: one that a zone or the profile could cover is one they cannot
    // see. They are also the only geometry here that keeps the **whole** series
    // and the whole price range -- a trendline drawn last week is still a
    // trendline when the window has moved past it, so the anchors are mapped
    // absolutely and the canvas clips, exactly as a zone's band is.
    let (drawings, unplaceable) = drawing_parts(&request.drawings, &frame);
    scene.drawings = drawings;

    // The answer's levels, between the derived geometry and the user's own
    // marks. Positioned here rather than in the shell because the shell has no
    // price scale -- see `Request::overlays` for why that matters.
    let (overlays, refused_overlays) = overlay_parts(&request.overlays, &frame);
    scene.overlays = overlays;

    match request.indicator.as_ref() {
        Some(output) => match indicator_parts(output, &frame) {
            Ok(indicator) => scene.indicator = Some(indicator),
            Err(reason) => add_note(
                &mut scene.note,
                format!("generated indicator is not drawn: {reason}"),
            ),
        },
        None => {}
    }

    if !refused.is_empty() {
        add_note(
            &mut scene.note,
            format!(
                "{} concept document(s) were refused and are not drawn: {}",
                refused.len(),
                refused.join("; ")
            ),
        );
    }
    if !unplaceable.is_empty() {
        add_note(
            &mut scene.note,
            format!(
                "{} drawing(s) could not be placed and are not drawn: {}",
                unplaceable.len(),
                unplaceable.join("; ")
            ),
        );
    }
    if !refused_overlays.is_empty() {
        add_note(
            &mut scene.note,
            format!(
                "{} answer level(s) could not be drawn: {}",
                refused_overlays.len(),
                refused_overlays.join("; ")
            ),
        );
    }

    scene
}

/// Resolve the answer's levels onto the frame.
///
/// Returns the positioned overlays and the reasons any were refused, so the
/// caller can put them in the scene's note. Refused rather than skipped: a stop
/// that silently does not appear reads as "the answer had no stop", which is a
/// different and much worse statement than "the stop it gave was not a number".
fn overlay_parts(overlays: &[Overlay], frame: &Frame) -> (Vec<SceneOverlay>, Vec<String>) {
    let mut placed = Vec::with_capacity(overlays.len());
    let mut refused = Vec::new();

    for overlay in overlays {
        if let Err(reason) = overlay.validate() {
            refused.push(reason);
            continue;
        }
        placed.push(position_overlay(overlay, frame));
    }

    (placed, refused)
}

/// Position a generated indicator's already-validated market coordinates.
///
/// Generated code never gets a canvas coordinate. Its contract is market time
/// and price; this is the one place those values become pixels, shared with
/// candles, regions, hand drawings, and answer overlays.
fn indicator_parts(output: &IndicatorOutput, frame: &Frame) -> Result<SceneIndicator, String> {
    output.validate()?;

    let evidence: BTreeMap<&str, (f64, f64, &str)> = output
        .evidence
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                (
                    frame.x_at_nanos(node.time),
                    frame.y_at(node.price),
                    node.explanation.as_str(),
                ),
            )
        })
        .collect();

    let zones = output
        .zones
        .iter()
        .map(|zone| {
            let left = frame.x_at_nanos(zone.start_time);
            let right = frame.x_at_nanos(zone.end_time);
            let top = frame.y_at(zone.price_high);
            let bottom = frame.y_at(zone.price_low);
            SceneIndicatorZone {
                id: zone.id.clone(),
                x: left,
                w: right - left,
                y_top: top,
                h: bottom - top,
                label: zone.label.clone(),
                state: zone.state,
            }
        })
        .collect();
    let markers = output
        .markers
        .iter()
        .map(|marker| {
            let explanation = evidence
                .get(marker.evidence_id.as_str())
                .map(|(_, _, explanation)| (*explanation).to_owned())
                // `validate` above guarantees this branch is unreachable. It
                // still names the failure rather than panicking if a future
                // refactor weakens validation.
                .ok_or_else(|| format!("marker `{}` has no evidence", marker.id))?;
            Ok(SceneIndicatorMarker {
                id: marker.id.clone(),
                evidence_id: marker.evidence_id.clone(),
                x: frame.x_at_nanos(marker.time),
                y: frame.y_at(marker.price),
                label: marker.label.clone(),
                kind: marker.kind,
                explanation,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let links = output
        .links
        .iter()
        .map(|link| {
            let (from_x, from_y, _) = evidence
                .get(link.from.as_str())
                .ok_or_else(|| format!("link `{}` has no `from` evidence", link.id))?;
            let (to_x, to_y, _) = evidence
                .get(link.to.as_str())
                .ok_or_else(|| format!("link `{}` has no `to` evidence", link.id))?;
            Ok(SceneEvidenceLink {
                id: link.id.clone(),
                from_x: *from_x,
                from_y: *from_y,
                control_x: (*from_x + *to_x) / 2.0,
                control_y: from_y.min(*to_y) - 18.0,
                to_x: *to_x,
                to_y: *to_y,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(SceneIndicator {
        revision_id: output.revision_id.clone(),
        zones,
        markers,
        links,
    })
}

/// One overlay, mapped onto canvas pixels.
fn position_overlay(overlay: &Overlay, frame: &Frame) -> SceneOverlay {
    SceneOverlay {
        y: frame.y_at(overlay.price),
        price: overlay.price,
        label: overlay.label.clone(),
        role: overlay.role,
        // `None` stays `None`: a level is a line, and turning it into a band
        // from its own price to its own price would make the shell stroke a
        // zero-height rectangle it then has to special-case away.
        band_y: overlay.band_to.map(|far| frame.y_at(far)),
        filled: overlay.filled,
    }
}

/// Append a sentence to the scene's note, keeping whatever was already there.
///
/// A note is the only channel a refusal has, and there are three producers now:
/// a mode's own caveat, a refused concept and an unplaceable drawing. Overwriting
/// would leave the loudest one and drop the rest, which is how a chart ends up
/// explaining one of its two problems.
fn add_note(note: &mut Option<String>, message: String) {
    *note = Some(match note.take() {
        Some(existing) => format!("{existing} {message}"),
        None => message,
    });
}

/// Heikin-Ashi, from the real candles.
///
/// `docs/14`'s rule: this is arithmetic over market data, so it lives in Rust and
/// never in the shell. The formulas are the standard ones --
///
/// ```text
/// haClose = (open + high + low + close) / 4
/// haOpen  = (previous haOpen + previous haClose) / 2      (first: (open + close) / 2)
/// haHigh  = max(high, haOpen, haClose)
/// haLow   = min(low,  haOpen, haClose)
/// ```
///
/// The averaged values are carried in the `Candle` fields, so everything
/// downstream treats them as the series. `symbol`, `timeframe`, `open_time` and
/// the volumes are untouched, because they describe the bucket rather than the
/// shape.
#[must_use]
pub fn heikin_ashi(candles: &[Candle]) -> Vec<Candle> {
    let mut out = Vec::with_capacity(candles.len());
    let mut previous: Option<(f64, f64)> = None;

    for candle in candles {
        let close = (candle.open + candle.high + candle.low + candle.close) / 4.0;
        let open = match previous {
            Some((open, close)) => (open + close) / 2.0,
            // The first candle has no predecessor, so it opens at its own
            // midpoint -- the convention every charting package uses.
            None => (candle.open + candle.close) / 2.0,
        };
        let high = candle.high.max(open).max(close);
        let low = candle.low.min(open).min(close);

        out.push(Candle {
            open,
            high,
            low,
            close,
            ..candle.clone()
        });
        previous = Some((open, close));
    }
    out
}

fn candle_bars(
    candles: &[Candle],
    slot: f64,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<Bar> {
    let body = (slot * BODY_FRACTION).max(1.0);

    candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            let centre = plot.x + slot * (index as f64 + 0.5);
            let open_y = price_to_y(candle.open, price_min, price_max, plot);
            let close_y = price_to_y(candle.close, price_min, price_max, plot);
            Bar {
                x: centre - body / 2.0,
                w: body,
                body_top: open_y.min(close_y),
                body_bottom: open_y.max(close_y),
                wick_top: price_to_y(candle.high, price_min, price_max, plot),
                wick_bottom: price_to_y(candle.low, price_min, price_max, plot),
                open_y,
                close_y,
                up: candle.close >= candle.open,
            }
        })
        .collect()
}

/// The close of every candle, as a path.
fn close_path(
    candles: &[Candle],
    slot: f64,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<Point> {
    candles
        .iter()
        .enumerate()
        .map(|(index, candle)| Point {
            x: plot.x + slot * (index as f64 + 0.5),
            y: price_to_y(candle.close, price_min, price_max, plot),
        })
        .collect()
}

fn profile_bars(
    profile: &VolumeProfile,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<ProfileBar> {
    let peak = profile
        .histogram
        .iter()
        .map(|node| node.volume)
        .fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return Vec::new();
    }
    let right = plot.x + plot.w;
    let row_height = (plot.h / PROFILE_ROWS).max(1.0);

    profile
        .histogram
        .iter()
        .filter(|node| node.price_level >= price_min && node.price_level <= price_max)
        .map(|node| {
            let y = price_to_y(node.price_level, price_min, price_max, plot);
            let w = node.volume / peak * PROFILE_WIDTH;
            let total = node.buy_volume + node.sell_volume;
            ProfileBar {
                x: right - w,
                y: y - row_height / 2.0,
                w,
                h: row_height,
                volume: node.volume,
                buy_ratio: if total > 0.0 {
                    node.buy_volume / total
                } else {
                    0.5
                },
                in_value_area: node.price_level >= profile.val && node.price_level <= profile.vah,
            }
        })
        .collect()
}

/// The overlay levels, measured over the **visible window**.
///
/// `candles` is the visible slice of the real series, so the VWAP is the window's
/// VWAP. That matters more than it looks: VWAP is period-dependent, so a line
/// computed over the whole series and drawn across a tenth of it marks a price
/// nobody in the window traded at. It still looks like a VWAP, which is why the
/// wrong version survives review -- the same trap the profile has.
fn levels(
    request: &Request,
    candles: &[Candle],
    profile: &VolumeProfile,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<Level> {
    let mut out = Vec::new();
    let wanted = |name: &str| request.lines.iter().any(|line| line == name);

    if wanted("vwap") {
        if let Some(vwap) = calculate_vwap(candles) {
            out.push(Level {
                y: price_to_y(vwap, price_min, price_max, plot),
                price: vwap,
                kind: "vwap".into(),
            });
        }
    }
    if !profile.is_empty() {
        for (name, price) in [
            ("poc", profile.poc),
            ("vah", profile.vah),
            ("val", profile.val),
        ] {
            if wanted(name) {
                out.push(Level {
                    y: price_to_y(price, price_min, price_max, plot),
                    price,
                    kind: name.into(),
                });
            }
        }
    }
    out
}

/// Where a price and a time land on the canvas, and the way back.
///
/// One struct rather than four loose numbers because they are one thing: the
/// mapping from `(timestamp, price)` to `(x, y)`. Handing them over whole is
/// also what keeps a caller from pairing the wrong `from` with the wrong `to` --
/// four same-shaped parameters in a row is a mistake the compiler cannot see,
/// and a band drawn against the wrong window is drawn silently wrong.
///
/// ## Why the inverse is here too
///
/// A drawing's anchors arrive as positions on the plot while they are being
/// placed, so something has to turn a position back into a time and a price.
/// That something is this struct, and it is the only place it happens -- which
/// is the point. The alternative is the shell inverting the mapping, and then
/// there are two implementations of "which price is at the top of the plot", and
/// the chart and its own axis disagree the moment either one is edited.
///
/// ## Why two x methods and not one
///
/// [`Frame::x_at_nanos`] and [`Frame::x_at_ms`] differ only in their unit, which
/// is exactly the shape of mistake this repository keeps finding: both compile,
/// both look right, and a nanosecond timestamp passed to the millisecond one
/// puts the drawing in 1970. The units are in the names because nothing else can
/// tell them apart.
///
/// ## Every method here assumes a usable frame, and nothing here checks
///
/// A `Frame` is only ever built in [`build`], immediately after the window has
/// produced candles and a usable price range -- both of which `build` has already
/// established and refuses to continue without. So there is no degenerate frame
/// to guard against, and a `span <= 0.0` branch here would be one nothing can
/// reach: the kind of guard this repository keeps deciding is worse than none,
/// because it reads as coverage. The precondition is written down instead of
/// defended.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// The plot rectangle.
    plot: Plot,
    /// Left edge of the visible window, in unix nanoseconds.
    from: i64,
    /// Right edge.
    to: i64,
    /// The bottom of the price range.
    price_min: f64,
    /// The top.
    price_max: f64,
}

impl Frame {
    /// A price's canvas y.
    fn y_at(&self, price: f64) -> f64 {
        price_to_y(price, self.price_min, self.price_max, &self.plot)
    }

    /// A timestamp's canvas x, in **nanoseconds** -- what a candle or a region
    /// carries.
    fn x_at_nanos(&self, time_ns: i64) -> f64 {
        let span = (self.to - self.from) as f64;
        self.plot.x + (time_ns - self.from) as f64 / span * self.plot.w
    }

    /// A timestamp's canvas x, in **milliseconds** -- what a drawing carries.
    fn x_at_ms(&self, time_ms: f64) -> f64 {
        let span = self.right_ms() - self.left_ms();
        self.plot.x + (time_ms - self.left_ms()) / span * self.plot.w
    }

    /// The millisecond timestamp at a fraction across the plot.
    fn ms_at(&self, fraction: f64) -> f64 {
        self.left_ms() + fraction * (self.right_ms() - self.left_ms())
    }

    /// The price at a fraction down the plot.
    fn price_at(&self, fraction: f64) -> f64 {
        // y grows downward, price grows upward.
        self.price_max - fraction * (self.price_max - self.price_min)
    }

    /// The fraction across the plot at a millisecond timestamp.
    ///
    /// The inverse of [`Frame::ms_at`], and here for the same reason that one
    /// is: a body drag is a screen-space delta, and the shell cannot add one to
    /// a timestamp without inverting this mapping. Reporting the fraction is
    /// what lets the shell stay in the unit it already works in -- the same
    /// division `pan` does -- while the price scale stays in Rust.
    fn fraction_at_ms(&self, time_ms: f64) -> f64 {
        (time_ms - self.left_ms()) / (self.right_ms() - self.left_ms())
    }

    /// The fraction down the plot at a price. The inverse of
    /// [`Frame::price_at`], with the same reversal of direction.
    fn fraction_at_price(&self, price: f64) -> f64 {
        (self.price_max - price) / (self.price_max - self.price_min)
    }

    /// The window's left edge in milliseconds.
    ///
    /// Integer division rather than `as f64 / 1e6`: the second one converts to a
    /// `f64` first, and a present-day nanosecond timestamp is past 2^53, so the
    /// division would be done on a number that had already lost its low bits.
    /// This is exact.
    ///
    /// Named for the edge rather than `from_ms`/`to_ms`, which is what it was
    /// first: clippy reads those as the `from_*` and `to_*` conversion
    /// conventions and objects, and the names it suggests are no clearer. The
    /// fields are `from` and `to`; these are the same two edges in another unit.
    fn left_ms(&self) -> f64 {
        (self.from / 1_000_000) as f64
    }

    /// The window's right edge in milliseconds.
    fn right_ms(&self) -> f64 {
        (self.to / 1_000_000) as f64
    }
}

/// Where each region's band lands on the canvas.
///
/// The time mapping is the same one the candles use: a region's edges are
/// timestamps, and the plot's width is divided by the **visible window's**
/// duration exactly as a candle's slot is. That is what makes a band line up with
/// the candles that formed it instead of drifting sideways -- a band drawn a slot
/// off reads as a different level entirely.
///
/// `candles` is the **real** series, not the drawn one. [`build`] passes
/// `request.candles` rather than its own `plotted`, deliberately: structure and
/// patterns are facts about prices that traded, so switching the chart to
/// Heikin-Ashi must not invent bands out of averaged candles. That holds for
/// both producers below, because they are handed the same series here.
///
/// Detection therefore runs over the **whole** series while the mapping covers
/// only the window, which is the arrangement that makes zooming honest: a band
/// that formed before the left edge is still a band, and clipping it away would
/// erase exactly the levels a trader scrolled back to look at. The clamp below
/// is what turns "outside the window" into an off-plot rectangle the canvas
/// discards, rather than into a band drawn at the wrong x.
fn region_rects(
    zones: bool,
    concepts: &[Concept],
    candles: &[Candle],
    map: &Frame,
) -> Vec<SceneRegion> {
    let span = (map.to - map.from) as f64;
    if span <= 0.0 {
        return Vec::new();
    }

    // Two producers, one geometry. The built-in detector is **not privileged**:
    // it is a producer of bands that happen to be called `demand` and `supply`,
    // and a document a client wrote is another. Neither knows this function
    // exists, which is why a concept needs no new drawing code.
    //
    // The order is the draw order: built-ins first, then client concepts, so a
    // band someone defined by hand is not hidden under the one that ships.
    let mut regions: Vec<Region> = Vec::new();
    if zones {
        regions.extend(detect_zones(candles, ZoneConfig::default()));
    }
    for concept in concepts {
        regions.extend(detect_concept(candles, concept));
    }

    regions
        .into_iter()
        // A band with no height is not a region. Drawing one produces an
        // invisible rectangle with a label floating on nothing, which reads as a
        // rendering bug rather than as a flat origin.
        .filter(|region| {
            let height = region.height();
            height.is_finite() && height > 0.0
        })
        .filter_map(|region| {
            // Clamp to the window. Detection runs on these very candles, so
            // today this never bites; it is here because the next caller -- a
            // strategy document naming its own concept -- may hand over a region
            // that starts before the visible window, and a band drawn off-canvas
            // to the left is worse than one drawn short.
            let start = region.from.max(map.from);
            let end = region.to.min(map.to);
            if end <= start {
                return None;
            }
            let mitigated = region.mitigated.clamp(0.0, 1.0);
            let y_top = map.y_at(region.price_high);
            let y_bottom = map.y_at(region.price_low);
            Some(SceneRegion {
                label: region_label(&region.name, mitigated),
                name: region.name,
                side: region.side.name().to_owned(),
                x: map.x_at_nanos(start),
                w: map.x_at_nanos(end) - map.x_at_nanos(start),
                y_top,
                h: y_bottom - y_top,
                price_low: region.price_low,
                price_high: region.price_high,
                mitigated,
                fresh: mitigated <= 0.0,
                origin: region.origin.into(),
            })
        })
        .collect()
}

/// How a region's label reads.
///
/// `name` is the region's own, already display-ready -- `demand`, `supply`, or
/// whatever a client called their concept. It is a `&str` and not a
/// [`RegionKind`] because there is no closed set of names to match on: a
/// concept document supplies one, and this function must not need to know it.
///
/// `mitigated` is taken already clamped, so the only branch here is the one the
/// concept needs: fresh or partly consumed.
///
/// [`RegionKind`]: analytics_core::regions::RegionKind
fn region_label(name: &str, mitigated: f64) -> String {
    if mitigated <= 0.0 {
        format!("{name} (fresh)")
    } else {
        format!("{name} ({:.0}% mitigated)", mitigated * 100.0)
    }
}

/// Where each drawing's shapes land on the canvas, and what could not be placed.
///
/// The anchors are resolved first and the shapes built from them, which is the
/// order the two units demand: a fraction anchor cannot be positioned until it
/// has become a time and a price, and every shape needs both.
///
/// A refusal is not an error. It is one drawing missing from a chart that still
/// has everything else, plus a sentence in the note saying which one and why --
/// the arrangement a refused concept document already gets, and for the same
/// reason: half a shape is worse than none, and a silent refusal teaches whoever
/// drew it nothing.
fn drawing_parts(drawings: &[Drawing], frame: &Frame) -> (Vec<SceneDrawing>, Vec<String>) {
    let mut placed = Vec::with_capacity(drawings.len());
    let mut refused = Vec::new();

    for drawing in drawings {
        match place(drawing, frame) {
            Ok(scene_drawing) => placed.push(scene_drawing),
            Err(reason) => refused.push(format!("`{}`: {reason}", drawing.id)),
        }
    }
    (placed, refused)
}

/// Resolve one drawing's anchors and build its shapes.
fn place(drawing: &Drawing, frame: &Frame) -> Result<SceneDrawing, String> {
    drawing.validate_anchors()?;
    // The id is required *here* rather than in `validate_anchors`, because this
    // is the only caller that needs one: it is how a refusal names the drawing
    // and how the shell finds the one it grabbed. Storage mints its own, so a
    // route validating a request body has none to give.
    if drawing.id.trim().is_empty() {
        return Err("it has no id, so nothing can refer to it".into());
    }

    let a1 = resolve(drawing.a1, frame);
    let a2 = drawing.a2.map(|anchor| resolve(anchor, frame));

    // A click with no drag, on a tool that needs two points. Both anchors are the
    // same point, so the shape has no extent: nothing visible is drawn, and yet
    // it is stored -- so the next reload draws the same nothing, and the user who
    // clicked is left believing the tool does not work.
    //
    // Exact equality is the right test rather than a tolerance. It is not a
    // *small* drawing being rejected: it is precisely "the pointer did not move
    // between down and up", because both anchors are the same fraction of the
    // same plot. A one-pixel drag is a different pair of numbers and is drawn.
    //
    // The whole anchor is compared, not one coordinate, because a vertical
    // trendline and a flat rectangle are both real things to draw.
    if drawing.kind.needs_second_anchor() && a2 == Some(a1) {
        return Err("it has two anchors at the same point, so it has no extent".into());
    }

    let parts = shapes(drawing.kind, drawing.selected, a1, a2, frame);
    Ok(SceneDrawing {
        id: drawing.id.clone(),
        kind: drawing.kind,
        label: drawing.label.clone(),
        selected: drawing.selected,
        a1: Anchor::Absolute {
            time: a1.0,
            price: a1.1,
        },
        a2: a2.map(|(time, price)| Anchor::Absolute { time, price }),
        // The fractions are derived from the resolved numbers rather than passed
        // down from the request, so an anchor that arrived absolute and one that
        // arrived as a fraction report the same thing. That is what makes the
        // round trip in
        // `placing_with_a_fraction_and_saving_the_answer_does_not_move_the_drawing`
        // hold for both, and it is why there is one `resolve` and not two.
        a1_fraction: Fraction {
            x: frame.fraction_at_ms(a1.0),
            y: frame.fraction_at_price(a1.1),
        },
        a2_fraction: a2.map(|(time, price)| Fraction {
            x: frame.fraction_at_ms(time),
            y: frame.fraction_at_price(price),
        }),
        parts,
    })
}

/// Turn one anchor into an absolute millisecond time and a price.
///
/// The inverse direction of the frame, and the only place it happens. Both a
/// placement and a drag come through here, which is what stops a dragged anchor
/// from landing somewhere a stored one could not -- the two go through one
/// function rather than two that agree today.
///
/// Infallible. The only thing that can be wrong with an anchor is its
/// *usability*, which [`Drawing::validate`] has already refused, and the frame is
/// usable by construction.
fn resolve(anchor: Anchor, frame: &Frame) -> (f64, f64) {
    match anchor {
        Anchor::Absolute { time, price } => (time, price),
        Anchor::Fraction { x, y } => (frame.ms_at(x), frame.price_at(y)),
    }
}

/// The shapes one drawing is made of.
///
/// `a1` and `a2` are already resolved to `(milliseconds, price)`, so there is no
/// unit left to get wrong here -- which is why this takes the numbers rather
/// than the [`Anchor`]s.
fn shapes(
    kind: DrawingKind,
    selected: bool,
    a1: (f64, f64),
    a2: Option<(f64, f64)>,
    frame: &Frame,
) -> Vec<DrawingPart> {
    let (t1, p1) = a1;
    let x1 = frame.x_at_ms(t1);
    let y1 = frame.y_at(p1);
    let mut parts = Vec::new();

    match kind {
        DrawingKind::Trendline => {
            if let Some((t2, p2)) = a2 {
                parts.push(DrawingPart::Segment {
                    x1,
                    y1,
                    x2: frame.x_at_ms(t2),
                    y2: frame.y_at(p2),
                    dashed: false,
                });
            }
        }
        DrawingKind::Hline => {
            // Across the whole plot, because a horizontal level is a price
            // rather than a segment, and one that stopped at the right edge of
            // wherever the user happened to click would be a line to nowhere.
            parts.push(DrawingPart::Segment {
                x1: frame.plot.x,
                y1,
                x2: frame.plot.x + frame.plot.w,
                y2: y1,
                dashed: false,
            });
            parts.push(DrawingPart::Text {
                x: frame.plot.x + 4.0,
                y: y1 - 4.0,
                text: format!("{p1:.2}"),
            });
        }
        DrawingKind::Rect => {
            if let Some((t2, p2)) = a2 {
                let x2 = frame.x_at_ms(t2);
                let y2 = frame.y_at(p2);
                parts.push(DrawingPart::Rect {
                    x: x1.min(x2),
                    y: y1.min(y2),
                    w: (x2 - x1).abs(),
                    h: (y2 - y1).abs(),
                    filled: true,
                });
            }
        }
        DrawingKind::Fib => {
            if let Some((t2, p2)) = a2 {
                // Between the two anchors, not across the plot: the levels are a
                // measurement of that range, and drawing them wider than the
                // range they measure implies a claim the user did not make.
                let x2 = frame.x_at_ms(t2);
                let left = x1.min(x2);
                let right = x1.max(x2);
                for (ratio, label) in FIB_LEVELS {
                    let price = p1 + (p2 - p1) * ratio;
                    let y = frame.y_at(price);
                    parts.push(DrawingPart::Segment {
                        x1: left,
                        y1: y,
                        x2: right,
                        y2: y,
                        dashed: false,
                    });
                    // The percentage *and* the price, formatted here: turning
                    // 0.618 into "61.8" is arithmetic, and `docs/14` keeps
                    // arithmetic out of JavaScript.
                    parts.push(DrawingPart::Text {
                        x: left + 4.0,
                        y: y - 3.0,
                        text: format!("{label}%  {price:.2}"),
                    });
                }
            }
        }
    }

    // Handles only when selected, and only at anchors this kind actually uses.
    // The second half matters: an `hline` that carried a second anchor -- which
    // is permitted and stored -- would otherwise offer a grab point for an
    // anchor the drawing does not read, and the shell cannot tell the
    // difference. This is what makes "the shell cannot offer a grab point the
    // engine would not honour" true rather than aspirational.
    if selected {
        parts.push(DrawingPart::Handle {
            x: x1,
            y: y1,
            anchor: 0,
        });
        if kind.needs_second_anchor() {
            if let Some((t2, p2)) = a2 {
                parts.push(DrawingPart::Handle {
                    x: frame.x_at_ms(t2),
                    y: frame.y_at(p2),
                    anchor: 1,
                });
            }
        }
    }

    parts
}

fn ticks(plot: &Plot, price_min: f64, price_max: f64) -> Vec<Tick> {
    const COUNT: usize = 6;
    let step = (price_max - price_min) / COUNT as f64;
    (0..=COUNT)
        .map(|i| {
            let price = price_min + step * i as f64;
            Tick {
                y: price_to_y(price, price_min, price_max, plot),
                price,
            }
        })
        .collect()
}

/// The timeframe of a series, for a caller that wants to label the axis.
#[must_use]
pub fn series_timeframe(candles: &[Candle]) -> Option<Timeframe> {
    candles.first().map(|candle| candle.timeframe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::OverlayRole;
    use crate::footprint;
    use crate::indicator::{Evidence, EvidenceLink, IndicatorMarker, IndicatorOutput, MarkerKind};
    use analytics_core::concepts::{Compare, Requirement, Selector};
    use analytics_core::types::Side;

    fn candle(index: i64, open: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: index * 300_000_000_000,
            open,
            high: open.max(close) + 1.0,
            low: open.min(close) - 1.0,
            close,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    fn series(count: i64) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let base = 100.0 + (i as f64) * 0.5;
                candle(i, base, base + 0.25)
            })
            .collect()
    }

    fn request(count: i64) -> Request {
        Request {
            candles: series(count),
            ..Request::default()
        }
    }

    fn mode_request(mode: Mode, count: i64) -> Request {
        Request {
            mode,
            ..request(count)
        }
    }

    /// A series with a demand zone in it.
    ///
    /// The same shape `analytics-core::regions`' own tests use: a decline that
    /// leaves a confirmed swing high, a three-candle pause, and an impulse that
    /// closes through that high. Written out by hand rather than fetched so the
    /// test states the rule it checks instead of inheriting whatever the market
    /// did -- and so it still means something when the database is empty.
    fn zoned_series() -> Vec<Candle> {
        #[rustfmt::skip]
        let rows: [(f64, f64, f64, f64); 20] = [
            (100.0, 101.0,  99.0,  99.5),  // a decline, so structure has lows to confirm
            ( 99.5, 100.0,  96.0,  96.5),
            ( 96.5,  98.0,  95.0,  97.5),
            ( 97.5,  98.5,  94.0,  94.5),
            ( 94.5,  95.5,  92.0,  92.5),
            ( 92.5,  93.5,  90.0,  93.0),
            ( 93.0,  96.0,  92.5,  95.5),
            ( 96.5,  97.0,  95.0,  95.5),  // the swing high the impulse will break
            ( 96.0,  96.2,  94.6,  94.8),  // the origin: three down-close candles
            ( 94.8,  95.0,  93.4,  93.6),
            ( 93.6,  93.9,  92.8,  93.0),  // ... spanning 92.8 up to 96.2
            ( 93.0,  95.5,  92.9,  95.0),  // the impulse, five up candles
            ( 95.0,  97.5,  94.8,  97.0),
            ( 97.0, 100.0,  96.8,  99.5),
            ( 99.5, 102.0,  99.0, 101.5),
            (101.5, 104.0, 101.0, 103.5),
            (103.5, 104.0, 100.0, 100.5),  // a pullback, back into the band
            (100.5, 101.0,  96.0,  96.5),
            ( 96.5,  97.0,  93.5,  94.0),
            ( 94.0,  95.0,  93.8,  94.8),
        ];
        rows.iter()
            .enumerate()
            .map(|(index, &(open, high, low, close))| Candle {
                symbol: "BTCUSDT".into(),
                timeframe: Timeframe::M5,
                open_time: index as i64 * 300_000_000_000,
                open,
                high,
                low,
                close,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            })
            .collect()
    }

    /// A series whose Heikin-Ashi range differs from its real range.
    ///
    /// Alternating gaps do it: `haOpen` is the mean of the previous *averaged*
    /// candle, so after a gap it sits outside the current candle's real range
    /// and drags `haHigh` or `haLow` with it.
    ///
    /// Worth knowing why this fixture exists at all: on a smooth trend the two
    /// ranges coincide -- `haClose` is the mean of the four prices, so it is
    /// inside `[low, high]`, and `haOpen` stays inside too. The first version of
    /// the profile test used a smooth trend and therefore passed against *both*
    /// the correct and the broken code. A guard that cannot fail is not a guard.
    fn gapped_series(count: i64) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let base = if i % 2 == 0 { 100.0 } else { 140.0 };
                candle(i, base, base + 5.0)
            })
            .collect()
    }

    /// The zoned series with zones switched on.
    fn zoned_request() -> Request {
        Request {
            zones: true,
            candles: zoned_series(),
            ..Request::default()
        }
    }

    /// A series with exactly one three-candle gap in it.
    ///
    /// Candles 4, 5 and 6 are the pattern: candle 4's high is 100.5, candle 6's
    /// low is 103.0, and nothing between them trades there. Every other window
    /// of three is checked against the rule in [`gap_concept`] and fails --
    /// which matters, because "any three-candle separation" fires again on every
    /// window that keeps separating, and a fixture that matches twice would not
    /// tell the two cases apart.
    ///
    /// The candles after the gap stay above it, so the band is still **fresh**.
    fn gap_series() -> Vec<Candle> {
        #[rustfmt::skip]
        let rows: [(f64, f64, f64, f64); 12] = [
            (100.0, 100.6,  99.2,  99.8),
            ( 99.8, 100.4,  99.0,  99.6),
            ( 99.6, 100.8,  99.1, 100.2),
            (100.2, 101.0,  99.9, 100.4),
            (100.4, 100.5,  99.5, 100.0),  // 4: the left candle -- high 100.5
            (100.0, 106.0, 100.6, 105.5),  // 5: the displacement
            (105.5, 105.6, 103.0, 104.0),  // 6: the right candle -- low 103.0
            (104.0, 105.5, 103.5, 105.0),
            (105.0, 106.0, 104.0, 105.5),
            (105.5, 106.2, 104.8, 106.0),
            (106.0, 106.5, 105.0, 105.8),
            (105.8, 106.4, 104.9, 105.2),
        ];
        rows.iter()
            .enumerate()
            .map(|(index, &(open, high, low, close))| Candle {
                symbol: "BTCUSDT".into(),
                timeframe: Timeframe::M5,
                open_time: index as i64 * 300_000_000_000,
                open,
                high,
                low,
                close,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            })
            .collect()
    }

    /// A fair value gap, written as a document and nothing else.
    ///
    /// This is the whole point of the feature, so it is worth being explicit
    /// about what is *not* here: no detector, no enum variant, no field, no
    /// entry in any list. Nothing in this workspace knows what a fair value gap
    /// is. Three numbers -- a window, a band and one requirement -- are the
    /// entire definition, and the same shape with different numbers is an order
    /// block, a breaker block or a concept nobody has named yet.
    fn gap_concept() -> Concept {
        Concept {
            name: "bullish_gap".into(),
            label: Some("bullish gap".into()),
            side: Side::Buy,
            window: 3,
            // The band is the gap itself: from the high price left behind to
            // the low price returned to.
            lower: Selector::High(0),
            upper: Selector::Low(2),
            require: vec![Requirement {
                left: Selector::High(0),
                op: Compare::Below,
                right: Selector::Low(2),
            }],
            min_band_ratio: None,
        }
    }

    // --- the basics ---------------------------------------------------------

    #[test]
    fn an_empty_request_produces_an_empty_scene_not_a_panic() {
        let scene = build(&Request::default());
        assert!(scene.candles.is_empty());
        assert!(scene.note.is_some(), "and it says why");
        assert!(scene.plot.w > 0.0 && scene.plot.h > 0.0);
    }

    #[test]
    fn every_candle_gets_a_bar_inside_the_plot() {
        let scene = build(&request(50));
        assert_eq!(scene.candles.len(), 50);
        for bar in &scene.candles {
            assert!(bar.x >= scene.plot.x - 1.0, "{bar:?}");
            assert!(
                bar.x + bar.w <= scene.plot.x + scene.plot.w + 1.0,
                "a body must not spill past the right edge: {bar:?}"
            );
            assert!(bar.wick_top <= bar.body_top, "the wick starts at the high");
            assert!(bar.body_bottom <= bar.wick_bottom);
            assert!(bar.body_top >= scene.plot.y - 1.0);
            assert!(bar.body_bottom <= scene.plot.y + scene.plot.h + 1.0);
        }
    }

    #[test]
    fn a_rising_series_is_drawn_as_up_candles() {
        let scene = build(&request(20));
        assert!(scene.candles.iter().all(|bar| bar.up));
    }

    #[test]
    fn higher_prices_are_higher_on_the_canvas() {
        // y grows downward, price grows upward: the topmost bar must be the
        // dearest one. Getting this backwards is the classic chart bug.
        let scene = build(&request(20));
        let first = &scene.candles[0];
        let last = &scene.candles[scene.candles.len() - 1];
        assert!(
            last.body_top < first.body_top,
            "the later, higher candle must be nearer the top"
        );
    }

    #[test]
    fn a_flat_series_does_not_divide_by_zero() {
        // A degenerate range must not produce NaN: a canvas silently ignores
        // NaN, so the chart would just be blank with no error anywhere.
        let flat: Vec<Candle> = (0..10).map(|i| candle(i, 100.0, 100.0)).collect();
        let scene = build(&Request {
            candles: flat,
            ..Request::default()
        });
        assert!(scene.candles.iter().all(|bar| bar.body_top.is_finite()));
        assert!(scene.candles.iter().all(|bar| bar.body_top > 0.0));
    }

    #[test]
    fn one_candle_is_a_scene_not_a_division_by_zero() {
        let scene = build(&request(1));
        assert_eq!(scene.candles.len(), 1);
        assert!(scene.candles[0].w.is_finite());
    }

    // --- the profile --------------------------------------------------------

    #[test]
    fn the_profile_is_inside_the_plot_and_never_wider_than_its_column() {
        let scene = build(&request(200));
        assert!(!scene.profile.is_empty());
        for bar in &scene.profile {
            assert!(bar.w > 0.0 && bar.w <= PROFILE_WIDTH + 1e-9, "{bar:?}");
            assert!(
                bar.x >= scene.plot.x,
                "the profile grows leftward from the right edge"
            );
            assert!(bar.buy_ratio >= 0.0 && bar.buy_ratio <= 1.0);
        }
    }

    #[test]
    fn the_longest_profile_bar_is_the_widest() {
        let scene = build(&request(300));
        let widest = scene.profile.iter().map(|bar| bar.w).fold(0.0f64, f64::max);
        let loudest = scene
            .profile
            .iter()
            .map(|bar| bar.volume)
            .fold(0.0f64, f64::max);
        let widest_volume = scene
            .profile
            .iter()
            .find(|bar| (bar.w - widest).abs() < 1e-9)
            .map(|bar| bar.volume)
            .expect("a widest bar");
        assert!(
            (widest_volume - loudest).abs() < 1e-9,
            "width must be proportional to volume"
        );
    }

    #[test]
    fn the_value_area_is_marked_on_the_profile() {
        let scene = build(&request(300));
        assert!(scene.profile.iter().any(|bar| bar.in_value_area));
        assert!(scene.profile.iter().any(|bar| !bar.in_value_area));
    }

    #[test]
    fn an_explicit_bucket_size_is_honoured() {
        let coarse = build(&Request {
            bucket_size: Some(50.0),
            ..request(300)
        });
        let fine = build(&Request {
            bucket_size: Some(0.5),
            ..request(300)
        });
        assert!(
            fine.profile.len() > coarse.profile.len(),
            "a finer bucket must produce more rows"
        );
    }

    #[test]
    fn a_nonsense_bucket_size_falls_back_rather_than_dividing_by_zero() {
        for bad in [Some(0.0), Some(-1.0), Some(f64::NAN)] {
            let scene = build(&Request {
                bucket_size: bad,
                ..request(100)
            });
            assert!(
                scene.profile.iter().all(|bar| bar.w.is_finite()),
                "bucket {bad:?} produced a non-finite bar"
            );
        }
    }

    #[test]
    fn the_chosen_bucket_is_a_round_number() {
        // So the axis reads 77,300 rather than 77,341.6667.
        for (low, high) in [(100.0, 200.0), (77_000.0, 78_000.0), (0.5, 2.5)] {
            let bucket = choose_bucket(low, high);
            let magnitude = 10f64.powf(bucket.log10().floor());
            let mantissa = bucket / magnitude;
            assert!(
                (mantissa - 1.0).abs() < 1e-9
                    || (mantissa - 2.0).abs() < 1e-9
                    || (mantissa - 5.0).abs() < 1e-9
                    || (mantissa - 10.0).abs() < 1e-9,
                "bucket {bucket} for {low}..{high} has mantissa {mantissa}"
            );
        }
    }

    // --- levels and ticks ---------------------------------------------------

    #[test]
    fn the_requested_levels_are_drawn_and_only_those() {
        let scene = build(&Request {
            lines: vec!["poc".into()],
            ..request(200)
        });
        assert_eq!(scene.levels.len(), 1);
        assert_eq!(scene.levels[0].kind, "poc");

        let all = build(&request(200));
        let kinds: Vec<&str> = all.levels.iter().map(|level| level.kind.as_str()).collect();
        for expected in ["vwap", "poc", "vah", "val"] {
            assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
        }
    }

    #[test]
    fn omitting_the_lines_field_does_not_mean_no_levels() {
        // The bug the ABI check found: `#[serde(default)]` on a `Vec` fills in
        // an empty one, so a request that never mentions `lines` drew nothing
        // while `Request::default()` promised four.
        let request: Request =
            serde_json::from_str(r#"{"candles": [], "width": 800, "height": 400}"#)
                .expect("a minimal request must deserialize");
        assert_eq!(request.lines, Request::default().lines);
        assert_eq!(request.lines.len(), 4);
    }

    #[test]
    fn an_explicit_empty_lines_list_does_mean_no_levels() {
        // Asking for none is different from not asking.
        let request: Request =
            serde_json::from_str(r#"{"candles": [], "width": 800, "height": 400, "lines": []}"#)
                .expect("must deserialize");
        assert!(request.lines.is_empty());
    }

    #[test]
    fn the_ticks_span_the_visible_range() {
        let scene = build(&request(100));
        assert!(scene.ticks.len() >= 2);
        let lowest = scene.ticks.first().expect("a first tick");
        let highest = scene.ticks.last().expect("a last tick");
        assert!((lowest.price - scene.price_min).abs() < 1e-6);
        assert!((highest.price - scene.price_max).abs() < 1e-6);
        assert!(lowest.y > highest.y);
    }

    #[test]
    fn the_profile_comes_from_the_real_series_not_the_average() {
        // The Heikin-Ashi caveat promises "the levels and the profile are
        // computed from the real series". The profile was built from the
        // averaged one -- which still looks like a profile, which is exactly why
        // nothing noticed. An explicit bucket takes `choose_bucket` out of the
        // comparison, so the only variable left is which series was read, and a
        // gapped series is what makes the two readings differ at all.
        let request = Request {
            mode: Mode::HeikinAshi,
            bucket_size: Some(0.5),
            candles: gapped_series(120),
            ..Request::default()
        };
        let scene = build(&request);

        let real = calculate_volume_profile_from_candles(&request.candles, 0.5);
        let expected: Vec<f64> = real.histogram.iter().map(|node| node.volume).collect();
        let drawn: Vec<f64> = scene.profile.iter().map(|bar| bar.volume).collect();
        assert!(
            !expected.is_empty(),
            "the fixture must produce a profile, or this compares nothing to nothing"
        );
        assert_eq!(
            drawn, expected,
            "the drawn profile must be the real series' profile"
        );
    }

    // --- zones --------------------------------------------------------------

    #[test]
    fn zones_are_absent_unless_asked_for() {
        // Off by default, and empty rather than missing when off: the shell's
        // draw loop should not need a null check.
        let scene = build(&request(60));
        assert!(scene.regions.is_empty());

        let json = serde_json::to_value(&scene).expect("serializes");
        assert_eq!(json["regions"], serde_json::json!([]));
    }

    #[test]
    fn a_zone_is_drawn_as_a_band_over_its_own_time_span() {
        let scene = build(&zoned_request());
        let zone = scene
            .regions
            .iter()
            .find(|region| region.name == "demand")
            .unwrap_or_else(|| panic!("no demand zone was drawn: {:?}", scene.regions));

        // The band starts at candle 8 -- the first origin candle -- and runs to
        // the right edge, because a zone that stopped at the break would be a
        // historical annotation rather than a level still in play.
        let slot = scene.plot.w / 20.0;
        let centre = scene.candles[8].x + scene.candles[8].w / 2.0;
        assert!(
            zone.x <= centre && zone.x >= centre - slot,
            "the zone must start inside candle 8's slot: x={} centre={centre} slot={slot}",
            zone.x
        );
        let expected_w = (20.0 - 8.0) / 20.0 * scene.plot.w;
        assert!(
            (zone.w - expected_w).abs() < 1e-6,
            "the width is the time span: {} vs {expected_w}",
            zone.w
        );
        assert!(zone.x + zone.w <= scene.plot.x + scene.plot.w + 1e-6);
    }

    #[test]
    fn the_band_carries_the_prices_it_was_detected_from() {
        // A tooltip has to be able to say which band this is. Carrying only
        // pixels would force the shell to invert the price mapping, which is
        // arithmetic and is forbidden in the shell.
        let scene = build(&zoned_request());
        let zone = scene
            .regions
            .iter()
            .find(|region| region.name == "demand")
            .expect("a demand zone");
        // The origin candles' lowest low and highest high.
        assert_eq!(zone.price_low, 92.8);
        assert_eq!(zone.price_high, 96.2);
        // And the y coordinates agree with the prices, top being dearer.
        assert!(zone.h > 0.0, "{zone:?}");
        assert!(zone.y_top >= scene.plot.y - 1.0);
        assert!(zone.y_top + zone.h <= scene.plot.y + scene.plot.h + 1.0);
        // The break that created it, so the drawing can say why it is there.
        // This is `origin` rather than two bare fields now, because a
        // client-defined pattern has no broken level and carrying one anyway
        // would be a lie that reads as data.
        assert_eq!(
            zone.origin,
            SceneOrigin::StructureBreak {
                kind: "bos".into(),
                level: 97.0,
            }
        );
        assert_eq!(zone.origin.broken_level(), Some(97.0));
        assert_eq!(zone.side, "buy");
    }

    #[test]
    fn a_zone_says_whether_it_is_still_fresh() {
        // The distinction the concept rests on: a consumed zone drawn like a
        // fresh one teaches someone to buy a level that no longer exists.
        let scene = build(&zoned_request());
        for zone in &scene.regions {
            assert!((0.0..=1.0).contains(&zone.mitigated), "{zone:?}");
            assert_eq!(zone.fresh, zone.mitigated <= 0.0, "{zone:?}");
            let expected = if zone.fresh {
                format!("{} (fresh)", zone.name)
            } else {
                format!("{} ({:.0}% mitigated)", zone.name, zone.mitigated * 100.0)
            };
            assert_eq!(zone.label, expected);
        }
    }

    #[test]
    fn no_zone_is_drawn_as_an_invisible_line() {
        // A zero-height band is a label floating on nothing, which reads as a
        // rendering bug rather than as a flat origin.
        for mode in Mode::ALL {
            let scene = build(&Request {
                mode,
                ..zoned_request()
            });
            for zone in &scene.regions {
                assert!(zone.h > 0.0, "{mode:?} drew a flat zone: {zone:?}");
                assert!(zone.w > 0.0, "{mode:?} drew a zero-width zone: {zone:?}");
                assert!(zone.price_high > zone.price_low, "{zone:?}");
            }
        }
    }

    #[test]
    fn zones_do_not_move_when_the_chart_style_changes() {
        // Zones are facts about prices that traded. Switching the chart to
        // Heikin-Ashi must not invent zones out of averaged candles -- the
        // prices, the label and the count have to be identical across every
        // rendering of the same series.
        let expected: Vec<(f64, f64, SceneOrigin, String)> = build(&zoned_request())
            .regions
            .iter()
            .map(|zone| {
                (
                    zone.price_low,
                    zone.price_high,
                    zone.origin.clone(),
                    zone.label.clone(),
                )
            })
            .collect();
        assert!(!expected.is_empty(), "the fixture must produce zones");

        for mode in Mode::ALL {
            let scene = build(&Request {
                mode,
                ..zoned_request()
            });
            let bands: Vec<(f64, f64, SceneOrigin, String)> = scene
                .regions
                .iter()
                .map(|zone| {
                    (
                        zone.price_low,
                        zone.price_high,
                        zone.origin.clone(),
                        zone.label.clone(),
                    )
                })
                .collect();
            assert_eq!(bands, expected, "{mode:?} changed the zones");
        }
    }

    #[test]
    fn the_zone_keys_the_shell_reads_are_pinned() {
        // A rename is not a compile error anywhere -- it is a zone overlay that
        // silently stops appearing, which looks like "no zones in this window"
        // rather than like a bug.
        let scene = build(&zoned_request());
        let json = serde_json::to_value(&scene).expect("serializes");
        let zone = &json["regions"][0];
        for key in [
            "name",
            "side",
            "x",
            "w",
            "y_top",
            "h",
            "price_low",
            "price_high",
            "mitigated",
            "fresh",
            "origin",
            "label",
        ] {
            assert!(!zone[key].is_null(), "the shell reads `{key}`: {zone}");
        }
        // `name` is the colour key and `side` the fallback colour, so both have
        // to be on the wire and both have to be snake_case: a concept's name is
        // whatever a client called it, and `side` is `"buy"`/`"sell"` rather
        // than the enum's own `"Buy"`.
        assert_eq!(zone["name"], "demand");
        assert_eq!(zone["side"], "buy");
    }

    // --- concepts a client wrote --------------------------------------------

    #[test]
    fn a_concept_a_client_wrote_is_drawn_without_a_detector() {
        // The feature, end to end, with nothing pre-built in the path: the
        // request carries a document, the document is measured against the
        // candles, and the result is a rectangle the shell can fill.
        let scene = build(&Request {
            concepts: vec![gap_concept()],
            candles: gap_series(),
            ..Request::default()
        });

        let band = scene
            .regions
            .iter()
            .find(|region| region.name == "bullish gap")
            .unwrap_or_else(|| panic!("the concept was not drawn: {:?}", scene.regions));

        // Exactly the band the document asked for: the left candle's high to
        // the right candle's low. Copied, not derived -- nothing here is a
        // subtraction, so exact equality is the honest assertion.
        assert_eq!(band.price_low, 100.5);
        assert_eq!(band.price_high, 103.0);
        assert_eq!(band.side, "buy");
        // A pattern, not a structure break: a client's band has no broken level,
        // and the origin says so rather than inventing one.
        assert_eq!(band.origin, SceneOrigin::Pattern);
        assert_eq!(band.origin.broken_level(), None);
        // Nothing after the gap traded back into it.
        assert!(band.fresh, "{band:?}");
        assert_eq!(band.label, "bullish gap (fresh)");

        // Geometry, not a promise: a real rectangle inside the plot.
        assert!(band.w > 0.0 && band.h > 0.0, "{band:?}");
        assert!(band.x >= scene.plot.x - 1.0);
        assert!(band.x + band.w <= scene.plot.x + scene.plot.w + 1.0);
        assert!(band.y_top >= scene.plot.y - 1.0);
        assert!(band.y_top + band.h <= scene.plot.y + scene.plot.h + 1.0);

        // It starts inside candle 4's slot -- the first candle of the pattern --
        // and runs to the right edge, which is the reason the time span is
        // mapped the way it is. A band drawn a slot off reads as a different
        // level entirely.
        let slot = scene.plot.w / 12.0;
        let centre = scene.candles[4].x + scene.candles[4].w / 2.0;
        assert!(
            band.x <= centre && band.x >= centre - slot,
            "the band must start inside candle 4's slot: x={} centre={centre} slot={slot}",
            band.x
        );
        let expected_w = (12.0 - 4.0) / 12.0 * scene.plot.w;
        assert!(
            (band.w - expected_w).abs() < 1e-6,
            "the width is the time span: {} vs {expected_w}",
            band.w
        );

        // And nothing was refused, so there is nothing to report.
        assert!(scene.note.is_none(), "{:?}", scene.note);
    }

    #[test]
    fn a_refused_concept_is_not_drawn_and_the_note_says_why() {
        // Half a pattern is worse than none, and a silent refusal teaches
        // whoever wrote the document nothing. So: the band is absent *and* the
        // reason is on the scene, naming the document.
        //
        // The ratio is the interesting refusal, because detection alone would
        // happily draw it -- every band is at least -50% of its window's range,
        // so the band being absent is the validation pass doing its job rather
        // than the detector failing to match. A guard only ever seen pass is not
        // a guard, and the same goes for a refusal that would have happened
        // anyway.
        let mut concept = gap_concept();
        concept.min_band_ratio = Some(-0.5);

        let scene = build(&Request {
            concepts: vec![concept],
            candles: gap_series(),
            ..Request::default()
        });

        assert!(scene.regions.is_empty(), "{:?}", scene.regions);
        let note = scene.note.expect("a refusal must be reported");
        assert!(note.contains("bullish_gap"), "{note}");
        assert!(note.contains("refused"), "{note}");
        assert!(note.contains("-0.5"), "{note}");
    }

    #[test]
    fn a_concept_and_a_built_in_zone_can_share_the_chart() {
        // The built-in detector is not privileged and the client's document is
        // not a special case: two producers of one shape. A chart showing both
        // must not have one overwrite the other -- and the client's band draws
        // last, so a hand-written concept is not hidden under the one that
        // ships.
        //
        // The zoned series is used rather than the gap series because the point
        // is the *coexistence*: this fixture has to produce a built-in zone, and
        // the gap concept happens to fire on it too (candle 5's high sits below
        // candle 7's low).
        let scene = build(&Request {
            zones: true,
            concepts: vec![gap_concept()],
            candles: zoned_series(),
            ..Request::default()
        });

        let zones: Vec<&SceneRegion> = scene
            .regions
            .iter()
            .filter(|region| region.name == "demand" || region.name == "supply")
            .collect();
        let gaps: Vec<&SceneRegion> = scene
            .regions
            .iter()
            .filter(|region| region.name == "bullish gap")
            .collect();
        assert!(
            !zones.is_empty(),
            "the fixture must produce a built-in zone too: {:?}",
            scene.regions
        );
        assert!(
            !gaps.is_empty(),
            "the fixture must produce the concept's band too: {:?}",
            scene.regions
        );

        let last_zone = scene
            .regions
            .iter()
            .rposition(|region| region.name == "demand" || region.name == "supply")
            .expect("a built-in zone");
        let first_gap = scene
            .regions
            .iter()
            .position(|region| region.name == "bullish gap")
            .expect("the concept's band");
        assert!(
            first_gap > last_zone,
            "concepts draw after the built-ins: {:?}",
            scene.regions
        );
    }

    // --- the chart types ----------------------------------------------------

    #[test]
    fn every_mode_produces_something_to_draw() {
        // A selector with an option that renders nothing is worse than no
        // option: the user concludes the chart is broken.
        for mode in Mode::ALL {
            let scene = build(&mode_request(mode, 120));
            assert_eq!(scene.style, mode, "the scene must say what it drew");
            // A mode may draw bars, a path, a profile or a ladder. What it must
            // never do is draw nothing, because a selector option that renders
            // an empty canvas makes the user conclude the chart is broken.
            let drew_something = !scene.candles.is_empty()
                || !scene.line.is_empty()
                || !scene.profile.is_empty()
                || scene.footprint.is_some();
            assert!(drew_something, "{mode:?} drew nothing");
        }
    }

    #[test]
    fn line_and_area_are_a_path_and_not_bars() {
        for mode in [Mode::Line, Mode::Area] {
            let scene = build(&mode_request(mode, 60));
            assert_eq!(scene.line.len(), 60, "{mode:?}");
            assert!(scene.candles.is_empty(), "{mode:?} must not also send bars");
            assert!(
                scene.profile.is_empty(),
                "{mode:?} must not overlay the profile"
            );
            for point in &scene.line {
                assert!(point.y.is_finite() && point.x.is_finite(), "{point:?}");
                assert!(point.x >= scene.plot.x - 1.0);
            }
        }
    }

    #[test]
    fn the_bars_mode_carries_the_open_and_close_ticks() {
        let scene = build(&mode_request(Mode::Bars, 40));
        assert_eq!(scene.candles.len(), 40);
        for bar in &scene.candles {
            // A bar chart draws a left tick at the open and a right tick at the
            // close, so both have to survive the trip.
            assert!(bar.open_y.is_finite() && bar.close_y.is_finite(), "{bar:?}");
            assert!(bar.open_y >= scene.plot.y - 1.0);
            assert!(bar.close_y <= scene.plot.y + scene.plot.h + 1.0);
            assert!(bar.wick_top <= bar.open_y.max(bar.close_y));
        }
    }

    #[test]
    fn the_bar_modes_share_the_same_geometry() {
        // They are three renderings of one series, so the x positions must not
        // move when the style changes -- otherwise switching styles makes the
        // chart appear to scroll sideways.
        let plain = build(&mode_request(Mode::Candles, 50));
        let bars = build(&mode_request(Mode::Bars, 50));
        let ha = build(&mode_request(Mode::HeikinAshi, 50));
        for i in 0..50 {
            assert_eq!(plain.candles[i].x, bars.candles[i].x);
            assert_eq!(plain.candles[i].x, ha.candles[i].x);
            assert_eq!(plain.candles[i].w, bars.candles[i].w);
        }
    }

    #[test]
    fn heikin_ashi_averages_and_says_so() {
        let scene = build(&mode_request(Mode::HeikinAshi, 60));
        assert!(!scene.candles.is_empty());
        assert_eq!(scene.style, Mode::HeikinAshi);
        // The caveat matters: an HA close is not a price you can trade at.
        let note = scene.note.expect("a caveat");
        assert!(note.contains("averaged"), "{note}");
    }

    #[test]
    fn the_first_heikin_ashi_candle_opens_at_its_midpoint() {
        let real = vec![candle(0, 100.0, 110.0)];
        let ha = heikin_ashi(&real);
        assert_eq!(ha.len(), 1);
        assert!(
            (ha[0].open - 105.0).abs() < 1e-9,
            "(100 + 110) / 2, got {}",
            ha[0].open
        );
        // (100 + 111 + 99 + 110) / 4
        assert!((ha[0].close - 105.0).abs() < 1e-9, "got {}", ha[0].close);
    }

    #[test]
    fn heikin_ashi_chains_from_the_previous_average() {
        // The recurrence is the whole point of the transform, and getting it
        // wrong still produces a plausible-looking chart.
        let real = vec![candle(0, 100.0, 110.0), candle(1, 110.0, 120.0)];
        let ha = heikin_ashi(&real);

        let expected_open = (ha[0].open + ha[0].close) / 2.0;
        assert!(
            (ha[1].open - expected_open).abs() < 1e-9,
            "got {}",
            ha[1].open
        );

        // And the range covers the average as well as the real extremes.
        let expected_close = (110.0 + 121.0 + 109.0 + 120.0) / 4.0;
        assert!((ha[1].close - expected_close).abs() < 1e-9);
        assert!(ha[1].high >= ha[1].open.max(ha[1].close));
        assert!(ha[1].low <= ha[1].open.min(ha[1].close));
        assert!(ha[1].high >= 121.0, "the real high still has to be inside");
        assert!(ha[1].low <= 109.0);
    }

    #[test]
    fn heikin_ashi_keeps_the_bucket_facts_untouched() {
        // The symbol, the timeframe and the open time describe the bucket, not
        // the shape, so the transform must not disturb them.
        let real = series(10);
        let ha = heikin_ashi(&real);
        for (before, after) in real.iter().zip(ha.iter()) {
            assert_eq!(before.symbol, after.symbol);
            assert_eq!(before.timeframe, after.timeframe);
            assert_eq!(before.open_time, after.open_time);
            assert_eq!(before.volume, after.volume);
            assert_eq!(before.buy_volume, after.buy_volume);
        }
    }

    #[test]
    fn heikin_ashi_smooths_a_zigzag() {
        // The reason anyone selects it: an alternating series still averages
        // into one whose bodies do not swing as far.
        let zigzag: Vec<Candle> = (0..20)
            .map(|i| {
                let base = 100.0 + (i % 2) as f64 * 4.0;
                candle(i, base, base + if i % 2 == 0 { 3.0 } else { -3.0 })
            })
            .collect();
        let ha = heikin_ashi(&zigzag);
        let biggest = ha
            .iter()
            .skip(1)
            .map(|c| (c.close - c.open).abs())
            .fold(0.0f64, f64::max);
        assert!(
            biggest < 4.0,
            "an HA body should not swing as far as the raw one: {biggest}"
        );
    }

    #[test]
    fn footprint_mode_with_trades_builds_the_grid() {
        // A ladder for two candles, one of them imbalanced.
        let mut imbalanced = footprint::ColumnCell {
            price: 100.0,
            bid: 0.4,
            ask: 2.4,
            delta: 2.0,
            imbalance: None,
        };
        imbalanced.imbalance = Some(footprint::Imbalance {
            side: "buy".into(),
            ratio: 6.0,
            stacked: 2,
        });

        let column = |open_time: i64, cells: Vec<footprint::ColumnCell>| {
            let bid: f64 = cells.iter().map(|c| c.bid).sum();
            let ask: f64 = cells.iter().map(|c| c.ask).sum();
            footprint::Column {
                open_time,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: bid + ask,
                bid_volume: bid,
                ask_volume: ask,
                delta: ask - bid,
                poc: Some(100.0),
                cells,
            }
        };

        let request = Request {
            mode: Mode::Footprint,
            footprint: vec![
                column(0, vec![imbalanced]),
                column(
                    1,
                    vec![footprint::ColumnCell {
                        price: 100.0,
                        bid: 1.0,
                        ask: 1.0,
                        delta: 0.0,
                        imbalance: None,
                    }],
                ),
            ],
            footprint_trades: 1_234,
            ..request(2)
        };
        let scene = build(&request);

        let grid = scene.footprint.expect("a trade-level grid");
        assert_eq!(grid.columns.len(), 2);
        assert_eq!(grid.rows.len(), 1, "one shared price level");
        assert_eq!(grid.stats.trades, 1_234);
        // The fallback must not also run: two footprints at once would draw a
        // profile over the ladder.
        assert!(scene.profile.is_empty());

        let first = &grid.columns[0].cells[0];
        assert_eq!(first.side.as_deref(), Some("buy"));
        assert_eq!(first.ratio, Some(6.0));
        assert_eq!(first.bid_text, "0.40");
        assert_eq!(first.ask_text, "2.40");
        // And the level is shared, so both columns agree on its height.
        assert_eq!(grid.columns[1].cells[0].y, first.y);
    }

    #[test]
    fn the_scene_serializes_for_the_shell() {
        // The whole contract with the browser is this JSON.
        let scene = build(&request(50));
        let json = serde_json::to_value(&scene).expect("the scene must serialize");
        assert!(json["candles"].is_array());
        assert!(json["plot"]["w"].is_number());
        assert_eq!(json["style"], "candles");
    }

    // --- the viewport -------------------------------------------------------

    /// A request for a window of `visible` bars starting at `from`.
    fn windowed(count: i64, from: usize, visible: usize) -> Request {
        Request {
            viewport: crate::viewport::Viewport {
                from,
                count: Some(visible),
                price: None,
            },
            ..request(count)
        }
    }

    #[test]
    fn the_window_decides_which_candles_are_drawn() {
        let scene = build(&windowed(500, 100, 50));
        assert_eq!(scene.candles.len(), 50, "the window is the slice");
        assert_eq!(scene.viewport.from, 100);
        assert_eq!(scene.viewport.count, Some(50));

        // The *right* fifty, not the first fifty. The fixture rises, so the
        // window's own prices are far above the start of the series -- and a
        // scene that ignored `from` would still pass every count assertion above
        // while drawing the wrong candles.
        //
        // Compared as prices rather than as y coordinates, because both scenes
        // fit their own window: bar 100 is the cheapest bar *in its window* and
        // so is bar 0 in its, which puts them at the same y and makes the two
        // indistinguishable on the canvas.
        let head = build(&windowed(500, 0, 50));
        assert!(
            scene.price_min > head.price_max,
            "the axis must be fitted to the window's prices ({}, {}), not to the \
             series' start ({}, {})",
            scene.price_min,
            scene.price_max,
            head.price_min,
            head.price_max
        );

        // And the times reported are the window's, which is what the shell's
        // axis label is built from.
        assert_eq!(scene.from, 100 * 300_000_000_000);
        assert_eq!(scene.to, 150 * 300_000_000_000);
    }

    #[test]
    fn zooming_in_draws_fewer_wider_bars_that_still_fill_the_plot() {
        let wide = build(&request(500));
        let narrow = build(&windowed(500, 200, 25));
        assert_eq!(wide.candles.len(), 500);
        assert_eq!(narrow.candles.len(), 25);
        assert!(
            narrow.candles[0].w > wide.candles[0].w,
            "a bar has to grow in pixels when there are fewer of them: {} vs {}",
            narrow.candles[0].w,
            wide.candles[0].w
        );

        // The slot is the plot's width over the *window*, so the drawn bars
        // still span the plot. Zooming in must not shrink the chart into its
        // left-hand corner.
        let slot = narrow.plot.w / 25.0;
        let left = narrow.plot.x;
        let right = narrow.plot.x + narrow.plot.w;
        let first = &narrow.candles[0];
        let last = &narrow.candles[24];
        assert!(
            first.x >= left - 1.0 && first.x < left + slot,
            "the first visible bar starts at the left edge: {first:?}"
        );
        assert!(
            last.x + last.w > right - slot && last.x + last.w <= right + 1.0,
            "the last visible bar reaches the right edge: {last:?}"
        );
    }

    #[test]
    fn the_profile_and_the_levels_follow_the_window() {
        // A volume profile over the whole series beside candles showing a tenth
        // of it is a chart that disagrees with itself -- and VWAP is
        // period-dependent, so a line computed over everything marks a price
        // nobody in the window traded at. It still looks like a VWAP, which is
        // exactly why this needs a test rather than an eye.
        //
        // The explicit bucket takes `choose_bucket` out of the comparison, so the
        // only variable left is which slice was read.
        let request = Request {
            bucket_size: Some(0.5),
            ..windowed(500, 100, 50)
        };
        let scene = build(&request);
        let visible = &request.candles[100..150];

        let expected = calculate_volume_profile_from_candles(visible, 0.5);
        let want: Vec<f64> = expected.histogram.iter().map(|node| node.volume).collect();
        let drawn: Vec<f64> = scene.profile.iter().map(|bar| bar.volume).collect();
        assert!(!want.is_empty(), "the fixture must produce a profile");
        assert_eq!(
            drawn, want,
            "the profile must be the window's, not the series'"
        );

        let vwap = scene
            .levels
            .iter()
            .find(|level| level.kind == "vwap")
            .expect("a vwap level");
        assert!(
            (vwap.price - calculate_vwap(visible).expect("a window vwap")).abs() < 1e-9,
            "the vwap must be the window's"
        );
        // And demonstrably not the whole series', which for a rising series is a
        // different number by a wide margin.
        let whole = calculate_vwap(&request.candles).expect("a series vwap");
        assert!(
            (vwap.price - whole).abs() > 1.0,
            "the window's vwap ({}) should differ from the series' ({whole})",
            vwap.price
        );
    }

    #[test]
    fn a_gesture_is_applied_and_the_engine_reports_what_it_resolved_to() {
        // The shell sends what the user *did*, not what it thinks the result is.
        // What comes back is the resolved window, and that is the only thing the
        // shell stores -- so a gesture the engine clamps cannot leave the shell
        // out of step with what is on screen.
        let scene = build(&Request {
            gesture: Some(crate::viewport::Gesture::ZoomTime {
                factor: 2.0,
                anchor: 0.5,
            }),
            ..request(500)
        });
        assert_eq!(
            scene.viewport.count,
            Some(250),
            "the gesture narrowed the window"
        );
        assert_eq!(scene.candles.len(), 250, "and the scene drew exactly that");

        // A pan past the oldest bar is clamped, and the scene reports the clamp
        // rather than the number that was asked for.
        let clamped = build(&Request {
            viewport: crate::viewport::Viewport {
                from: 0,
                count: Some(50),
                price: None,
            },
            gesture: Some(crate::viewport::Gesture::Pan {
                time: -50.0,
                price: 0.0,
            }),
            ..request(500)
        });
        assert_eq!(clamped.viewport.from, 0);
        assert_eq!(
            clamped.candles.len(),
            50,
            "panning must not change the zoom"
        );
    }

    #[test]
    fn a_window_outside_the_series_is_clamped_rather_than_panicking() {
        for (from, count) in [(9_999, 50), (0, 0), (0, 1), (499, 500), (usize::MAX, 10)] {
            let scene = build(&windowed(500, from, count));
            // What the scene *says* it is showing, resolved against the series it
            // was given -- the shell's next request, in other words.
            let reported = scene.viewport.resolve(500);
            assert!(
                reported.end() <= 500,
                "{from}/{count} ran past the series: {reported:?}"
            );
            assert_eq!(
                scene.candles.len(),
                reported.count,
                "{from}/{count}: the scene must draw what it says it is showing"
            );
        }
    }

    #[test]
    fn a_zoomed_window_still_anchors_zones_to_their_own_candles() {
        // Zones are detected over the whole series and clamped to the window, so
        // zooming has to move them with the candles instead of leaving them at
        // the x they had when the whole series was on screen.
        let whole = build(&zoned_request());
        let zone = whole
            .regions
            .iter()
            .find(|region| region.name == "demand")
            .expect("a demand zone in the fixture");

        // Zoomed to the last ten bars, the zone's origin (candle 8) is off the
        // left edge, so it clamps to the plot's left edge with a real width.
        let zoomed = build(&Request {
            viewport: crate::viewport::Viewport {
                from: 10,
                count: Some(10),
                price: None,
            },
            ..zoned_request()
        });
        let clipped = zoomed
            .regions
            .iter()
            .find(|region| region.name == "demand")
            .expect("a zone still in play is still drawn");
        assert!(
            (clipped.x - zoomed.plot.x).abs() < 1e-6,
            "a band that starts before the window clamps to its left edge: {} vs {}",
            clipped.x,
            zoomed.plot.x
        );
        assert!(clipped.w > 0.0, "{clipped:?}");
        assert!(clipped.x + clipped.w <= zoomed.plot.x + zoomed.plot.w + 1e-6);
        // The same band: the window changes where it is drawn, never what it is.
        assert_eq!(clipped.price_low, zone.price_low);
        assert_eq!(clipped.price_high, zone.price_high);
    }

    #[test]
    fn the_shell_reads_these_viewport_keys() {
        // A rename here is not a compile error anywhere. It is a chart that
        // forgets where the user had scrolled to, once per frame -- which reads
        // as a flicker rather than as a bug.
        let scene = build(&windowed(500, 100, 50));
        let json = serde_json::to_value(&scene).expect("serializes");
        let viewport = &json["viewport"];
        for key in ["from", "count", "price"] {
            assert!(
                viewport.get(key).is_some(),
                "the shell reads `viewport.{key}`: {viewport}"
            );
        }
        assert_eq!(viewport["from"], 100);
        assert_eq!(viewport["count"], 50);

        // And the shell sends the scene's viewport straight back, so the same
        // object has to deserialize as a request's viewport.
        let echoed: Request = serde_json::from_value(serde_json::json!({
            "candles": [],
            "width": 800.0,
            "height": 400.0,
            "viewport": viewport,
        }))
        .expect("the shell echoes the scene's viewport back");
        assert_eq!(echoed.viewport.from, 100);
        assert_eq!(echoed.viewport.count, Some(50));
    }

    #[test]
    fn a_window_with_no_candles_still_reports_the_viewport() {
        // A live feed between the backfill and its next tick momentarily has
        // nothing to draw. The note says so; the viewport has to survive it, or
        // the axis springs back the moment the candles return.
        let scene = build(&Request {
            viewport: crate::viewport::Viewport {
                from: 0,
                count: Some(50),
                price: Some(crate::viewport::PriceRange {
                    min: 150.0,
                    max: 160.0,
                }),
            },
            ..Request::default()
        });
        assert!(scene.note.is_some());
        assert_eq!(
            scene.viewport.count, None,
            "an empty series is still 'everything', not 'zero bars' -- the shell \
             echoes this back, and a zero would pin the chart to nothing"
        );
        assert_eq!(
            scene.viewport.price,
            Some(crate::viewport::PriceRange {
                min: 150.0,
                max: 160.0,
            }),
            "the user's price zoom must outlive an empty window"
        );
    }

    #[test]
    fn a_fitted_scene_keeps_following_new_candles() {
        // The wiring, not the arithmetic: `build` has to report the *request* form
        // rather than the window it sliced with, and getting that wrong is
        // invisible on the frame it happens -- the chart simply stops including
        // candles, one at a time, from then on. So the check is the round trip a
        // live chart actually performs: scene, echo, one more candle.
        let mut candles = series(200);
        let mut viewport = crate::viewport::Viewport::default();
        for added in 0..3 {
            let scene = build(&Request {
                candles: candles.clone(),
                viewport,
                ..Request::default()
            });
            assert_eq!(
                scene.candles.len(),
                candles.len(),
                "a fitted chart stopped following the market at {} bars",
                candles.len()
            );
            viewport = scene.viewport;
            candles.push(candle(200 + added, 200.0, 200.5));
        }
    }

    // --- drawings -----------------------------------------------------------

    fn absolute(time: f64, price: f64) -> Anchor {
        Anchor::Absolute { time, price }
    }

    fn fraction(x: f64, y: f64) -> Anchor {
        Anchor::Fraction { x, y }
    }

    fn trendline(a1: Anchor, a2: Anchor) -> Drawing {
        Drawing {
            id: "t1".into(),
            kind: DrawingKind::Trendline,
            a1,
            a2: Some(a2),
            label: None,
            selected: false,
        }
    }

    /// A request with 100 bars and these drawings on it.
    fn drawn(drawings: Vec<Drawing>) -> Request {
        Request {
            drawings,
            ..request(100)
        }
    }

    /// The first segment's four coordinates.
    fn first_segment(drawing: &SceneDrawing) -> (f64, f64, f64, f64) {
        drawing
            .parts
            .iter()
            .find_map(|part| match part {
                DrawingPart::Segment { x1, y1, x2, y2, .. } => Some((*x1, *y1, *x2, *y2)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no segment in {drawing:?}"))
    }

    fn handle_anchors(drawing: &SceneDrawing) -> Vec<u8> {
        drawing
            .parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Handle { anchor, .. } => Some(*anchor),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn placing_with_a_fraction_and_saving_the_answer_does_not_move_the_drawing() {
        // The property that makes the two units safe, and the reason a drag needs
        // no gesture of its own. The shell places with fractions and stores what
        // the scene reported; a reload has to put it back exactly where it was,
        // or a drawing drifts a little further every time it is dragged -- which
        // reads as the chart being slightly wrong about the user's own analysis.
        let placed = build(&drawn(vec![trendline(
            fraction(0.31, 0.62),
            fraction(0.74, 0.20),
        )]));
        let first = &placed.drawings[0];

        let reloaded = build(&drawn(vec![trendline(
            first.a1,
            first.a2.expect("a trendline has two anchors"),
        )]));
        let second = &reloaded.drawings[0];

        assert_eq!(first.a1, second.a1);
        assert_eq!(first.a2, second.a2);
        assert_eq!(first.parts, second.parts, "the pixels moved");
    }

    #[test]
    fn a_fraction_anchor_lands_where_the_pointer_was() {
        // x = 0.5 across a window of 100 five-minute bars starting at the epoch
        // is 15,000,000 ms, and y = 0.5 is the middle of the price axis. Both
        // are checked because a fraction is two coordinates and getting one
        // right hides the other.
        let scene = build(&drawn(vec![trendline(
            fraction(0.5, 0.5),
            fraction(0.25, 0.25),
        )]));
        let placed = &scene.drawings[0];
        let (time, price) = placed.a1.absolute().expect("resolved to absolute");

        assert!(
            (time - 15_000_000.0).abs() < 1.0,
            "half way across a 0..30,000,000 ms window, got {time}"
        );
        let (x1, y1, _, _) = first_segment(placed);
        assert!(
            (x1 - (scene.plot.x + scene.plot.w / 2.0)).abs() < 1e-6,
            "and half way across the plot: {x1}"
        );
        assert!(
            (y1 - (scene.plot.y + scene.plot.h / 2.0)).abs() < 1e-6,
            "and half way down it: {y1}"
        );
        // The price it resolved to is the one that lands there: the round trip
        // in one line, and the thing the shell is trusting the engine with.
        assert!(
            (price_to_y(price, scene.price_min, scene.price_max, &scene.plot) - y1).abs() < 1e-6
        );
    }

    #[test]
    fn a_stored_drawing_reports_the_same_anchors_whatever_the_window() {
        // What is *stored* must not depend on how the user is looking at it. The
        // pixels move with the window -- that is what makes a drawing track its
        // own candles -- but the anchor the shell reads back, which is what it
        // saves, has to be the number the user drew. A scene that reported a
        // window-relative anchor would rewrite every drawing on every pan.
        let one = absolute(6_000_000.0, 110.0);
        let two = absolute(12_000_000.0, 130.0);
        let whole = build(&drawn(vec![trendline(one, two)]));
        let zoomed = build(&Request {
            viewport: crate::viewport::Viewport {
                from: 10,
                count: Some(20),
                price: None,
            },
            ..drawn(vec![trendline(one, two)])
        });

        assert_eq!(whole.drawings[0].a1, zoomed.drawings[0].a1);
        assert_eq!(whole.drawings[0].a2, zoomed.drawings[0].a2);
        // And the pixels did move, or this compares nothing to nothing.
        assert_ne!(
            whole.drawings[0].parts, zoomed.drawings[0].parts,
            "the fixture must put the two windows in different places"
        );
    }

    #[test]
    fn a_drawing_whose_anchors_are_off_the_window_is_still_drawn() {
        // The zones rule, for the same reason: a trendline drawn last week is
        // still a trendline when the window has moved past it, and clipping it
        // away would erase exactly what the user scrolled back to look at. The
        // mapping is absolute and the canvas clips.
        let one = absolute(0.0, 100.0);
        let two = absolute(3_000_000.0, 120.0);
        let scene = build(&Request {
            viewport: crate::viewport::Viewport {
                from: 60,
                count: Some(20),
                price: None,
            },
            ..drawn(vec![trendline(one, two)])
        });

        let placed = &scene.drawings[0];
        assert_eq!(placed.a1, one, "the anchors are unchanged");
        let (x1, _, x2, _) = first_segment(placed);
        assert!(
            x2 < scene.plot.x,
            "both anchors are left of the window, so the segment is off-plot \
             and the canvas discards it: {x1}..{x2}"
        );
    }

    #[test]
    fn a_horizontal_line_spans_the_plot_at_its_own_price() {
        let scene = build(&drawn(vec![Drawing {
            id: "h1".into(),
            kind: DrawingKind::Hline,
            a1: absolute(6_000_000.0, 110.0),
            a2: None,
            label: None,
            selected: false,
        }]));
        let (x1, y1, x2, y2) = first_segment(&scene.drawings[0]);

        assert!(
            (x1 - scene.plot.x).abs() < 1e-9,
            "it starts at the plot's left edge"
        );
        assert!(
            (x2 - (scene.plot.x + scene.plot.w)).abs() < 1e-9,
            "and reaches its right edge, whatever x the user clicked"
        );
        assert!((y1 - y2).abs() < 1e-9, "and it is horizontal");
        let expected = price_to_y(110.0, scene.price_min, scene.price_max, &scene.plot);
        assert!(
            (y1 - expected).abs() < 1e-6,
            "at the anchor's price: {y1} vs {expected}"
        );
    }

    #[test]
    fn a_fibonacci_draws_the_retracement_levels_between_its_anchors() {
        let scene = build(&drawn(vec![Drawing {
            id: "f1".into(),
            kind: DrawingKind::Fib,
            a1: absolute(3_000_000.0, 100.0),
            a2: Some(absolute(15_000_000.0, 150.0)),
            label: None,
            selected: false,
        }]));
        let placed = &scene.drawings[0];
        let levels: Vec<f64> = placed
            .parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Segment { y1, .. } => Some(*y1),
                _ => None,
            })
            .collect();
        assert_eq!(levels.len(), FIB_LEVELS.len());

        let y_of = |price: f64| price_to_y(price, scene.price_min, scene.price_max, &scene.plot);
        // Level 0 is the first anchor and level 1 is the second. Swapping those
        // two still draws a plausible Fibonacci, which is exactly why it is
        // asserted rather than eyeballed.
        assert!((levels[0] - y_of(100.0)).abs() < 1e-6, "{levels:?}");
        assert!((levels[FIB_LEVELS.len() - 1] - y_of(150.0)).abs() < 1e-6);
        // 61.8 is 61.8% of the way from the first anchor's price to the second's.
        assert!((levels[4] - y_of(100.0 + 50.0 * 0.618)).abs() < 1e-6);

        // They span the two anchors, not the plot: a measurement drawn wider
        // than the range it measures claims something the user did not.
        let (x1, _, x2, _) = first_segment(placed);
        assert!(x1 > scene.plot.x + 1.0, "starts at the first anchor: {x1}");
        assert!(
            x2 < scene.plot.x + scene.plot.w - 1.0,
            "ends at the second: {x2}"
        );

        // The labels carry the percentage *and* the price, formatted in Rust,
        // because turning 0.618 into "61.8" is arithmetic.
        let texts: Vec<&str> = placed
            .parts
            .iter()
            .filter_map(|part| match part {
                DrawingPart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), FIB_LEVELS.len());
        assert!(texts[4].contains("61.8%"), "{texts:?}");
        assert!(texts[4].contains("130.90"), "{texts:?}");
    }

    #[test]
    fn handles_belong_to_the_selected_drawing_and_to_no_other() {
        // Every drawing's anchors at once is visual noise, and it invites a drag
        // nobody can aim. The engine decides which handles exist, which is also
        // what stops the shell from offering a grab point the engine would not
        // honour.
        let mut chosen = trendline(fraction(0.2, 0.2), fraction(0.6, 0.6));
        chosen.selected = true;
        let mut other = trendline(fraction(0.3, 0.3), fraction(0.8, 0.8));
        other.id = "t2".into();

        let scene = build(&drawn(vec![chosen, other]));
        assert_eq!(
            handle_anchors(&scene.drawings[0]),
            vec![0, 1],
            "the selected one gets one handle per anchor"
        );
        assert!(
            handle_anchors(&scene.drawings[1]).is_empty(),
            "and nothing else gets any"
        );
    }

    #[test]
    fn a_horizontal_line_offers_one_handle_even_with_a_second_anchor() {
        // A second anchor on an `hline` is permitted and stored, and the drawing
        // does not read it. Offering a handle for it would give the user a grab
        // point that moves nothing -- and the shell cannot tell the difference,
        // because a handle is a handle.
        let scene = build(&drawn(vec![Drawing {
            id: "h1".into(),
            kind: DrawingKind::Hline,
            a1: fraction(0.5, 0.4),
            a2: Some(fraction(0.9, 0.9)),
            label: None,
            selected: true,
        }]));
        assert_eq!(handle_anchors(&scene.drawings[0]), vec![0]);
    }

    #[test]
    fn a_drawing_the_engine_cannot_place_is_not_drawn_and_the_note_names_it() {
        // Two refusals that would each otherwise be a shape silently absent with
        // nothing anywhere saying why: a trendline with one anchor, and an anchor
        // that is not a number.
        let mut one_anchor = trendline(absolute(0.0, 100.0), absolute(1.0, 100.0));
        one_anchor.id = "one-anchor".into();
        one_anchor.a2 = None;

        let mut not_a_number = trendline(absolute(0.0, 100.0), absolute(1.0, 100.0));
        not_a_number.id = "not-a-number".into();
        not_a_number.a2 = Some(absolute(f64::NAN, 100.0));

        // And one that is drawable, so the test also says the refusal is
        // per-drawing rather than per-request: one bad shape must not take the
        // rest of the chart with it.
        let good = trendline(absolute(3_000_000.0, 110.0), absolute(9_000_000.0, 120.0));

        let scene = build(&drawn(vec![one_anchor, not_a_number, good]));
        assert_eq!(scene.drawings.len(), 1, "{:?}", scene.drawings);
        assert_eq!(scene.drawings[0].id, "t1");

        let note = scene.note.expect("a refusal must be reported");
        assert!(note.contains("one-anchor"), "{note}");
        assert!(note.contains("not-a-number"), "{note}");
        assert!(note.contains("2 drawing(s)"), "{note}");
    }

    #[test]
    fn a_refusal_does_not_overwrite_the_note_that_was_already_there() {
        // Three producers write to the note: a mode's own caveat, a refused
        // concept document and an unplaceable drawing. Overwriting leaves the
        // loudest one and drops the rest, which is how a chart ends up explaining
        // one of its two problems -- and the one it drops is the one the user can
        // act on.
        let mut refused = trendline(absolute(0.0, 100.0), absolute(1.0, 100.0));
        refused.a2 = None;

        let scene = build(&Request {
            mode: Mode::HeikinAshi,
            drawings: vec![refused],
            ..request(100)
        });
        let note = scene.note.expect("both notes must be there");
        assert!(note.contains("averaged"), "the mode's caveat: {note}");
        assert!(note.contains("could not be placed"), "the refusal: {note}");
    }

    #[test]
    fn a_flat_market_draws_no_drawing_rather_than_inventing_a_price_axis() {
        // A flat market is a real state: every candle at one price. There is no
        // price at the top of that chart, so an anchor cannot be positioned -- and
        // the honest answer is a note, not a drawing of a move that did not
        // happen.
        //
        // The high and low are forced as well as the open and close, because the
        // shared `candle` helper gives every candle a one-point wick: a fixture
        // built from it is not flat at all, and the first version of this test
        // proved exactly that by failing.
        let flat: Vec<Candle> = (0..50)
            .map(|i| Candle {
                high: 100.0,
                low: 100.0,
                ..candle(i, 100.0, 100.0)
            })
            .collect();
        let scene = build(&Request {
            candles: flat,
            drawings: vec![trendline(
                absolute(0.0, 100.0),
                absolute(1_000_000.0, 100.0),
            )],
            ..Request::default()
        });
        assert!(scene.drawings.is_empty());
        let note = scene.note.expect("a note");
        assert!(note.contains("usable prices"), "{note}");
    }

    #[test]
    fn the_shell_reads_these_drawing_keys() {
        // A rename here is not a compile error anywhere. It is a drawing that
        // stops appearing, or -- worse -- one whose handle the shell cannot find,
        // so it can be drawn and never moved.
        let scene = build(&drawn(vec![Drawing {
            id: "d1".into(),
            kind: DrawingKind::Trendline,
            a1: absolute(3_000_000.0, 110.0),
            a2: Some(absolute(12_000_000.0, 130.0)),
            label: Some("the one I keep watching".into()),
            selected: true,
        }]));
        let json = serde_json::to_value(&scene).expect("serializes");
        let drawing = &json["drawings"][0];

        for key in [
            "id",
            "kind",
            "label",
            "selected",
            "a1",
            "a2",
            "a1_fraction",
            "a2_fraction",
            "parts",
        ] {
            assert!(
                !drawing[key].is_null(),
                "the shell reads `{key}`: {drawing}"
            );
        }
        assert_eq!(drawing["id"], "d1");
        assert_eq!(drawing["kind"], "trendline");
        assert_eq!(drawing["selected"], true);

        // The anchors are the API's own body, so the shell posts these two fields
        // straight back. Both have to be absolute by the time they get here: a
        // fraction on the wire to storage would be a drawing whose position
        // depends on the window it happened to be saved from.
        for anchor in ["a1", "a2"] {
            assert_eq!(drawing[anchor]["unit"], "absolute", "{anchor}");
            assert!(drawing[anchor]["time"].is_number(), "{anchor}");
            assert!(drawing[anchor]["price"].is_number(), "{anchor}");
        }

        // And the fractions are a plain `{x, y}`, not an `Anchor`. They are only
        // ever fractions, so a `unit` tag here would be a field the shell has to
        // read and can never find to be anything but `"fraction"` -- which is a
        // branch that cannot be taken, dressed up as an interface.
        for key in ["a1_fraction", "a2_fraction"] {
            assert!(drawing[key]["x"].is_number(), "{key}");
            assert!(drawing[key]["y"].is_number(), "{key}");
            assert!(drawing[key]["unit"].is_null(), "{key} is not an Anchor");
        }

        // And the parts carry their shape tag, which is what the shell switches
        // on to decide whether to stroke, fill, label or place a handle.
        let shapes: Vec<&str> = drawing["parts"]
            .as_array()
            .expect("parts is an array")
            .iter()
            .map(|part| part["shape"].as_str().expect("a shape tag"))
            .collect();
        assert!(shapes.contains(&"segment"), "{shapes:?}");
        assert!(shapes.contains(&"handle"), "{shapes:?}");
    }

    #[test]
    fn a_drawing_with_no_id_is_refused_by_name() {
        // The scene needs the id for two things -- naming a refusal, and telling
        // the shell which drawing it grabbed -- so an empty one makes both
        // impossible. Storage mints its own, so this is the scene's rule rather
        // than the request body's, and the distinction is asserted on both sides.
        let mut unnamed = trendline(fraction(0.2, 0.2), fraction(0.6, 0.6));
        unnamed.id = "   ".into();

        let scene = build(&drawn(vec![unnamed]));
        assert!(scene.drawings.is_empty());
        let note = scene.note.expect("a refusal must be reported");
        assert!(note.contains("no id"), "{note}");
    }

    #[test]
    fn a_drawing_survives_the_request_wire() {
        // The shell sends this object, so it has to deserialize from the same
        // JSON the scene serializes -- the two ends of one contract.
        let request: Request = serde_json::from_str(
            r#"{"candles": [], "width": 800, "height": 400,
                "drawings": [{"id": "d1", "kind": "hline",
                              "a1": {"unit": "fraction", "x": 0.4, "y": 0.6}}]}"#,
        )
        .expect("a minimal request with a drawing must deserialize");
        assert_eq!(request.drawings.len(), 1);
        assert_eq!(request.drawings[0].kind, DrawingKind::Hline);
        assert_eq!(request.drawings[0].a2, None);
        assert!(!request.drawings[0].selected);

        // And no drawings at all is not an error: the field is optional, which is
        // what keeps an older shell working against a newer engine.
        let bare: Request = serde_json::from_str(r#"{"candles": [], "width": 800, "height": 400}"#)
            .expect("must deserialize");
        assert!(bare.drawings.is_empty());
    }

    // --- moving a whole drawing ---------------------------------------------
    //
    // The shell's body drag: read the fractions the engine reported, add a
    // screen-space delta to *both* anchors, and send them back. The shell does
    // the addition -- two fractions, the same class of arithmetic as the
    // division in `plotFraction` -- and the engine does the conversion, so the
    // price scale never leaves Rust.

    /// A reported fraction moved by a fraction of the plot, as an anchor.
    fn moved(anchor: Fraction, dx: f64, dy: f64) -> Anchor {
        Anchor::Fraction {
            x: anchor.x + dx,
            y: anchor.y + dy,
        }
    }

    /// The first rectangle's four numbers.
    fn first_rect(drawing: &SceneDrawing) -> (f64, f64, f64, f64) {
        drawing
            .parts
            .iter()
            .find_map(|part| match part {
                DrawingPart::Rect { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no rect in {drawing:?}"))
    }

    fn rect(a1: Anchor, a2: Anchor) -> Drawing {
        Drawing {
            id: "r1".into(),
            kind: DrawingKind::Rect,
            a1,
            a2: Some(a2),
            label: None,
            selected: false,
        }
    }

    #[test]
    fn a_body_drag_moves_both_ends_of_a_line_by_the_same_pixels() {
        // The property, and the bug it replaces: dragging the *body* of a
        // drawing used to send one anchor to the pointer and leave the other
        // where it was, which turns a translation into a stretch. A trendline
        // dragged by its middle has to keep its slope and its length.
        let before = build(&drawn(vec![trendline(
            fraction(0.2, 0.7),
            fraction(0.6, 0.3),
        )]));
        let start = before.drawings[0].clone();
        let (dx, dy) = (0.1, -0.05);

        let after = build(&drawn(vec![trendline(
            moved(start.a1_fraction, dx, dy),
            moved(
                start
                    .a2_fraction
                    .expect("a trendline is a kind with two anchors"),
                dx,
                dy,
            ),
        )]));

        let (x1, y1, x2, y2) = first_segment(&before.drawings[0]);
        let (nx1, ny1, nx2, ny2) = first_segment(&after.drawings[0]);

        // The same delta at both ends, expressed in pixels rather than asserted
        // in prices: the shell's delta is a fraction of the plot, so that is what
        // it has to come back as.
        let want_x = dx * before.plot.w;
        let want_y = dy * before.plot.h;
        for (end, got) in [("first", nx1 - x1), ("second", nx2 - x2)] {
            assert!(
                (got - want_x).abs() < 1e-6,
                "the {end} end moved {got} across, wanted {want_x}"
            );
        }
        for (end, got) in [("first", ny1 - y1), ("second", ny2 - y2)] {
            assert!(
                (got - want_y).abs() < 1e-6,
                "the {end} end moved {got} down, wanted {want_y}"
            );
        }
    }

    #[test]
    fn a_body_drag_does_not_resize_a_rectangle() {
        // The same property on the kind where getting it wrong is loudest: a
        // rectangle dragged by its middle that collapses to a corner is the
        // single most visible way for a body drag to be broken.
        let before = build(&drawn(vec![rect(fraction(0.25, 0.75), fraction(0.5, 0.5))]));
        let start = before.drawings[0].clone();
        let (dx, dy) = (0.12, 0.08);

        let after = build(&drawn(vec![rect(
            moved(start.a1_fraction, dx, dy),
            moved(
                start.a2_fraction.expect("a rectangle has two anchors"),
                dx,
                dy,
            ),
        )]));

        let (x, y, w, h) = first_rect(&before.drawings[0]);
        let (nx, ny, nw, nh) = first_rect(&after.drawings[0]);
        assert!((nw - w).abs() < 1e-6, "width went {w} -> {nw}");
        assert!((nh - h).abs() < 1e-6, "height went {h} -> {nh}");
        assert!((nx - x - dx * before.plot.w).abs() < 1e-6, "x {x} -> {nx}");
        assert!((ny - y - dy * before.plot.h).abs() < 1e-6, "y {y} -> {ny}");
    }

    #[test]
    fn a_drawing_stored_as_a_timestamp_still_reports_a_fraction_to_drag_by() {
        // Every drawing loaded from the API arrives *absolute* -- that is the
        // only form storage keeps. If the engine reported fractions only for
        // anchors that arrived as fractions, a saved drawing would be undraggable
        // until it had been dragged once, which is a feature that works the
        // second time and not the first.
        let scene = build(&drawn(vec![trendline(
            absolute(1_700_000_000_000.0, 100.0),
            absolute(1_700_000_600_000.0, 110.0),
        )]));
        let stored = &scene.drawings[0];
        let a2 = stored.a2_fraction.expect("a trendline has two anchors");
        assert!(
            stored.a1_fraction.x.is_finite() && stored.a1_fraction.y.is_finite(),
            "a stored drawing must report a usable drag base: {:?}",
            stored.a1_fraction
        );

        // And the fraction is the *same point*: feeding it back as a fraction
        // lands on the pixels the absolute anchors produced. That is what makes
        // it usable as a base to add a delta to, rather than merely a number.
        let again = build(&drawn(vec![trendline(
            Anchor::Fraction {
                x: stored.a1_fraction.x,
                y: stored.a1_fraction.y,
            },
            Anchor::Fraction { x: a2.x, y: a2.y },
        )]));
        let (x1, y1, x2, y2) = first_segment(stored);
        let (rx1, ry1, rx2, ry2) = first_segment(&again.drawings[0]);
        for (what, got, want) in [
            ("x1", rx1, x1),
            ("y1", ry1, y1),
            ("x2", rx2, x2),
            ("y2", ry2, y2),
        ] {
            assert!(
                (got - want).abs() < 1e-9,
                "{what}: the fraction gave {got}, the timestamp gave {want}"
            );
        }
    }

    #[test]
    fn a_horizontal_line_moved_by_its_body_keeps_spanning_the_plot() {
        // An `hline` has one anchor, so a body drag shifts its price and nothing
        // else. Its width is not a property it has -- the engine draws it across
        // the plot -- so a horizontal component must not be able to do anything.
        let before = build(&drawn(vec![Drawing {
            id: "h1".into(),
            kind: DrawingKind::Hline,
            a1: fraction(0.3, 0.4),
            a2: None,
            label: None,
            selected: false,
        }]));
        let start = before.drawings[0].clone();

        let after = build(&drawn(vec![Drawing {
            id: "h1".into(),
            kind: DrawingKind::Hline,
            a1: moved(start.a1_fraction, 0.25, 0.1),
            a2: None,
            label: None,
            selected: false,
        }]));

        let (x1, y1, x2, y2) = first_segment(&before.drawings[0]);
        let (nx1, ny1, nx2, ny2) = first_segment(&after.drawings[0]);
        assert!(
            (ny1 - y1 - 0.1 * before.plot.h).abs() < 1e-6,
            "the price did not follow the drag: {y1} -> {ny1}"
        );
        assert!(
            (ny2 - y2 - 0.1 * before.plot.h).abs() < 1e-6,
            "the two ends of one horizontal line disagreed"
        );
        // Across, it is the plot's own edges both times -- a horizontal drag of
        // the body is not a thing this drawing can respond to.
        for (got, want) in [(nx1, x1), (nx2, x2)] {
            assert!(
                (got - want).abs() < 1e-6,
                "a horizontal line should ignore the across component: {want} -> {got}"
            );
        }
    }

    // --- a click with no drag -------------------------------------------------

    #[test]
    fn a_click_with_no_drag_is_refused_rather_than_stored_without_extent() {
        // The tool is a drag, so a click places both anchors at one point. That
        // is a shape with no extent: nothing is drawn, and it is stored anyway --
        // so it is still there on the next reload, and the user who clicked is
        // left believing the tool does not work.
        let scene = build(&drawn(vec![trendline(
            fraction(0.3, 0.3),
            fraction(0.3, 0.3),
        )]));
        assert!(scene.drawings.is_empty(), "{:?}", scene.drawings);
        let note = scene.note.expect("a refusal must be reported");
        assert!(note.contains("no extent"), "{note}");
    }

    #[test]
    fn a_vertical_trendline_is_a_drawing_and_not_a_degenerate_one() {
        // One shared coordinate is not the same as one shared point. A vertical
        // trendline and a flat rectangle are both real things to draw, which is
        // why the rule compares the whole anchor rather than a coordinate.
        let scene = build(&drawn(vec![trendline(
            fraction(0.3, 0.2),
            fraction(0.3, 0.8),
        )]));
        assert_eq!(scene.drawings.len(), 1, "{:?}", scene.note);
        assert!(scene.note.is_none(), "{:?}", scene.note);
    }

    #[test]
    fn a_horizontal_line_is_one_point_and_is_not_refused_for_it() {
        // The rule is about kinds that need *two* anchors, so the one kind that
        // does not is untouched. It matters more than it looks: `hline` is placed
        // by a click, so a rule written without the kind check would refuse the
        // only tool that is not a drag.
        let scene = build(&drawn(vec![Drawing {
            id: "h1".into(),
            kind: DrawingKind::Hline,
            a1: fraction(0.3, 0.4),
            a2: None,
            label: None,
            selected: false,
        }]));
        assert_eq!(scene.drawings.len(), 1);
        assert!(scene.note.is_none(), "{:?}", scene.note);
    }

    // --- the answer's own levels --------------------------------------------

    fn overlay(price: f64, role: OverlayRole, band_to: Option<f64>) -> Overlay {
        Overlay {
            price,
            label: role.name().to_string(),
            role,
            band_to,
            filled: false,
        }
    }

    fn with_overlays(overlays: Vec<Overlay>, count: i64) -> Request {
        Request {
            overlays,
            ..request(count)
        }
    }

    #[test]
    fn an_answers_levels_are_positioned_against_the_engines_own_price_scale() {
        // The whole reason the overlays travel as prices: the engine owns the
        // scale, so the y a level gets here is the same y the candle at that
        // price gets. A shell mapping the prices itself is a second copy of
        // that arithmetic, and this asserts there is only one.
        let scene = build(&with_overlays(
            vec![
                overlay(105.0, OverlayRole::Entry, None),
                overlay(110.0, OverlayRole::Target, None),
            ],
            20,
        ));
        assert_eq!(scene.overlays.len(), 2, "{:?}", scene.note);
        assert!(scene.note.is_none(), "{:?}", scene.note);

        let frame = Frame {
            plot: scene.plot,
            from: scene.from,
            to: scene.to,
            price_min: scene.price_min,
            price_max: scene.price_max,
        };
        for placed in &scene.overlays {
            assert!(
                (placed.y - frame.y_at(placed.price)).abs() < 1e-9,
                "{} sat at {}, the scale says {}",
                placed.label,
                placed.y,
                frame.y_at(placed.price)
            );
        }
    }

    #[test]
    fn a_higher_price_is_drawn_higher_up_the_canvas() {
        // y grows downward and price grows upward, and an overlay is the one
        // place that inversion is easy to get backwards because the caller
        // never sees a pixel. If it were backwards the stop and target would
        // swap places, which is a chart that quietly argues for the other side.
        let scene = build(&with_overlays(
            vec![
                overlay(110.0, OverlayRole::Target, None),
                overlay(100.0, OverlayRole::Stop, None),
            ],
            20,
        ));
        let target = scene
            .overlays
            .iter()
            .find(|o| o.role == OverlayRole::Target)
            .expect("the target is drawn");
        let stop = scene
            .overlays
            .iter()
            .find(|o| o.role == OverlayRole::Stop)
            .expect("the stop is drawn");
        assert!(
            target.y < stop.y,
            "the target at {} drew at y={} and the stop at {} drew at y={}",
            target.price,
            target.y,
            stop.price,
            stop.y
        );
    }

    #[test]
    fn generated_indicator_evidence_is_positioned_and_linked_by_the_engine() {
        let first = 20 * 300_000_000_000;
        let second = 40 * 300_000_000_000;
        let output = IndicatorOutput {
            revision_id: "revision-7".into(),
            evidence: vec![
                Evidence {
                    id: "sweep".into(),
                    event: "liquidity_sweep".into(),
                    time: first,
                    price: 108.0,
                    explanation: "Price swept the prior low.".into(),
                },
                Evidence {
                    id: "choch".into(),
                    event: "bullish_choch".into(),
                    time: second,
                    price: 116.0,
                    explanation: "Close broke the prior swing high.".into(),
                },
            ],
            zones: vec![],
            markers: vec![IndicatorMarker {
                id: "sweep-marker".into(),
                evidence_id: "sweep".into(),
                time: first,
                price: 108.0,
                label: "Sweep".into(),
                kind: MarkerKind::Context,
            }],
            links: vec![EvidenceLink {
                id: "sweep-to-choch".into(),
                from: "sweep".into(),
                to: "choch".into(),
            }],
        };
        let scene = build(&Request {
            indicator: Some(output),
            ..request(100)
        });
        let indicator = scene.indicator.expect("valid output is drawn");
        assert_eq!(indicator.revision_id, "revision-7");
        assert_eq!(
            indicator.markers[0].explanation,
            "Price swept the prior low."
        );
        assert!(indicator.links[0].from_x < indicator.links[0].to_x);
        assert!(indicator.links[0].control_y < indicator.links[0].from_y);
    }

    #[test]
    fn a_band_reports_both_edges_in_canvas_pixels() {
        // So the shell can shade without subtracting two pixels whose order it
        // would have to guess at.
        let scene = build(&with_overlays(
            vec![Overlay {
                filled: true,
                ..overlay(105.0, OverlayRole::Entry, Some(110.0))
            }],
            20,
        ));
        assert_eq!(scene.overlays.len(), 1, "{:?}", scene.note);
        let band = scene.overlays[0].band_y.expect("the band is positioned");
        assert!(
            band < scene.overlays[0].y,
            "the far edge at 110 must sit above the level at 105"
        );
    }

    #[test]
    fn a_level_with_no_band_reports_no_band() {
        // Not a band of zero height. The shell strokes a line for `None` and
        // fills a rectangle for `Some`, so collapsing the two would make every
        // level a rectangle it has to special-case back into a line.
        let scene = build(&with_overlays(
            vec![overlay(105.0, OverlayRole::Level, None)],
            20,
        ));
        assert_eq!(scene.overlays[0].band_y, None);
    }

    #[test]
    fn a_level_that_is_not_a_number_is_refused_and_says_so() {
        // Silently dropped would read as "the answer had no stop", which is a
        // claim about the analysis. The note makes it a claim about the number.
        //
        // The refusal path is reached by *direct construction* here rather than
        // through JSON -- `drawing::tests::a_null_price_never_reaches_the_validator`
        // asserts that serde rejects a `null` before this code runs. What is
        // being tested is that the scene reports the refusal and keeps the
        // levels it can draw, not that the shell can produce a `NaN`.
        let scene = build(&with_overlays(
            vec![
                overlay(f64::NAN, OverlayRole::Stop, None),
                overlay(105.0, OverlayRole::Entry, None),
            ],
            20,
        ));
        assert_eq!(scene.overlays.len(), 1, "only the usable one is drawn");
        assert_eq!(scene.overlays[0].role, OverlayRole::Entry);
        let note = scene.note.expect("a refusal must be reported");
        assert!(note.contains("stop"), "{note}");
    }

    #[test]
    fn an_overlay_never_costs_the_chart_its_candles() {
        // The failure mode this guards: an overlay so malformed that building
        // the scene bails out. A chart with no candles because the AI's stop
        // was `NaN` is a much worse outcome than a chart without the stop.
        let scene = build(&with_overlays(
            vec![
                overlay(f64::INFINITY, OverlayRole::Target, None),
                overlay(f64::NAN, OverlayRole::Entry, None),
            ],
            20,
        ));
        assert!(!scene.candles.is_empty(), "the candles survived");
        assert!(scene.overlays.is_empty());
        assert!(scene.note.is_some(), "and both refusals are explained");
    }

    #[test]
    fn a_request_that_says_nothing_about_overlays_gets_none() {
        // `#[serde(default)]` filling an empty `Vec` is correct here, unlike
        // `lines`: there is no standard set of levels an answer cited.
        let parsed: Request = serde_json::from_value(serde_json::json!({
            "candles": [], "width": 800.0, "height": 400.0
        }))
        .expect("a bare request parses");
        assert!(parsed.overlays.is_empty());
        assert!(build(&parsed).overlays.is_empty());
    }
}
