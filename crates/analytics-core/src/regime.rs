//! The regime layer: trend/range classification, multi-timeframe alignment,
//! and the daily/weekly bias. One regime event per SHIFT -- the classifier's
//! output is a state, and the state changing is the tradeable fact.

use crate::events::{EventKind, MarketEvent};
use crate::indicators::trend::adx;
use crate::market_structure::{find_swings, SwingKind};
use crate::resample::resample;
use crate::types::{Candle, Side, Timeframe};

/// The classifier's states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// ADX over 25, +DI dominant, structure rising.
    TrendUp,
    /// ADX over 25, -DI dominant, structure falling.
    TrendDown,
    /// No trend the ADX respects, or a structure that disagrees with it.
    Range,
}

/// The structure's lean: the newest two swing highs and lows both rising
/// (up), both falling (down), or neither.
pub fn structure_lean(candles: &[Candle]) -> Option<Side> {
    structure_lean_at(candles, 3)
}

/// The lean at a chosen swing scale: coarser timeframes read coarser
/// swings (a 4h resample has a third as many bars, and a lookback that
/// means "nearby" on the 1h means "far" on it).
pub fn structure_lean_at(candles: &[Candle], lookback: usize) -> Option<Side> {
    let swings = find_swings(candles, lookback);
    let highs: Vec<f64> = swings.iter().filter(|s| s.kind == SwingKind::High).map(|s| s.price).collect();
    let lows: Vec<f64> = swings.iter().filter(|s| s.kind == SwingKind::Low).map(|s| s.price).collect();
    if highs.len() >= 2 && lows.len() >= 2 {
        let (h1, h2) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        let (l1, l2) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        if h2 > h1 && l2 > l1 {
            return Some(Side::Buy);
        }
        if h2 < h1 && l2 < l1 {
            return Some(Side::Sell);
        }
    }
    None
}

/// The regime at the window's tail: the ADX says whether a trend exists,
/// the DI says who owns it, and the structure must AGREE (a trend the
/// swings deny is a range about to turn).
pub fn classify(candles: &[Candle]) -> Regime {
    if candles.len() < 32 {
        return Regime::Range;
    }
    let (plus, minus, adx_line) = adx(candles, 14);
    let n = candles.len() - 1;
    let (Some(a), Some(p), Some(m)) = (adx_line[n], plus[n], minus[n]) else {
        return Regime::Range;
    };
    if a <= 25.0 {
        return Regime::Range;
    }
    match (structure_lean(candles), p >= m) {
        (Some(Side::Buy), true) => Regime::TrendUp,
        (Some(Side::Sell), false) => Regime::TrendDown,
        _ => Regime::Range,
    }
}

/// Fire on the SHIFT: the tail's regime differs from the previous bar's.
pub fn detect_regime(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 34 {
        return out;
    }
    let now = classify(candles);
    let was = classify(&candles[..candles.len() - 1]);
    if now == was {
        return out;
    }
    let last = candles.last().expect("checked above");
    let (side, magnitude) = match now {
        Regime::TrendUp => (Some(Side::Buy), None),
        Regime::TrendDown => (Some(Side::Sell), None),
        Regime::Range => (None, None),
    };
    let (_, _, adx_line) = adx(candles, 14);
    out.push(MarketEvent {
        kind: EventKind::RegimeShift,
        symbol: symbol.to_string(),
        timeframe: last.timeframe,
        bar_time: last.open_time,
        close: last.close,
        price: last.close,
        side,
        zone_low: Some(last.low),
        zone_high: Some(last.high),
        magnitude: magnitude.or(adx_line[candles.len() - 1]),
        formed_at: None,
    });
    out
}

/// Multi-timeframe alignment: the structure's lean on the chart's own bars
/// versus the lean on the same bars resampled 4x up (1h -> 4h, 15m -> 1h).
/// Fires when the two START agreeing -- a disagreement is the default state,
/// not an event.
pub fn detect_mtf_alignment(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 60 {
        return out;
    }
    let tf = candles[0].timeframe;
    let htf = match tf {
        Timeframe::M1 => Timeframe::M5,
        Timeframe::M5 => Timeframe::M15,
        Timeframe::M15 => Timeframe::H1,
        Timeframe::H1 => Timeframe::H4,
        Timeframe::H4 => Timeframe::D1,
        Timeframe::D1 => Timeframe::W1,
        _ => return out,
    };
    let upper = resample(candles, htf);
    if upper.len() < 12 {
        return out;
    }
    let ltf_lean = structure_lean(candles);
    let htf_lean = structure_lean_at(&upper, 2);
    let (Some(l), Some(h)) = (ltf_lean, htf_lean) else {
        return out;
    };
    if l != h {
        return out;
    }
    // The event is the agreement FORMING, whichever side turned last: the
    // LTF usually turns first and the HTF catches up a bar later, so
    // "the LTF just changed" misses the moment the HTF arrives. Fire when
    // both agree and the previous bar did NOT have both agreeing.
    let was_l = structure_lean(&candles[..candles.len() - 1]);
    let was_h = structure_lean_at(&upper[..upper.len().saturating_sub(1).max(1)], 2);
    if was_l == Some(l) && was_h == Some(h) {
        return out;
    }
    let last = candles.last().expect("checked above");
    out.push(MarketEvent {
        kind: EventKind::MtfAlignment,
        symbol: symbol.to_string(),
        timeframe: last.timeframe,
        bar_time: last.open_time,
        close: last.close,
        price: last.close,
        side: Some(l),
        zone_low: Some(last.low),
        zone_high: Some(last.high),
        magnitude: None,
        formed_at: None,
    });
    out
}

/// The daily/weekly bias, computed at the period roll: where the new open
/// sits against the period that just closed (above its high is a buy bias,
/// below its low a sell bias), confirmed by the structure's lean. Inside the
/// range with no structure agreement, the honest answer is no bias at all.
pub fn period_bias(candles: &[Candle], from_ns: i64, to_ns: i64) -> Option<Side> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for k in candles.iter().filter(|k| k.open_time >= from_ns && k.open_time < to_ns) {
        lo = lo.min(k.low);
        hi = hi.max(k.high);
    }
    if !lo.is_finite() {
        return None;
    }
    let last = candles.last()?;
    let lean = structure_lean(candles);
    if last.open > hi && lean == Some(Side::Buy) {
        return Some(Side::Buy);
    }
    if last.open < lo && lean == Some(Side::Sell) {
        return Some(Side::Sell);
    }
    None
}

/// The bias events, checked at the newest bar: a day (or week) that just
/// rolled gets its bias once.
pub fn detect_bias(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 12 {
        return out;
    }
    const HOUR_NS: i64 = 3_600_000_000_000;
    const DAY_NS: i64 = 24 * HOUR_NS;
    const WEEK_NS: i64 = 7 * DAY_NS;
    let n = candles.len() - 1;
    let last = &candles[n];
    let prev = &candles[n - 1];
    // The day roll: the previous day's range plus the structure lean.
    let newest_day = last.open_time.div_euclid(DAY_NS);
    let prev_day = prev.open_time.div_euclid(DAY_NS);
    if newest_day != prev_day {
        if let Some(side) = period_bias(candles, prev_day * DAY_NS, newest_day * DAY_NS) {
            out.push(MarketEvent {
                kind: EventKind::DailyBias,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time: last.open_time,
                close: last.close,
                price: last.open,
                side: Some(side),
                zone_low: None,
                zone_high: None,
                magnitude: None,
                formed_at: Some(last.open_time),
            });
        }
    }
    let newest_week = last.open_time.div_euclid(WEEK_NS);
    let prev_week = prev.open_time.div_euclid(WEEK_NS);
    if newest_week != prev_week {
        if let Some(side) = period_bias(candles, prev_week * WEEK_NS, newest_week * WEEK_NS) {
            out.push(MarketEvent {
                kind: EventKind::WeeklyBias,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time: last.open_time,
                close: last.close,
                price: last.open,
                side: Some(side),
                zone_low: None,
                zone_high: None,
                magnitude: None,
                formed_at: Some(last.open_time),
            });
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

    fn series(bars: &[(i64, f64, f64, f64, f64)]) -> Vec<Candle> {
        bars.iter().map(|(t, o, h, l, c)| k(*t, *o, *h, *l, *c)).collect()
    }

    const H: i64 = 3_600_000_000_000;

    fn uptrend(n: usize, start: i64, from: f64) -> Vec<Candle> {
        uptrend_with_dips(n, start, from, -0.006)
    }

    fn uptrend_with_dips(n: usize, start: i64, from: f64, dip: f64) -> Vec<Candle> {
        // The fixture's rules, learned the hard way: the drift must be
        // strong enough for the ADX to read trend (0.5%/bar here), the wave
        // strong enough to make real swings (2%), and the wicks SMALL and
        // additive -- a proportional padding makes adjacent highs float-equal
        // and the strict swing rule finds nothing at all.
        let mut bars = Vec::new();
        let mut price = from;
        for i in 0..n {
            // A TRIANGLE wave: a sine's peaks plateau across two bars and the
            // strict swing rule rejects ties; the triangle's extremes are
            // exactly one bar. Drift 0.4% up, swing 1.5% per bar each way.
            // The wave period must NOT divide the resample factor (an
            // 8-bar triangle's peaks repeat across two 4h bars exactly, and
            // the strict swing rule finds nothing at the coarser scale).
            let phase = i % 10;
            // The small wave sets the LTF's own rhythm; the BIG dips (every
            // 20 bars, deeper in the back half so the 4h has structure of
            // its own) keep the HTF's most recent swing low far below any
            // ordinary pullback.
            let swing = if phase < 5 { 0.005 } else { -0.005 };
            let big = if i % 20 >= 15 { dip } else { 0.002 };
            let open = price;
            price *= 1.004 + swing + big;
            // VARYING wicks: a continuous path shares every extreme between
            // two bars (one's close is the next's open), and uniform wicks
            // make the highs exactly equal -- the strict swing rule rejects
            // ties. Real data varies the wick; so does this fixture.
            let wick = 0.05 + (i % 3) as f64 * 0.01;
            let hi = open.max(price) + wick;
            let lo = open.min(price) - wick;
            bars.push((start + i as i64 * H, open, hi, lo, price));
        }
        series(&bars)
    }

    fn chop(n: usize, start: i64) -> Vec<Candle> {
        let mut bars = Vec::new();
        for i in 0..n {
            let c = 100.0 + (i as f64 / 2.0).sin() * 0.4;
            bars.push((start + i as i64 * H, c, c + 0.3, c - 0.3, c));
        }
        series(&bars)
    }

    #[test]
    fn the_classifier_reads_a_trend_and_chop_honestly() {
        let trend = uptrend(60, 0, 100.0);
        assert_eq!(classify(&trend), Regime::TrendUp, "a rising structure with a strong ADX");
        let flat = chop(60, 0);
        assert_eq!(classify(&flat), Regime::Range, "chop is a range");
    }

    #[test]
    fn a_regime_shift_fires_once_at_the_turn() {
        // Chop into a trend: the shift fires on the bar the classifier
        // changes its mind, and only there.
        let mut candles = chop(45, 0);
        candles.extend(uptrend(35, 45 * H, 100.5));
        let mut shifts = 0;
        for end in 34..candles.len() {
            for e in detect_regime(&candles[..=end], "T") {
                if e.kind == EventKind::RegimeShift && e.side == Some(Side::Buy) {
                    assert_eq!(e.bar_time, candles[end].open_time, "stamped on the shift bar");
                    shifts += 1;
                }
            }
        }
        assert!(shifts >= 1, "the turn produces a bullish regime shift");
        // Inside steady chop, no shift events at all.
        let flat = chop(60, 0);
        let mut flat_shifts = 0;
        for end in 34..flat.len() {
            flat_shifts += detect_regime(&flat[..=end], "T").len();
        }
        assert_eq!(flat_shifts, 0, "steady chop never shifts");
    }

    #[test]
    fn the_mtf_alignment_fires_when_the_ltf_turns_into_the_htfs_lean() {
        // The event is the LOWER timeframe turning into the higher's lean:
        // an established uptrend (the HTF reads Buy throughout), a pullback
        // (the LTF's lean flips down), and the resumption (the LTF turns
        // back up -- the bar that fires).
        let mut candles = uptrend_with_dips(50, 0, 100.0, -0.012);
        // The pullback: SHALLOW -- deep enough to flip the LTF's lean down,
        // never deep enough to break the higher timeframe's structure (a
        // pullback that breaks the HTF's lows honestly ends the bullish
        // lean, and there is nothing to re-join).
        let mut bars = Vec::new();
        let mut price = candles.last().expect("trend").close;
        for i in 0..6 {
            let open = price;
            price *= 0.985;
            let wick = 0.05 + (i % 3) as f64 * 0.01;
            bars.push(((50 + i) as i64 * H, open, open.max(price) + wick, open.min(price) - wick, price));
        }
        candles.extend(series(&bars));
        // The resumption: the uptrend restarts.
        let resume = uptrend_with_dips(30, 58 * H, price, -0.012);
        candles.extend(resume);
        let mut fired = false;
        for end in 60..candles.len() {
            for e in detect_mtf_alignment(&candles[..=end], "T") {
                if e.side == Some(Side::Buy) {
                    assert_eq!(e.bar_time, candles[end].open_time, "stamped on the turn bar");
                    fired = true;
                }
            }
        }
        assert!(fired, "the LTF turning back into the HTF's uptrend fires");
    }

    #[test]
    fn the_daily_bias_needs_the_open_outside_the_range_with_structure() {
        // Day 1 (48 bars): a rising day whose roll lands mid up-phase (the
        // dip cycle puts bars 15..19 and 35..39 underwater -- a roll inside
        // one reads no bullish lean, which is honest). Day 2 opens ABOVE
        // day 1's high with the structure rising: buy bias.
        let mut day1 = uptrend(48, 0, 100.0);
        let hi = day1.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let open2 = hi * 1.002;
        day1.push(k(48 * H, open2, open2 * 1.002, open2 * 0.999, open2 * 1.001));
        let events = detect_bias(&day1, "T");
        let bias = events.iter().find(|e| e.kind == EventKind::DailyBias);
        assert!(bias.is_some(), "the roll above the range with rising structure is a buy bias");
        assert_eq!(bias.expect("checked").side, Some(Side::Buy));
        // An open INSIDE the range is no bias.
        let mut day1b = uptrend(48, 0, 100.0);
        let mid = (day1b.iter().map(|c| c.high).fold(f64::MIN, f64::max)
            + day1b.iter().map(|c| c.low).fold(f64::MAX, f64::min))
            / 2.0;
        day1b.push(k(48 * H, mid, mid * 1.002, mid * 0.998, mid * 1.001));
        assert!(!detect_bias(&day1b, "T").iter().any(|e| e.kind == EventKind::DailyBias));
    }
}
