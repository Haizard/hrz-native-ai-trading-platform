//! The measurement group: a box between two points that reports a number.
//!
//! Both kinds draw the same outline as [`crate::draw::shapes::rect`] and differ
//! in what they say about it. That is deliberate: a measurement is a question
//! about two prices and two times, and the box is only the frame around the
//! answer.
//!
//! ## The numbers are formatted here
//!
//! A percentage and a bar count are arithmetic, and `docs/14` keeps arithmetic out
//! of JavaScript. The bar count is measured against the series' own timeframe,
//! which the [`crate::scene::Frame`] carries, rather than guessed from the pixel
//! width -- the same reason the price scale is in Rust.

use crate::drawing::DrawingPart;

use super::shapes::corners;
use super::Geometry;

/// The price/distance ruler: an empty box, guides to both axes, and the delta.
#[must_use]
pub fn ruler(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((left, top, w, h)) = corners(geometry) else {
        return Vec::new();
    };
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let (t1, p1) = geometry.points[0];
    let (t2, p2) = geometry.points[1];
    let bars = bar_count(geometry, t1, t2);
    let change = (p2 - p1).abs() / p1.abs().max(f64::EPSILON) * 100.0;
    vec![
        // Unfilled: it is a measurement, and a filled one would read as a zone
        // the user placed.
        DrawingPart::Rect {
            x: left,
            y: top,
            w,
            h,
            filled: false,
        },
        // Dashed guides to both axes, so which two things are being compared is
        // on the chart rather than implied.
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2,
            y2: y1,
            dashed: true,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2: x1,
            y2,
            dashed: true,
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: top + 10.0,
            text: format!("{:.2} ({change:.1}%) {bars} bars", (p2 - p1).abs()),
        },
    ]
}

/// The date-range box: the same drawing, reporting the range as a percentage.
///
/// The parity tool's whole addition is the signed percentage instead of the
/// absolute delta, which is the question a date range actually asks -- "what did
/// this stretch of time do to price?"
#[must_use]
pub fn range(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let Some((left, top, w, h)) = corners(geometry) else {
        return Vec::new();
    };
    let (x1, y1) = (geometry.x(0), geometry.y(0));
    let (x2, y2) = (geometry.x(1), geometry.y(1));
    let (t1, p1) = geometry.points[0];
    let (t2, p2) = geometry.points[1];
    let bars = bar_count(geometry, t1, t2);
    let change = (p2 - p1) / p1.abs().max(f64::EPSILON) * 100.0;
    vec![
        DrawingPart::Rect {
            x: left,
            y: top,
            w,
            h,
            filled: false,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2,
            y2: y1,
            dashed: true,
        },
        DrawingPart::Segment {
            x1: x2,
            y1: y2,
            x2: x1,
            y2,
            dashed: true,
        },
        DrawingPart::Text {
            x: x1.max(x2) + 6.0,
            y: top + 10.0,
            text: format!("{change:+.2}% {bars} bars"),
        },
    ]
}

/// How many bars of the series' own timeframe lie between two timestamps.
fn bar_count(geometry: Geometry<'_>, t1: f64, t2: f64) -> i64 {
    ((t2 - t1).abs() * 1_000_000.0 / geometry.frame.bar_nanos as f64).round() as i64
}

/// A horizontal band between two price levels, spanning the full plot width.
///
/// The price band is the zone itself: a filled rectangle from the left to right
/// plot edge, between the lower and upper prices.
#[must_use]
pub fn price_band(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let y0 = geometry.y(0);
    let y1 = geometry.y(1);
    let top = y0.min(y1);
    let h = (y1 - y0).abs();
    let price_lo = geometry.price(0).min(geometry.price(1));
    let price_hi = geometry.price(0).max(geometry.price(1));
    vec![
        DrawingPart::Rect {
            x: plot.x,
            y: top,
            w: plot.w,
            h,
            filled: true,
        },
        DrawingPart::Text {
            x: plot.x + 4.0,
            y: top - 4.0,
            text: format!("{price_hi:.2}"),
        },
        DrawingPart::Text {
            x: plot.x + 4.0,
            y: top + h + 12.0,
            text: format!("{price_lo:.2}"),
        },
    ]
}

/// A vertical band between two time points, spanning the full plot height.
#[must_use]
pub fn time_band(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let x0 = geometry.x(0);
    let x1 = geometry.x(1);
    let left = x0.min(x1);
    let w = (x1 - x0).abs();
    vec![DrawingPart::Rect {
        x: left,
        y: plot.y,
        w,
        h: plot.h,
        filled: true,
    }]
}

/// A forecast target: dashed crosshair at the target with a diagonal from origin.
///
/// Point 0 is the current position (the origin); point 1 is the projected target
/// in both time and price. The drawing connects them with a dashed line, then
/// marks the target with horizontal/vertical reference lines.
#[must_use]
pub fn forecast(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let plot = geometry.frame.plot;
    let (ox, oy) = (geometry.x(0), geometry.y(0));
    let (tx, ty) = (geometry.x(1), geometry.y(1));
    vec![
        // Diagonal from origin to target.
        DrawingPart::Segment {
            x1: ox,
            y1: oy,
            x2: tx,
            y2: ty,
            dashed: true,
        },
        // Horizontal line at target price (right to plot edge).
        DrawingPart::Segment {
            x1: tx,
            y1: ty,
            x2: plot.x + plot.w,
            y2: ty,
            dashed: true,
        },
        // Vertical line at target time (down to plot bottom).
        DrawingPart::Segment {
            x1: tx,
            y1: ty,
            x2: tx,
            y2: plot.y + plot.h,
            dashed: true,
        },
        // Price label at the target.
        DrawingPart::Text {
            x: tx + 6.0,
            y: ty - 6.0,
            text: format!("{:.2}", geometry.price(1)),
        },
    ]
}

/// Fixed-range volume profile: horizontal histogram between two time anchors.
///
/// Uses `analytics_core::volume_profile::calculate_volume_profile_from_candles`
/// on the bars between the two anchors, then renders each bucket as a horizontal
/// bar. Width is proportional to volume; the POC row is annotated.
#[must_use]
pub fn fixed_vp(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let bars = geometry.bars_between(geometry.points[0], geometry.points[1]);
    volume_histogram(geometry, bars)
}

/// Anchored volume profile: histogram from the anchor bar to the right edge.
#[must_use]
pub fn anchored_vp(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let anchor = geometry.points[0];
    let start = geometry
        .candles
        .partition_point(|c| super::ms(c) < anchor.0);
    let bars = &geometry.candles[start..];
    volume_histogram(geometry, bars)
}

/// Shared volume profile rendering: horizontal bars from a histogram.
fn volume_histogram(geometry: Geometry<'_>, bars: &[analytics_core::types::Candle]) -> Vec<DrawingPart> {
    if bars.is_empty() {
        return Vec::new();
    }
    let plot = geometry.frame.plot;
    // Choose a bucket size: 1/50th of the price range or a sensible default.
    let price_range = bars
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, c| {
            (acc.0.min(c.low), acc.1.max(c.high))
        });
    let span = price_range.1 - price_range.0;
    if span <= 0.0 {
        return Vec::new();
    }
    let bucket = span / 50.0;
    let profile = analytics_core::volume_profile::calculate_volume_profile_from_candles(bars, bucket);
    if profile.histogram.is_empty() {
        return Vec::new();
    }
    let max_vol = profile
        .histogram
        .iter()
        .map(|n| n.volume)
        .fold(0.0f64, f64::max);
    if max_vol <= 0.0 {
        return Vec::new();
    }
    // The histogram anchors at the left side of the profile's time region.
    let anchor_x = geometry.x(0);
    let max_w = plot.w * 0.3;
    let mut parts = Vec::with_capacity(profile.histogram.len() + 1);
    for node in &profile.histogram {
        let y = geometry.frame.y_at(node.price_level);
        let bar_h = (geometry.frame.y_at(node.price_level - bucket / 2.0)
            - geometry.frame.y_at(node.price_level + bucket / 2.0))
            .abs();
        let w = (node.volume / max_vol) * max_w;
        parts.push(DrawingPart::Rect {
            x: anchor_x,
            y: y - bar_h / 2.0,
            w,
            h: bar_h.max(1.0),
            filled: true,
        });
    }
    // POC label.
    let poc_y = geometry.frame.y_at(profile.poc);
    parts.push(DrawingPart::Segment {
        x1: anchor_x,
        y1: poc_y,
        x2: anchor_x + max_w,
        y2: poc_y,
        dashed: false,
    });
    parts.push(DrawingPart::Text {
        x: anchor_x + max_w + 4.0,
        y: poc_y - 4.0,
        text: format!("POC {:.2}", profile.poc),
    });
    parts
}

/// Anchored VWAP: a line computed bar-by-bar from the anchor to the right edge.
///
/// The running VWAP at each bar is `Σ(typical_price × volume) / Σ(volume)` from
/// the anchor forward. Each step becomes a segment, producing a smooth curve that
/// the trader uses as a dynamic support/resistance level.
#[must_use]
pub fn avwap(geometry: Geometry<'_>) -> Vec<DrawingPart> {
    let anchor_ms = geometry.points[0].0;
    let anchor_ns = (anchor_ms * 1_000_000.0) as i64;
    let start = geometry
        .candles
        .partition_point(|c| c.open_time < anchor_ns);
    let bars = &geometry.candles[start..];
    if bars.len() < 2 {
        return Vec::new();
    }
    let mut parts = Vec::with_capacity(bars.len());
    let mut cum_pv = 0.0f64;
    let mut cum_vol = 0.0f64;
    let mut prev: Option<(f64, f64)> = None;
    for candle in bars {
        let tp = (candle.high + candle.low + candle.close) / 3.0;
        cum_pv += tp * candle.volume;
        cum_vol += candle.volume;
        if cum_vol <= 0.0 {
            continue;
        }
        let vwap_price = cum_pv / cum_vol;
        let x = geometry.frame.x_at_ms(super::ms(candle));
        let y = geometry.frame.y_at(vwap_price);
        if let Some((px, py)) = prev {
            parts.push(DrawingPart::Segment {
                x1: px,
                y1: py,
                x2: x,
                y2: y,
                dashed: false,
            });
        }
        prev = Some((x, y));
    }
    parts
}
