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
    /// First candle's open time.
    pub from: i64,
    /// Last candle's close time.
    pub to: i64,
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
        candles: Vec::new(),
        line: Vec::new(),
        profile: Vec::new(),
        levels: Vec::new(),
        regions: Vec::new(),
        ticks: Vec::new(),
        footprint: None,
        note: None,
    };

    if request.candles.is_empty() {
        scene.note = Some("no candles in this window".into());
        return scene;
    }

    // Heikin-Ashi candles are derived from the series, so the range has to be
    // measured from the *drawn* values: an HA candle can sit outside every real
    // high or low in the window, and clamping to the raw range would push it off
    // the plot.
    let plotted: Vec<Candle> = match request.mode {
        Mode::HeikinAshi => heikin_ashi(&request.candles),
        _ => request.candles.clone(),
    };

    // The visible range is the drawn candles' own range, padded a little so a
    // wick touching the edge is not clipped.
    let mut price_min = f64::INFINITY;
    let mut price_max = f64::NEG_INFINITY;
    for candle in &plotted {
        price_min = price_min.min(candle.low);
        price_max = price_max.max(candle.high);
    }
    if !price_min.is_finite() || !price_max.is_finite() {
        scene.note = Some("candles have no usable prices".into());
        return scene;
    }
    let pad = ((price_max - price_min) * 0.04).max(f64::EPSILON);
    scene.price_min = price_min - pad;
    scene.price_max = price_max + pad;

    let first = &plotted[0];
    let last = &plotted[plotted.len() - 1];
    let width_nanos = first.timeframe.nanos().max(1);
    scene.from = first.open_time;
    scene.to = last.open_time + width_nanos;

    let slot = plot.w / plotted.len() as f64;
    if request.mode.draws_bars() {
        scene.candles = candle_bars(&plotted, slot, &plot, scene.price_min, scene.price_max);
    }
    if matches!(request.mode, Mode::Line | Mode::Area) {
        scene.line = close_path(&plotted, slot, &plot, scene.price_min, scene.price_max);
    }

    let bucket_size = request
        .bucket_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or_else(|| choose_bucket(price_min, price_max));
    // From the **real** series, not `plotted`. A volume profile assigns each
    // candle's volume to price levels, and a Heikin-Ashi candle's high and low
    // are averages -- prices nobody traded at. The profile would still look
    // plausible, which is exactly why it is worth being explicit: the note for
    // that mode promises the real series, and this is where that promise is
    // kept. `levels()` reads `request.candles` for the same reason.
    let profile = calculate_volume_profile_from_candles(&request.candles, bucket_size);

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
                scene.note =
                    crate::footprint::truncation_note(&request.footprint).map(|note| note.message);
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

    scene.levels = levels(request, &profile, &plot, scene.price_min, scene.price_max);
    scene.regions = region_rects(
        request.zones,
        &concepts,
        &request.candles,
        &Mapping {
            plot,
            from: scene.from,
            to: scene.to,
            price_min: scene.price_min,
            price_max: scene.price_max,
        },
    );
    scene.ticks = ticks(&plot, scene.price_min, scene.price_max);

    if !refused.is_empty() {
        let message = format!(
            "{} concept document(s) were refused and are not drawn: {}",
            refused.len(),
            refused.join("; ")
        );
        scene.note = Some(match scene.note.take() {
            Some(existing) => format!("{existing} {message}"),
            None => message,
        });
    }

    scene
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

fn levels(
    request: &Request,
    profile: &VolumeProfile,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<Level> {
    let mut out = Vec::new();
    let wanted = |name: &str| request.lines.iter().any(|line| line == name);

    if wanted("vwap") {
        if let Some(vwap) = calculate_vwap(&request.candles) {
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

/// Where each region's band lands on the canvas.
///
/// The time mapping is the same one the candles use: a region's edges are
/// timestamps, and the plot's width is divided by the window's duration exactly
/// as a candle's slot is. That is what makes a band line up with the candles
/// that formed it instead of drifting sideways -- a band drawn a slot off reads
/// as a different level entirely.
///
/// `candles` is the **real** series, not the drawn one. [`build`] passes
/// `request.candles` rather than its own `plotted`, deliberately: structure and
/// patterns are facts about prices that traded, so switching the chart to
/// Heikin-Ashi must not invent bands out of averaged candles. That holds for
/// both producers below, because they are handed the same series here.
/// Where a price and a time land on the canvas.
///
/// One struct rather than four loose numbers because they are one thing: the
/// mapping from `(timestamp, price)` to `(x, y)`. Handing them over whole is
/// also what keeps a caller from pairing the wrong `from` with the wrong `to` --
/// four same-shaped parameters in a row is a mistake the compiler cannot see,
/// and a band drawn against the wrong window is drawn silently wrong.
#[derive(Debug, Clone, Copy)]
struct Mapping {
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

impl Mapping {
    /// A price's canvas y.
    fn y(&self, price: f64) -> f64 {
        price_to_y(price, self.price_min, self.price_max, &self.plot)
    }
}

fn region_rects(
    zones: bool,
    concepts: &[Concept],
    candles: &[Candle],
    map: &Mapping,
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
            let y_top = map.y(region.price_high);
            let y_bottom = map.y(region.price_low);
            Some(SceneRegion {
                label: region_label(&region.name, mitigated),
                name: region.name,
                side: region.side.name().to_owned(),
                x: map.plot.x + (start - map.from) as f64 / span * map.plot.w,
                w: (end - start) as f64 / span * map.plot.w,
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
    use crate::footprint;
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
}
