//! The TPO / Market Profile family: time-at-price auctions. The volume
//! profile counts VOLUME at price; the TPO counts TIME -- how many bars
//! traded at each price. Same bucketing, different question: where did the
//! market spend its day, and where did it only pass through?

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};
use crate::volume_profile::round_bucket;

/// One price cell of the profile.
#[derive(Debug, Clone, Copy)]
pub struct TpoCell {
    /// The cell's center price.
    pub price: f64,
    /// Bars that traded through this cell.
    pub count: usize,
}

/// The profile of a window: the cells, the point of control, the value
/// area (70% of time), and the single prints.
#[derive(Debug, Clone)]
pub struct TpoProfile {
    /// Cells ascending by price.
    pub cells: Vec<TpoCell>,
    /// The cell with the most time.
    pub poc: f64,
    /// Value area high (70% of time).
    pub vah: f64,
    /// Value area low.
    pub val: f64,
    /// Cells with exactly ONE bar (unfinished auction).
    pub single_prints: Vec<f64>,
}

/// The time-at-price profile of `candles`, bucketed at `bucket_size`.
pub fn tpo_profile(candles: &[Candle], bucket_size: f64) -> TpoProfile {
    if candles.is_empty() || bucket_size <= 0.0 {
        return TpoProfile {
            cells: Vec::new(),
            poc: 0.0,
            vah: 0.0,
            val: 0.0,
            single_prints: Vec::new(),
        };
    }
    let mut counts: std::collections::BTreeMap<i64, (f64, usize)> = std::collections::BTreeMap::new();
    for c in candles {
        // Every cell the bar's range covers gets one time unit. The cell
        // count is bounded by the window: a 500-bar window with a 1%
        // bucket can never produce more cells than bars to fill them.
        let first = (c.low / bucket_size).floor() as i64;
        let last = (c.high / bucket_size).floor() as i64;
        for bucket in first..=last {
            let price = (bucket as f64 + 0.5) * bucket_size;
            counts.entry(bucket).or_insert((price, 0)).1 += 1;
        }
    }
    let cells: Vec<TpoCell> = counts
        .values()
        .map(|(price, count)| TpoCell { price: *price, count: *count })
        .collect();
    if cells.is_empty() {
        return TpoProfile {
            cells,
            poc: 0.0,
            vah: 0.0,
            val: 0.0,
            single_prints: Vec::new(),
        };
    }
    let poc = cells.iter().max_by_key(|c| c.count).map(|c| c.price).unwrap_or(0.0);
    // The value area: 70% of total time, expanded from the POC outward.
    let total: usize = cells.iter().map(|c| c.count).sum();
    let target = (total as f64 * 0.7).ceil() as usize;
    let poc_idx = cells.iter().position(|c| c.price == poc).unwrap_or(0);
    let (mut lo, mut hi) = (poc_idx, poc_idx);
    let mut covered = cells[poc_idx].count;
    while covered < target && (lo > 0 || hi + 1 < cells.len()) {
        let expand_lo = lo > 0 && (hi + 1 >= cells.len() || cells[lo - 1].count >= cells[hi + 1].count);
        if expand_lo {
            lo -= 1;
            covered += cells[lo].count;
        } else {
            hi += 1;
            covered += cells[hi].count;
        }
    }
    let single_prints: Vec<f64> = cells.iter().filter(|c| c.count == 1).map(|c| c.price).collect();
    TpoProfile {
        val: cells[lo].price,
        vah: cells[hi].price,
        cells,
        poc,
        single_prints,
    }
}

/// The session's initial balance: the range of its first `ib_bars` bars
/// (the first hour on the 15m chart, the convention).
fn initial_balance(candles: &[Candle], ib_bars: usize) -> Option<(f64, f64)> {
    if candles.len() < ib_bars + 1 {
        return None;
    }
    let hi = candles[..ib_bars].iter().map(|c| c.high).fold(f64::MIN, f64::max);
    let lo = candles[..ib_bars].iter().map(|c| c.low).fold(f64::MAX, f64::min);
    Some((lo, hi))
}

/// The auction concepts' events on the window's tail.
pub fn detect_auction(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 30 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let mut push = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price,
            side,
            zone_low: low,
            zone_high: high,
            magnitude: None,
            geometry: None,
            formed_at: formed,
        });
    };

    // -- Initial balance: the breakout (close beyond the IB) and the
    // reversal (trade beyond, close back inside).
    if let Some((ib_lo, ib_hi)) = initial_balance(candles, 4) {
        let prev = &candles[n - 1];
        if prev.close <= ib_hi && last.close > ib_hi {
            push(EventKind::InitialBalanceBreakout, Some(Side::Buy), ib_hi, Some(ib_lo), Some(ib_hi), Some(candles[0].open_time));
        } else if prev.close >= ib_lo && last.close < ib_lo {
            push(EventKind::InitialBalanceBreakout, Some(Side::Sell), ib_lo, Some(ib_lo), Some(ib_hi), Some(candles[0].open_time));
        }
        // The reversal: traded beyond and closed back in, on the tail.
        let traded_above = last.high > ib_hi || prev.high > ib_hi;
        let traded_below = last.low < ib_lo || prev.low < ib_lo;
        if traded_above && last.close < ib_hi && prev.close <= ib_hi && prev.high <= ib_hi {
            push(EventKind::InitialBalanceReversal, Some(Side::Sell), ib_hi, Some(ib_lo), Some(ib_hi), Some(candles[0].open_time));
        } else if traded_below && last.close > ib_lo && prev.close >= ib_lo && prev.low >= ib_lo {
            push(EventKind::InitialBalanceReversal, Some(Side::Buy), ib_lo, Some(ib_lo), Some(ib_hi), Some(candles[0].open_time));
        }
    }

    // -- The profile: acceptance beyond value, single prints, poor extremes.
    let span = last.close * 0.01;
    let bucket = round_bucket(span.max(1e-9), 50);
    let profile = tpo_profile(candles, bucket);
    if !profile.cells.is_empty() {
        // Acceptance: TWO consecutive closes beyond the value area's edge.
        let prev = &candles[n - 1];
        if prev.close > profile.vah && last.close > profile.vah {
            push(EventKind::AcceptanceEvent, Some(Side::Buy), profile.vah, Some(profile.val), Some(profile.vah), None);
        } else if prev.close < profile.val && last.close < profile.val {
            push(EventKind::AcceptanceEvent, Some(Side::Sell), profile.val, Some(profile.val), Some(profile.vah), None);
        }
        // The newest single print still UNFILLED below/above price: the
        // unfinished auction the market owes a revisit.
        let unvisited_below = profile
            .single_prints
            .iter()
            .filter(|p| **p < last.low)
            .max_by(|a, b| a.total_cmp(b));
        let unvisited_above = profile
            .single_prints
            .iter()
            .filter(|p| **p > last.high)
            .min_by(|a, b| a.total_cmp(b));
        if let Some(p) = unvisited_below {
            push(EventKind::SinglePrint, Some(Side::Buy), *p, Some(*p), Some(*p + bucket), None);
        }
        if let Some(p) = unvisited_above {
            push(EventKind::SinglePrint, Some(Side::Sell), *p, Some(*p - bucket), Some(*p), None);
        }
    }

    // -- Poor extremes: a swing high/low whose own cell carries HEAVY time
    // (the auction never rejected it -- unfinished business at the edge).
    if !profile.cells.is_empty() && profile.cells.len() > 4 {
        let edge_hi = profile.cells[profile.cells.len() - 1];
        let edge_lo = profile.cells[0];
        let mean_count = profile.cells.iter().map(|c| c.count).sum::<usize>() as f64 / profile.cells.len() as f64;
        let heavy = |c: &TpoCell| c.count as f64 > mean_count * 1.5;
        // The newest swing high sits in the heavy edge cell and the close
        // already left it below: the poor high.
        if heavy(&edge_hi) && last.close < edge_hi.price - bucket {
            push(EventKind::PoorExtreme, Some(Side::Buy), edge_hi.price, Some(edge_hi.price - bucket), Some(edge_hi.price), None);
        }
        if heavy(&edge_lo) && last.close > edge_lo.price + bucket {
            push(EventKind::PoorExtreme, Some(Side::Sell), edge_lo.price, Some(edge_lo.price), Some(edge_lo.price + bucket), None);
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
            timeframe: Timeframe::M15,
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
    fn the_profile_counts_time_not_volume() {
        // Three bars at 100, one bar at 101: time concentrates at 100 even
        // though the 101 bar's "volume" would be identical here.
        let candles = vec![
            k(0, 100.0, 100.4, 99.8, 100.1),
            k(900_000_000_000, 100.1, 100.5, 99.9, 100.2),
            k(1_800_000_000_000, 100.2, 100.6, 100.0, 100.3),
            k(2_700_000_000_000, 100.3, 101.2, 100.2, 101.0),
        ];
        let p = tpo_profile(&candles, 0.2);
        assert_eq!(p.poc.floor(), 100.0, "time concentrates at 100");
        // The 101 cell got exactly one bar: a single print.
        assert!(p.single_prints.iter().any(|price| *price > 100.8), "the one-bar cell is a single print");
        assert!(p.val <= p.poc && p.poc <= p.vah, "the value area brackets the POC");
    }

    #[test]
    fn the_ib_breakout_fires_beyond_the_first_hours_range() {
        // IB (bars 0..3): 100..101. Then a close at 102.
        let mut candles: Vec<Candle> = (0..30).map(|i| k(i as i64 * 900_000_000_000, 100.4, 100.9, 100.1, 100.5)).collect();
        candles.push(k(30 * 900_000_000_000, 100.6, 102.2, 100.5, 102.0));
        let events = detect_auction(&candles, "T");
        let ib = events.iter().find(|e| e.kind == EventKind::InitialBalanceBreakout);
        assert!(ib.is_some(), "closing beyond the first hour fires");
        assert_eq!(ib.expect("checked").side, Some(Side::Buy));
    }

    #[test]
    fn acceptance_needs_two_closes_beyond_value() {
        // A profile concentrated at 100 (lots of time), then two closes at
        // the top -- one is a probe, two is acceptance.
        let mut candles: Vec<Candle> = (0..40).map(|i| k(i as i64 * 900_000_000_000, 100.0, 100.6, 99.6, 100.1)).collect();
        candles.push(k(40 * 900_000_000_000, 100.1, 102.0, 100.0, 101.8));
        candles.push(k(41 * 900_000_000_000, 101.8, 102.4, 101.6, 102.1));
        let events = detect_auction(&candles, "T");
        let acc = events.iter().find(|e| e.kind == EventKind::AcceptanceEvent);
        assert!(acc.is_some(), "two closes above the value area is acceptance");
        assert_eq!(acc.expect("checked").side, Some(Side::Buy));
    }
}
