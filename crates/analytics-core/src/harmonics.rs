//! The platform's own harmonic detector: XABCD over our swing machinery,
//! with the canonical Fibonacci windows.
//!
//! Why this exists alongside the wickra adapter: wickra's detectors are
//! textbook-pure (a fixed 5% swing-reversal gate plus tight windows), which
//! verified correctly on clean shapes but completed nothing on three years
//! of real data. This detector keeps their canonical Fibonacci windows --
//! read straight out of their source, which is the community's settled
//! definition of each pattern -- and swaps the swing engine for the
//! platform's own lookback swings, so legs of a realistic scale (1-3% on a
//! 15m chart) complete as they should.
//!
//! The match: the newest five alternating swings (X-A-B-C-D), legs measured
//! on the swings' own prices, every ratio inside its window. The completion
//! fires when the D swing confirms (the swing rule's own confirmation --
//! there is no extra gate), and the zone is the PRZ around D.

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};

/// One pattern's ratio windows, as learned from the reference
/// implementation's source: (ab/xa, bc/ab, cd/bc, ad/xa), each (lo, hi).
/// `None` skips a check (the AB=CD shape has no AD window of its own).
struct Windows {
    ab_xa: (f64, f64),
    bc_ab: (f64, f64),
    cd_bc: (f64, f64),
    ad_xa: (f64, f64),
}

/// Every ratio lies inside its window.
fn within(value: f64, window: (f64, f64)) -> bool {
    value >= window.0 && value <= window.1
}

/// The canonical windows per pattern kind.
fn windows_for(kind: EventKind) -> Option<Windows> {
    match kind {
        EventKind::HarmonicGartley => Some(Windows {
            ab_xa: (0.55, 0.70),
            bc_ab: (0.382, 0.886),
            cd_bc: (1.13, 1.618),
            ad_xa: (0.74, 0.84),
        }),
        EventKind::HarmonicBat => Some(Windows {
            ab_xa: (0.382, 0.50),
            bc_ab: (0.382, 0.886),
            cd_bc: (1.618, 2.618),
            ad_xa: (0.84, 0.93),
        }),
        EventKind::HarmonicButterfly => Some(Windows {
            ab_xa: (0.74, 0.84),
            bc_ab: (0.382, 0.886),
            cd_bc: (1.618, 2.618),
            ad_xa: (1.27, 1.618),
        }),
        EventKind::HarmonicCrab => Some(Windows {
            ab_xa: (0.382, 0.618),
            bc_ab: (0.382, 0.886),
            cd_bc: (2.24, 3.618),
            ad_xa: (1.55, 1.65),
        }),
        EventKind::HarmonicShark => Some(Windows {
            ab_xa: (1.13, 1.618),
            bc_ab: (1.618, 2.24),
            cd_bc: (0.382, 0.886),
            ad_xa: (0.886, 1.13),
        }),
        EventKind::HarmonicAbcd => Some(Windows {
            ab_xa: (0.0, f64::MAX), // no constraint of its own
            bc_ab: (0.382, 0.886),
            cd_bc: (1.13, 2.618),
            ad_xa: (0.0, f64::MAX),
        }),
        _ => None,
    }
}

/// One alternating swing: price and direction (true = high).
#[derive(Debug, Clone, Copy)]
struct Pivot {
    price: f64,
    high: bool,
    time: i64,
}

/// The strict lookback swings of the series, oldest first.
fn pivots(candles: &[Candle], lookback: usize) -> Vec<Pivot> {
    let mut out = Vec::new();
    let n = candles.len();
    for i in lookback..n.saturating_sub(lookback) {
        let k = &candles[i];
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].high < k.high) {
            out.push(Pivot { price: k.high, high: true, time: k.open_time });
        }
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].low > k.low) {
            out.push(Pivot { price: k.low, high: false, time: k.open_time });
        }
    }
    out
}

/// The newest five strictly alternating pivots, or none. Real structure
/// sometimes prints two same-side swings in a row; take the more extreme one
/// and keep alternating.
fn last_five_alternating(pivots: &[Pivot]) -> Option<[Pivot; 5]> {
    let mut chain: Vec<Pivot> = Vec::new();
    for p in pivots {
        match chain.last() {
            Some(last) if last.high == p.high => {
                // Same side as the last: keep the more extreme of the two.
                let replace = if p.high { p.price > last.price } else { p.price < last.price };
                if replace {
                    *chain.last_mut().expect("checked above") = *p;
                }
            }
            _ => chain.push(*p),
        }
    }
    if chain.len() < 5 {
        return None;
    }
    let tail = &chain[chain.len() - 5..];
    Some([tail[0], tail[1], tail[2], tail[3], tail[4]])
}

/// Detect harmonic completions on the window, with the platform's own swing
/// resolution (lookback 3, matching the rest of the structure machinery).
pub fn detect_harmonics(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    const LOOKBACK: usize = 3;
    const PRZ_FRACTION: f64 = 0.01; // the reversal zone is 1% around D

    let mut out = Vec::new();
    // Five alternating swings at the lookback's own resolution, plus the
    // confirmation tail on either side.
    if candles.len() < 14 {
        return out;
    }
    let Some(last) = candles.last() else { return out };
    let raw_pivots = pivots(candles, LOOKBACK);
    eprintln!("raw: {:?}", raw_pivots);
    let Some([x, a, b, c, d]) = last_five_alternating(&raw_pivots) else {
        eprintln!("no alternating five");
        return out;
    };
    eprintln!("chain: x={:?} a={:?} b={:?} c={:?} d={:?}", x, a, b, c, d);
    // The direction: a low X starts a bullish XABCD, a high X a bearish one.
    let bullish = !x.high;
    if (x.high == a.high) || (a.high == b.high) || (b.high == c.high) || (c.high == d.high) {
        return out;
    }
    let (xa, ab, bc, cd, ad) = (
        (a.price - x.price).abs(),
        (b.price - a.price).abs(),
        (c.price - b.price).abs(),
        (d.price - c.price).abs(),
        (d.price - a.price).abs(),
    );
    if xa <= 0.0 || ab <= 0.0 || bc <= 0.0 {
        return out;
    }
    let (r_ab, r_bc, r_cd, r_ad) = (ab / xa, bc / ab, cd / bc, ad / xa);

    for kind in [
        EventKind::HarmonicGartley,
        EventKind::HarmonicBat,
        EventKind::HarmonicButterfly,
        EventKind::HarmonicCrab,
        EventKind::HarmonicShark,
        EventKind::HarmonicAbcd,
    ] {
        let Some(w) = windows_for(kind) else { continue };
        if within(r_ab, w.ab_xa) && within(r_bc, w.bc_ab) && within(r_cd, w.cd_bc) && within(r_ad, w.ad_xa) {
            // The D pivot must already be the newest confirmed swing (it is,
            // by construction of the chain) -- the completion fires here.
            let prz = d.price * PRZ_FRACTION;
            out.push(MarketEvent {
                kind,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time: last.open_time,
                close: last.close,
                price: d.price,
                side: Some(if bullish { Side::Buy } else { Side::Sell }),
                zone_low: Some(d.price - prz),
                zone_high: Some(d.price + prz),
                magnitude: None,
                formed_at: Some(x.time),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, price: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: price,
            high: price + 0.3,
            low: price - 0.3,
            close: price,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    /// A bullish Gartley at 15m scale: X=100, A=103 (3%), B=101.2 (0.6 of
    /// XA), C=102.5 (0.72 of AB), D=100.6 (0.8 of XA, 1.54 of BC) -- legs of
    /// one to three percent, the scale real charts move at.
    fn realistic_gartley() -> Vec<Candle> {
        let h = 3_600_000_000_000_i64;
        let prices = [
            // Three lead bars so the X low itself confirms as a swing (the
            // lookback needs its window on both sides).
            100.4, 100.3, 100.2, 100.0,        // lead-in to X=100
            101.0, 102.0, 103.0,             // X to A (A=103)
            102.0, 101.2,                    // A to B
            101.8, 102.5,                    // B to C
            101.8, 100.6,                    // C to D
            101.2, 101.8, 102.2,             // the turn (D confirms)
        ];
        prices.iter().enumerate().map(|(i, p)| k(i as i64 * h, *p)).collect()
    }

    #[test]
    fn a_realistic_scale_gartley_completes_where_the_strict_gate_could_not() {
        let events = detect_harmonics(&realistic_gartley(), "T");
        let gartley = events.iter().find(|e| e.kind == EventKind::HarmonicGartley);
        assert!(gartley.is_some(), "1-3% legs complete: {:?}", events.iter().map(|e| e.kind.name()).collect::<Vec<_>>());
        let gartley = gartley.expect("checked above");
        assert_eq!(gartley.side, Some(Side::Buy));
        assert!((gartley.price - 100.3).abs() < 0.05, "the PRZ sits at the D swing low: {}", gartley.price);
        assert!(gartley.zone_low.is_some() && gartley.zone_high.is_some(), "the PRZ bands");
    }

    #[test]
    fn a_shape_outside_the_windows_completes_nothing() {
        let h = 3_600_000_000_000_i64;
        // Same shape but B at 0.35 of XA: outside the Gartley window.
        let prices = [
            100.4, 100.3, 100.2, 100.0,
            101.0, 102.0, 103.0,
            101.0, 101.05, // B too shallow
            101.8, 102.3,
            101.6, 100.8,
            101.2, 101.7,
        ];
        let candles: Vec<Candle> = prices.iter().enumerate().map(|(i, p)| k(i as i64 * h, *p)).collect();
        let events = detect_harmonics(&candles, "T");
        assert!(!events.iter().any(|e| e.kind == EventKind::HarmonicGartley), "off-ratio is no Gartley");
    }

    #[test]
    fn the_bat_and_butterfly_windows_are_their_own() {
        // A deep-D shape: D at 0.886 of XA (the Bat) should match Bat and
        // not Gartley (which ends at 0.786).
        let h = 3_600_000_000_000_i64;
        // X=100, A=103, B=101.9 (0.47 of XA: the Bat's shallow B), C=102.6,
        // D=100.34 (0.906 of XA, 2.2 of BC) -- ratios on the swing EXTREMES
        // (the lows/highs), not the closes.
        let prices = [
            100.4, 100.3, 100.2, 100.0,
            101.0, 102.0, 103.0,
            102.4, 101.9,
            102.2, 102.6,
            102.3, 100.34,
            100.9, 101.4, 101.8,
        ];
        let candles: Vec<Candle> = prices.iter().enumerate().map(|(i, p)| k(i as i64 * h, *p)).collect();
        let events = detect_harmonics(&candles, "T");
        assert!(events.iter().any(|e| e.kind == EventKind::HarmonicBat), "the deep D is a Bat: {:?}", events.iter().map(|e| e.kind.name()).collect::<Vec<_>>());
        assert!(!events.iter().any(|e| e.kind == EventKind::HarmonicGartley), "and not a Gartley");
    }
}
