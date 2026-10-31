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

use crate::events::{EventGeometry, EventKind, MarketEvent};
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

/// The Wolfe Wave: five alternating pivots with the channel rules and the
/// 1-4 target line. Bullish: lows 1-3-5 descend (each a new low), highs 2-4
/// descend too (the channel narrows), and the target is the line through
/// points 1 and 4, extended. Bearish mirrors it.
fn detect_wolfe(candles: &[Candle], symbol: &str, pivots: &[Pivot]) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let Some(last) = candles.last() else { return out };
    let Some(chain5) = last_five_alternating(pivots) else { return out };
    let [p1, p2, p3, p4, p5] = chain5;
    let line_at = |a: &Pivot, b: &Pivot, i: usize| -> f64 {
        let ta = candles.iter().position(|c| c.open_time == a.time).unwrap_or(0) as f64;
        let tb = candles.iter().position(|c| c.open_time == b.time).unwrap_or(1) as f64;
        a.price + (b.price - a.price) * (i as f64 - ta) / (tb - ta).max(1.0)
    };
    let n = candles.len() - 1;
    let bullish = !p1.high && p2.high && !p3.high && p4.high && !p5.high
        && p3.price < p1.price
        && p4.price < p2.price
        && p5.price < p3.price
        && p4.price > p5.price;
    let bearish = p1.high && !p2.high && p3.high && !p4.high && p5.high
        && p3.price > p1.price
        && p4.price > p2.price
        && p5.price > p3.price
        && p4.price < p5.price;
    if bullish {
        let target = line_at(&p1, &p4, n + 10);
        out.push(MarketEvent {
            kind: EventKind::WolfeWave,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: p5.price,
            side: Some(crate::types::Side::Buy),
            zone_low: Some(p5.price),
            zone_high: Some(target),
            magnitude: Some(target - p5.price),
            geometry: Some(EventGeometry::path(vec![
                (p1.time, p1.price),
                (p2.time, p2.price),
                (p3.time, p3.price),
                (p4.time, p4.price),
                (p5.time, p5.price),
            ])),
            formed_at: Some(p1.time),
        });
    } else if bearish {
        let target = line_at(&p1, &p4, n + 10);
        out.push(MarketEvent {
            kind: EventKind::WolfeWave,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: p5.price,
            side: Some(crate::types::Side::Sell),
            zone_low: Some(target),
            zone_high: Some(p5.price),
            magnitude: Some(p5.price - target),
            geometry: Some(EventGeometry::path(vec![
                (p1.time, p1.price),
                (p2.time, p2.price),
                (p3.time, p3.price),
                (p4.time, p4.price),
                (p5.time, p5.price),
            ])),
            formed_at: Some(p1.time),
        });
    }
    out
}

/// Detect harmonic completions on the window, with the platform's own swing
/// resolution (lookback 3, matching the rest of the structure machinery).
pub fn detect_wolfe_waves(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    detect_wolfe(candles, symbol, &pivots(candles, 3))
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
    let Some([x, a, b, c, d]) = last_five_alternating(&raw_pivots) else {
        return out;
    };
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
                geometry: Some(EventGeometry::path(vec![
                    (x.time, x.price),
                    (a.time, a.price),
                    (b.time, b.price),
                    (c.time, c.price),
                    (d.time, d.price),
                ])),
                formed_at: Some(x.time),
            });
        }
    }
    out
}

#[cfg(test)]
mod wolfe_tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, o: f64, h: f64, l: f64, c: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    #[test]
    fn a_bullish_wolfe_completes_with_the_1_4_target() {
        // The five pivots: L 100, H 106, L 98, H 104, L 96 -- descending
        // lows and descending highs, the channel narrowing.
        let mut candles: Vec<Candle> = (0..20).map(|i| k(i as i64 * 3_600_000_000_000, 102.0, 102.5, 101.5, 102.0)).collect();
        // Same-type pivots need four or more bars between them (the
        // lookback-3 rule respects three on each side): L20 H22 L26 H29 L32.
        let path = [
            (102.0, 102.2, 100.0, 100.4), // 20: p1 low 100
            (100.4, 106.0, 100.2, 105.6), // 21
            (105.6, 105.5, 104.0, 104.4), // 22: p2 high 106
            (104.4, 104.5, 101.0, 101.4), // 23
            (101.4, 101.6, 99.5, 99.9),   // 24
            (99.9, 100.1, 98.0, 98.6),    // 25
            (98.6, 98.8, 97.5, 97.9),     // 26: p3 low 98
            (97.9, 100.0, 97.7, 99.8),    // 27
            (99.8, 104.0, 99.6, 103.6),   // 28
            (103.6, 103.5, 102.0, 102.4), // 29: p4 high 104
            (102.4, 102.5, 100.5, 100.9), // 30
            (100.9, 101.0, 98.5, 98.9),   // 31
            (98.9, 99.0, 96.0, 96.6),     // 32: p5 low 96
            (96.6, 98.0, 96.2, 97.8),     // 33: the turn begins
            (97.8, 99.0, 97.6, 98.8),     // 34
            (98.8, 99.4, 98.4, 99.2),     // 35: p5 confirms with three right
            (99.2, 99.6, 98.8, 99.4),     // 36
        ];
        for (i, (o, h, l, c)) in path.iter().enumerate() {
            candles.push(k((20 + i) as i64 * 3_600_000_000_000, *o, *h, *l, *c));
        }
        let events = detect_wolfe_waves(&candles, "T");
        let w = events.iter().find(|e| e.kind == EventKind::WolfeWave);
        assert!(w.is_some(), "the five-point wave completes");
        let w = w.expect("checked");
        assert_eq!(w.side, Some(crate::types::Side::Buy));
        assert!((w.price - 96.0).abs() < 0.01, "the entry rides wave 5's low");
        let target = w.zone_high.expect("the 1-4 line");
        assert!(target > 96.0, "the target is above the entry");
    }
}

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
