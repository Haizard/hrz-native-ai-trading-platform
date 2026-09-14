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
    /// Overlay levels to draw.
    ///
    /// `serde(default = "default_lines")` rather than a bare `#[serde(default)]`:
    /// the latter fills in `Vec::default()`, which is *empty*, so a request that
    /// omits the field would draw no levels at all -- while [`Request::default`]
    /// promises the standard four. Two defaults disagreeing is a trap, and the
    /// ABI check found it.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
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
            lines: default_lines(),
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

/// One footprint cell: a bucket with its buy/sell split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
    /// The bucket's midpoint price.
    pub price: f64,
    /// Buy-aggressed volume.
    pub buy: f64,
    /// Sell-aggressed volume.
    pub sell: f64,
    /// Buy minus sell.
    pub delta: f64,
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
    /// Footprint cells, cheapest first. Only in [`Mode::Footprint`].
    pub cells: Vec<Cell>,
    /// Overlay levels.
    pub levels: Vec<Level>,
    /// Price-axis ticks.
    pub ticks: Vec<Tick>,
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
/// A profile with 2 rows says nothing and one with 4,000 is a solid block. The
/// target is ~40 rows over the visible range, rounded to a power of ten times
/// 1, 2 or 5 -- so the prices on the axis are round numbers rather than
/// `77341.6667`.
fn choose_bucket(price_min: f64, price_max: f64) -> f64 {
    let span = (price_max - price_min).max(f64::EPSILON);
    let raw = span / PROFILE_ROWS;
    let magnitude = 10f64.powf(raw.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        let candidate = magnitude * step;
        if candidate >= raw {
            return candidate;
        }
    }
    magnitude * 10.0
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
        cells: Vec::new(),
        levels: Vec::new(),
        ticks: Vec::new(),
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
    let profile = calculate_volume_profile_from_candles(&plotted, bucket_size);

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
            scene.cells = footprint_cells(&profile, &plot, scene.price_min, scene.price_max);
            // Say which footprint this is. A true one needs trades, and there
            // are none -- the agent's own tooling reports the same thing, and
            // the chart should not imply otherwise.
            scene.note = Some(
                "volume by price, from candles: this deployment stores no tick data, so a \
                 trade-level footprint cannot be built. Buy/sell here is the candle's own split."
                    .into(),
            );
        }
        Mode::Candles | Mode::Bars | Mode::Line | Mode::Area => {}
    }

    scene.levels = levels(request, &profile, &plot, scene.price_min, scene.price_max);
    scene.ticks = ticks(&plot, scene.price_min, scene.price_max);
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

fn footprint_cells(
    profile: &VolumeProfile,
    plot: &Plot,
    price_min: f64,
    price_max: f64,
) -> Vec<Cell> {
    let row_height = (plot.h / PROFILE_ROWS).max(1.0);
    profile
        .histogram
        .iter()
        .filter(|node| node.price_level >= price_min && node.price_level <= price_max)
        .map(|node| {
            let y = price_to_y(node.price_level, price_min, price_max, plot);
            Cell {
                x: plot.x,
                y: y - row_height / 2.0,
                w: plot.w,
                h: row_height,
                price: node.price_level,
                buy: node.buy_volume,
                sell: node.sell_volume,
                delta: node.buy_volume - node.sell_volume,
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

    // --- the chart types ----------------------------------------------------

    #[test]
    fn every_mode_produces_something_to_draw() {
        // A selector with an option that renders nothing is worse than no
        // option: the user concludes the chart is broken.
        for mode in Mode::ALL {
            let scene = build(&mode_request(mode, 120));
            assert_eq!(scene.style, mode, "the scene must say what it drew");
            let drew_something =
                !scene.candles.is_empty() || !scene.line.is_empty() || !scene.cells.is_empty();
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
    fn footprint_mode_produces_cells_and_says_which_footprint_it_is() {
        let scene = build(&mode_request(Mode::Footprint, 200));
        assert!(!scene.cells.is_empty());
        assert!(scene.profile.is_empty(), "one view at a time");
        assert!(scene.candles.is_empty());
        let note = scene.note.expect("a caveat");
        assert!(note.contains("no tick data"), "{note}");
    }

    #[test]
    fn a_footprint_cell_delta_is_buy_minus_sell() {
        let scene = build(&mode_request(Mode::Footprint, 200));
        for cell in &scene.cells {
            assert!(
                (cell.delta - (cell.buy - cell.sell)).abs() < 1e-9,
                "{cell:?}"
            );
        }
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
