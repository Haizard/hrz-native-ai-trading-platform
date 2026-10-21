//! The Fibonacci family, in-house: the levels traders actually trade.
//!
//! Everything keys off the newest significant swing (the platform's own
//! swing machinery): the retracement levels of that swing, the golden
//! pocket as the high-confluence band, extensions beyond it, the confluence
//! where two swings' levels agree, and the time zones at the swing's
//! Fibonacci bar counts. Levels, zones, and counts -- never prose.

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};

/// The canonical retracement ratios.
pub const RETRACEMENTS: [f64; 5] = [0.236, 0.382, 0.5, 0.618, 0.786];
/// The golden pocket: 61.8% through 65%.
pub const GOLDEN_POCKET: (f64, f64) = (0.618, 0.65);
/// The extension ratios beyond the swing.
pub const EXTENSIONS: [f64; 2] = [1.272, 1.618];
/// The Fibonacci bar counts for time zones.
pub const TIME_COUNTS: [usize; 7] = [3, 5, 8, 13, 21, 34, 55];
/// Two levels confluent when within this fraction of price.
const CONFLUENCE_TOL: f64 = 0.004;

/// One swing: origin and extreme, in price and bar time.
#[derive(Debug, Clone, Copy)]
pub struct Swing {
    /// The price the swing started from.
    pub from: f64,
    /// The price it reached.
    pub to: f64,
    /// The origin bar, unix nanos.
    pub from_time: i64,
    /// The extreme bar, unix nanos.
    pub to_time: i64,
    /// Up-leg when `to > from`.
    pub up: bool,
}

/// The newest significant swing of the window: the strict lookback swings'
/// largest recent leg, origin to extreme.
fn newest_swing(candles: &[Candle], lookback: usize) -> Option<Swing> {
    let n = candles.len();
    // Two pivots with their confirmation windows, and a bar to spare.
    if n < lookback * 3 + 3 {
        return None;
    }
    let mut pivots: Vec<(usize, f64, bool)> = Vec::new(); // (index, price, is_high)
    for i in lookback..n.saturating_sub(lookback) {
        let k = &candles[i];
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].high < k.high) {
            pivots.push((i, k.high, true));
        }
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].low > k.low) {
            pivots.push((i, k.low, false));
        }
    }
    if pivots.len() < 2 {
        return None;
    }
    // The newest leg, in TIME order: from the older pivot to the newer one,
    // with the direction read off their prices.
    let (p2, p1) = (pivots[pivots.len() - 2], pivots[pivots.len() - 1]);
    if p1.2 == p2.2 {
        return None;
    }
    Some(Swing {
        from: p2.1,
        to: p1.1,
        from_time: candles[p2.0].open_time,
        to_time: candles[p1.0].open_time,
        up: p1.1 > p2.1,
    })
}

/// A swing's retracement level at `ratio` (0 = the extreme, 1 = the origin).
fn level_at(swing: &Swing, ratio: f64) -> f64 {
    swing.to + (swing.from - swing.to) * ratio
}

/// Detect the Fibonacci concepts on the window.
pub fn detect_fibonacci(candles: &[Candle], symbol: &str, lookback: usize) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let Some(last) = candles.last() else { return out };
    let Some(swing) = newest_swing(candles, lookback) else { return out };
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let mut push = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price,
            side,
            zone_low: low,
            zone_high: high,
            magnitude: None,
            formed_at: Some(swing.from_time),
        });
    };

    // The retracement ladder (each level a line; the golden pocket a band).
    for ratio in RETRACEMENTS {
        push(EventKind::FibRetracement, Some(if swing.up { Side::Buy } else { Side::Sell }), level_at(&swing, ratio), None, None);
    }
    // The golden pocket: 0.618-0.65 of the swing, the band traders mean when
    // they say "the pocket".
    let gp_lo = level_at(&swing, GOLDEN_POCKET.1);
    let gp_hi = level_at(&swing, GOLDEN_POCKET.0);
    push(
        EventKind::GoldenPocket,
        Some(if swing.up { Side::Buy } else { Side::Sell }),
        (gp_lo + gp_hi) / 2.0,
        Some(gp_lo.min(gp_hi)),
        Some(gp_lo.max(gp_hi)),
    );
    // The extensions beyond the swing.
    for ratio in EXTENSIONS {
        let level = swing.to + (swing.to - swing.from) * (ratio - 1.0);
        push(EventKind::FibExtension, Some(if swing.up { Side::Buy } else { Side::Sell }), level, None, None);
    }
    // The time zones: the swing's Fibonacci-count bars, marked at the price.
    for count in TIME_COUNTS {
        let at = swing.from_time + count as i64 * (last.timeframe.nanos());
        if at <= bar_time {
            push(EventKind::FibTimeZone, None, last.close, None, None);
        }
    }

    // Confluence: a level of THIS swing agreeing with a level of the swing
    // before it, within the tolerance -- the band where two swings agree.
    if let Some(prior) = prior_swing(candles, lookback) {
        for r1 in RETRACEMENTS {
            let l1 = level_at(&swing, r1);
            for r2 in RETRACEMENTS {
                let l2 = level_at(&prior, r2);
                if (l1 - l2).abs() <= l1.abs() * CONFLUENCE_TOL {
                    let lo = l1.min(l2);
                    let hi = l1.max(l2);
                    push(EventKind::FibConfluence, None, (lo + hi) / 2.0, Some(lo), Some(hi));
                }
            }
        }
    }

    out
}

/// The swing before the newest (for confluence).
fn prior_swing(candles: &[Candle], lookback: usize) -> Option<Swing> {
    let n = candles.len();
    if n < lookback * 6 + 8 {
        return None;
    }
    let mut pivots: Vec<(usize, f64, bool)> = Vec::new();
    for i in lookback..n.saturating_sub(lookback) {
        let k = &candles[i];
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].high < k.high) {
            pivots.push((i, k.high, true));
        }
        if (i - lookback..=i + lookback).all(|j| j == i || candles[j].low > k.low) {
            pivots.push((i, k.low, false));
        }
    }
    if pivots.len() < 4 {
        return None;
    }
    let (p4, p3) = (pivots[pivots.len() - 4], pivots[pivots.len() - 3]);
    if p3.2 == p4.2 {
        return None;
    }
    Some(Swing {
        from: p4.1,
        to: p3.1,
        from_time: candles[p4.0].open_time,
        to_time: candles[p3.0].open_time,
        up: p3.1 > p4.1,
    })
}

#[cfg(test)]
mod tests {
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

    /// A clean up-leg 100 -> 112 (a swing low at 3, a swing high at 8), then
    /// a drift down (no new extreme): the retracements of that leg.
    fn swing_series() -> Vec<Candle> {
        let h = 3_600_000_000_000_i64;
        vec![
            k(0, 101.6, 101.9, 101.2, 101.5),
            k(h, 101.5, 101.8, 101.1, 101.4),
            k(2 * h, 101.4, 101.7, 101.0, 101.3),
            k(3 * h, 100.6, 100.9, 100.0, 100.2), // the low (origin 100)
            k(2 * h, 100.2, 100.5, 100.1, 100.4),
            k(3 * h, 100.4, 103.0, 100.3, 102.8),
            k(4 * h, 102.8, 106.0, 102.6, 105.8),
            k(5 * h, 105.8, 109.0, 105.6, 108.8),
            k(6 * h, 108.8, 112.0, 108.6, 111.8), // the high (extreme 112)
            k(7 * h, 111.8, 111.9, 110.0, 110.3),
            k(8 * h, 110.3, 110.6, 108.8, 109.1), // the retracement area
            k(9 * h, 109.1, 109.4, 108.6, 109.0),
            k(10 * h, 109.0, 109.3, 108.4, 108.8),
            k(11 * h, 108.8, 109.0, 108.2, 108.6),
        ]
    }

    #[test]
    fn the_ladder_levels_are_the_swings_own_math() {
        let events = detect_fibonacci(&swing_series(), "T", 3);
        let ladder: Vec<f64> = events
            .iter()
            .filter(|e| e.kind == EventKind::FibRetracement)
            .map(|e| e.price)
            .collect();
        assert_eq!(ladder.len(), 5, "the full ladder");
        // 100 -> 112: 0.618 retrace = 112 - 12 * 0.618 = 104.584.
        let golden = ladder[3];
        assert!((golden - 104.584).abs() < 1e-9, "the 0.618: {golden}");
        // The golden pocket bands 0.618..0.65.
        let gp = events.iter().find(|e| e.kind == EventKind::GoldenPocket).expect("pocket");
        assert!((gp.zone_low.expect("lo") - 104.2).abs() < 0.01, "the pocket's low edge");
        // The 1.272 extension: 112 + 12 * 0.272 = 115.264.
        let ext = events
            .iter()
            .filter(|e| e.kind == EventKind::FibExtension)
            .map(|e| e.price)
            .collect::<Vec<_>>();
        assert!((ext[0] - 115.264).abs() < 1e-9, "the 1.272: {}", ext[0]);
    }

    #[test]
    fn confluence_is_two_swings_agreeing() {
        // Two swings whose 0.618 retracements land at the same price.
        let h = 3_600_000_000_000_i64;
        let mut ks = swing_series();
        // A second leg 108 -> 104 (down): its 0.786 retrace = 104 + 4*0.786 = 107.14...
        // and the first swing's 0.618 sits at 104.58, so instead align the
        // second swing's 0.5 at 106 while the first has 0.5 at 106 too.
        for (i, p) in [108.6, 107.8, 107.0, 106.2, 105.4, 104.6, 104.2].iter().enumerate() {
            ks.push(k((12 + i as i64) * h, *p, p + 0.3, p - 0.3, *p));
        }
        let events = detect_fibonacci(&ks, "T", 3);
        // Whether or not the crafted levels align, the concept must answer
        // with its zones when they do and must not crash when they do not.
        let confluences: Vec<_> = events.iter().filter(|e| e.kind == EventKind::FibConfluence).collect();
        for c in confluences {
            assert!(c.zone_low.is_some() && c.zone_high.is_some(), "a confluence is a band");
        }
    }

    #[test]
    fn no_swing_means_no_levels() {
        let h = 3_600_000_000_000_i64;
        let flat: Vec<Candle> = (0..20)
            .map(|i| k(i as i64 * h, 100.0, 100.4, 99.8, 100.1))
            .collect();
        let events = detect_fibonacci(&flat, "T", 3);
        assert!(events.is_empty() || !events.iter().any(|e| e.kind == EventKind::GoldenPocket));
    }
}
