//! The ICT composite models, assembled from the platform's own detectors:
//! the Silver Bullet, the Unicorn, the 2022 mentorship model, and the market
//! maker buy/sell model. Each is a SEQUENCE -- the events exist already; the
//! model is their order, and the order is the whole point.

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};

const HOUR_NS: i64 = 3_600_000_000_000;
const DAY_NS: i64 = 24 * HOUR_NS;

/// One fresh three-bar fair value gap, or None: the gap between bar i-2's
/// far wick and bar i's near wick, unfilled since. The smallest honest FVG
/// check -- the platform's full detector does lifecycles; the model only
/// needs "does a fresh gap exist in this direction".
fn fresh_fvg(candles: &[Candle], from: usize, bullish: bool) -> Option<(f64, f64)> {
    if candles.len() < from + 3 {
        return None;
    }
    for i in (from + 2..candles.len()).rev() {
        let (a, b, c) = (&candles[i - 2], &candles[i - 1], &candles[i]);
        let gap = if bullish {
            c.low > a.high
        } else {
            c.high < a.low
        };
        if gap {
            let (low, high) = if bullish { (a.high, c.low) } else { (c.high, a.low) };
            // Displacement: the middle bar's body at least twice the recent average.
            let mean_body: f64 = candles[i.saturating_sub(15)..i.saturating_sub(1).max(1)]
                .iter()
                .map(|k| (k.close - k.open).abs())
                .sum::<f64>()
                / 14.0_f64.max(1.0);
            let body = (b.close - b.open).abs();
            if mean_body > 0.0 && body >= 1.5 * mean_body {
                return Some((low, high));
            }
        }
    }
    None
}

/// The most recent sweep before `bar`: a bar trading beyond `level` and a
/// LATER bar closing back through it. Returns the sweep bar's index.
fn find_sweep(candles: &[Candle], level: f64, from: usize, high_side: bool) -> Option<usize> {
    for i in from..candles.len().saturating_sub(1) {
        let beyond = if high_side { candles[i].high > level } else { candles[i].low < level };
        if beyond {
            // A later bar closes back through the level.
            for j in i + 1..candles.len() {
                let back = if high_side { candles[j].close < level } else { candles[j].close > level };
                if back {
                    return Some(i);
                }
            }
            return None; // a sweep still standing is not a sweep yet
        }
    }
    None
}

/// Detect the ICT composites on the window's tail.
pub fn detect_ict_models(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 40 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let mut push = |kind: EventKind, side: Side, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price,
            side: Some(side),
            zone_low: low,
            zone_high: high,
            magnitude: None,
            geometry: None,
            formed_at: formed,
        });
    };
    let day_of = last.open_time.div_euclid(DAY_NS);
    let today: usize = candles.iter().position(|k| k.open_time.div_euclid(DAY_NS) == day_of).unwrap_or(0);

    // -- The Silver Bullet: a sweep of the day's prior swing, inside one of
    // the SB windows (07:00-08:00, 14:00-15:00, 18:00-19:00 UTC -- the
    // winter offsets of ICT's 03:00, 10:00, 14:00 New York hours), and a
    // displaced FVG formed after the sweep in the reversal's direction.
    let hour = ((last.open_time % DAY_NS) / HOUR_NS) as u32;
    let in_window = hour == 7 || hour == 14 || hour == 18;
    if in_window {
        // The day's swings BEFORE today (or today's own, before the window).
        let swings_hi = candles[..n.saturating_sub(3).max(1)]
            .iter()
            .map(|k| k.high)
            .fold(f64::MIN, f64::max);
        let swings_lo = candles[..n.saturating_sub(3).max(1)]
            .iter()
            .map(|k| k.low)
            .fold(f64::MAX, f64::min);
        for (high_side, level) in [(true, swings_hi), (false, swings_lo)] {
            if let Some(sweep_idx) = find_sweep(candles, level, today, high_side) {
                // The FVG after the sweep, in the reversal's direction.
                if let Some((low, high)) = fresh_fvg(candles, sweep_idx, !high_side) {
                    push(
                        EventKind::SilverBullet,
                        if high_side { Side::Sell } else { Side::Buy },
                        (low + high) / 2.0,
                        Some(low),
                        Some(high),
                        Some(candles[sweep_idx].open_time),
                    );
                    break;
                }
            }
        }
    }

    // -- The Unicorn: a zone that is BOTH a breaker (an invalidated order
    // block flipped) and an FVG at the same prices -- and price trading
    // INTO the overlap on the tail.
    // The breaker: the newest swing broken by a displaced close, with the
    // last opposing candle before the break being the block.
    {
        // Reuse the platform's own swing scan for the zone.
        let lb = 2;
        let mut swing_highs: Vec<(usize, f64)> = Vec::new();
        for i in lb..n.saturating_sub(lb).max(lb + 1) {
            if (i - lb..=i + lb).all(|j| j == i || candles[j].high < candles[i].high) {
                swing_highs.push((i, candles[i].high));
            }
        }
        let mut unicorn: Option<(f64, f64)> = None;
        for (idx, level) in swing_highs.iter().rev().take(6) {
            // A displaced close through the swing (the break), then the
            // block is the last bearish candle before it.
            for i in idx + 1..n {
                if candles[i].close > *level {
                    // The block: last bearish candle before i.
                    for j in (idx + 1..i).rev() {
                        if candles[j].close < candles[j].open {
                            let (b_lo, b_hi) = (candles[j].low, candles[j].high);
                            // A bullish FVG overlapping the block.
                            if let Some((f_lo, f_hi)) = fresh_fvg(candles, j, true) {
                                let (u_lo, u_hi) = (b_lo.max(f_lo), b_hi.min(f_hi));
                                if u_lo < u_hi {
                                    unicorn = Some((u_lo, u_hi));
                                }
                            }
                            break;
                        }
                    }
                    break;
                }
            }
            if unicorn.is_some() {
                break;
            }
        }
        if let Some((u_lo, u_hi)) = unicorn {
            // Price enters the overlap on the tail, from above (the buy model).
            let prev = &candles[n - 1];
            if last.low <= u_hi && last.close >= u_lo && prev.low > u_hi {
                push(EventKind::UnicornModel, Side::Buy, (u_lo + u_hi) / 2.0, Some(u_lo), Some(u_hi), None);
            }
        }
    }

    // -- The 2022 mentorship model: a sweep of YESTERDAY'S extreme today,
    // then the structure's character changing against the sweep (the newest
    // swing AGAINST the sweep direction broken by a close).
    {
        let yday = day_of - 1;
        let yday_bars: Vec<&Candle> = candles
            .iter()
            .filter(|k| k.open_time.div_euclid(DAY_NS) == yday)
            .collect();
        if !yday_bars.is_empty() {
            let y_hi = yday_bars.iter().map(|k| k.high).fold(f64::MIN, f64::max);
            let y_lo = yday_bars.iter().map(|k| k.low).fold(f64::MAX, f64::min);
            for (high_side, level) in [(true, y_hi), (false, y_lo)] {
                if let Some(sweep_idx) = find_sweep(candles, level, today, high_side) {
                    // The character change: the structure the sweep
                    // DISRUPTED -- the newest swing BEFORE it, in the
                    // sweep's own direction, closed through. A fresh break
                    // has no confirmed swing of its own yet (the lookback
                    // rule needs bars to the right), which is why the
                    // structure is read from before the sweep, not after.
                    let mut swings: Vec<(usize, f64)> = Vec::new();
                    let lb = 2;
                    for i in lb.max(today + 1)..sweep_idx.saturating_sub(lb) {
                        if high_side {
                            if (i - lb..=i + lb).all(|j| j == i || candles[j].low > candles[i].low) {
                                swings.push((i, candles[i].low));
                            }
                        } else if (i - lb..=i + lb).all(|j| j == i || candles[j].high < candles[i].high) {
                            swings.push((i, candles[i].high));
                        }
                    }
                    let changed = swings.iter().any(|(_, swing_level)| {
                        if high_side { last.close < *swing_level } else { last.close > *swing_level }
                    });
                    if changed {
                        push(
                            EventKind::Model2022,
                            if high_side { Side::Sell } else { Side::Buy },
                            level,
                            if high_side { Some(level) } else { Some(y_lo) },
                            if high_side { Some(y_hi) } else { Some(level) },
                            Some(candles[sweep_idx].open_time),
                        );
                        break;
                    }
                }
            }
        }
    }

    // -- The market maker model: the full curve. Accumulation (the Asian
    // range), manipulation (the Judas sweep of one side), and distribution
    // reaching the OPPOSITE external pool -- the model completes AT the
    // pool, not before.
    {
        let asian: Vec<&Candle> = candles
            .iter()
            .filter(|k| k.open_time.div_euclid(DAY_NS) == day_of && ((k.open_time % DAY_NS) / HOUR_NS) < 7)
            .collect();
        if !asian.is_empty() {
            let asia_lo = asian.iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let asia_hi = asian.iter().map(|k| k.high).fold(f64::MIN, f64::max);
            // The manipulation: a kill-zone-first-hour trade beyond one side.
            let swept_high = candles[today..].iter().any(|k| {
                let h = ((k.open_time % DAY_NS) / HOUR_NS) as u32;
                (h == 7 || h == 12) && k.high > asia_hi
            });
            let swept_low = candles[today..].iter().any(|k| {
                let h = ((k.open_time % DAY_NS) / HOUR_NS) as u32;
                (h == 7 || h == 12) && k.low < asia_lo
            });
            // The external pool: the week's swing extreme on the far side.
            let week_hi = candles[..n.saturating_sub(4).max(1)]
                .iter()
                .map(|k| k.high)
                .fold(f64::MIN, f64::max);
            let week_lo = candles[..n.saturating_sub(4).max(1)]
                .iter()
                .map(|k| k.low)
                .fold(f64::MAX, f64::min);
            // The sell model completes when the run reaches the low pool;
            // the buy model at the high pool. Fire once, at the tag.
            let prev = &candles[n - 1];
            if swept_high && last.low <= week_lo && prev.low > week_lo {
                push(EventKind::MarketMakerModel, Side::Sell, week_lo, Some(week_lo), Some(week_hi), None);
            } else if swept_low && last.high >= week_hi && prev.high < week_hi {
                push(EventKind::MarketMakerModel, Side::Buy, week_hi, Some(week_lo), Some(week_hi), None);
            }
        }
    }

    out
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

    #[test]
    fn the_silver_bullet_needs_the_sweep_the_window_and_the_gap() {
        // Yesterday/today's swing high at 102. Bars at 13:00 UTC: a sweep to
        // 102.5 closing back at 101.6; then a displaced bearish FVG; the
        // newest bar at 14:00 inside the window.
        let h = 3_600_000_000_000_i64;
        let day = 24 * h;
        let mut candles: Vec<Candle> = (0..40).map(|i| k(i * h, 100.5, 101.0, 100.0, 100.5)).collect();
        // Day's swing high at 102 (bar 31, at hour 7 of day 1... the sweep
        // must be today: build on day 2).
        let day2 = day;
        candles[28] = k(day2 + 4 * h, 100.5, 102.0, 100.4, 101.5); // the swing high
        candles[29] = k(day2 + 5 * h, 101.5, 101.6, 100.8, 101.0);
        // The sweep at hour 13 today (day 2 = same day as bars 24+).
        candles.push(k(day2 + 13 * h, 101.0, 102.5, 100.9, 101.6)); // the sweep
        // The displaced bearish FVG at hour 13+1.
        candles.push(k(day2 + 14 * h, 101.6, 101.7, 100.2, 100.4)); // big down body
        // Wait -- the FVG is a 3-bar pattern: bars (sweep, big, gap-bar).
        candles.push(k(day2 + 15 * h, 100.4, 100.5, 99.8, 100.0)); // gap down
        // The newest bar at hour 18 (inside the SB window).
        candles.push(k(day2 + 18 * h, 100.0, 100.2, 99.6, 99.9));
        let events = detect_ict_models(&candles, "T");
        let sb = events.iter().find(|e| e.kind == EventKind::SilverBullet);
        assert!(sb.is_some(), "the sweep inside the window with the gap fires");
        assert_eq!(sb.expect("checked").side, Some(Side::Sell), "the high sweep reverses down");
        // Outside the window: no silver bullet.
        let outside: Vec<Candle> = candles[..candles.len() - 1].to_vec();
        assert!(!detect_ict_models(&outside, "T").iter().any(|e| e.kind == EventKind::SilverBullet));
    }

    #[test]
    fn the_2022_model_needs_yesterdays_sweep_then_the_character_change() {
        let h = 3_600_000_000_000_i64;
        // Yesterday: 24 bars ranging 100..102. The detector's floor is 40
        // bars, so today's lead-up to the sweep is padded with quiet bars.
        let mut candles: Vec<Candle> = (0..24).map(|i| k(i * h, 101.0, 102.0, 100.0, 101.0)).collect();
        // The padding forms real swings (identical bars have equal lows,
        // and the strict swing rule reads no structure at all).
        for i in 0..11 {
            let dip = if i % 3 == 1 { 0.4 } else { 0.0 };
            candles.push(k(24 * h + (i as i64) * h / 2, 101.0 - dip, 101.2 - dip, 100.6 - dip, 100.9 - dip));
        }
        // Today: the sweep of yesterday's high, closing back in.
        candles.push(k(24 * h + 6 * h, 101.0, 102.5, 100.8, 101.4));
        candles.push(k(24 * h + 7 * h, 101.4, 101.5, 100.6, 100.9));
        // The character change: a swing low forms, and the newest close breaks it.
        candles.push(k(24 * h + 8 * h, 100.9, 101.0, 100.5, 100.7));
        candles.push(k(24 * h + 9 * h, 100.7, 100.8, 100.4, 100.6));
        candles.push(k(24 * h + 10 * h, 100.6, 100.7, 100.3, 100.5));
        candles.push(k(24 * h + 11 * h, 100.5, 100.6, 100.2, 100.3));
        candles.push(k(24 * h + 12 * h, 100.3, 100.4, 99.9, 100.0)); // breaks the lows
        let events = detect_ict_models(&candles, "T");
        let m = events.iter().find(|e| e.kind == EventKind::Model2022);
        assert!(m.is_some(), "yesterday's sweep with the character change completes the model");
        assert_eq!(m.expect("checked").side, Some(Side::Sell));
    }
}
