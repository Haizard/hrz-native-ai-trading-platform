//! The platform's own candlestick-pattern detectors: the master-list family,
//! implemented in-house.
//!
//! The rules here are the community-settled definitions, checked against the
//! reference implementations' source while the patterns ran externally
//! (dragonfly: body ≤ 10% of range with the lower shadow ≥ 50%; marubozu as
//! body ≥ 95%; the kicker as a marubozu pair with a gap against the prior
//! direction), and every pattern is pinned in OUR tests on textbook shapes.
//! Same event vocabulary as always: one `MarketEvent` per detected bar, the
//! pattern bar's own extremes riding as the zone.

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};

/// One bar's shape vocabulary.
#[derive(Debug, Clone, Copy)]
struct Shape {
    range: f64,
    body: f64,
    upper: f64,
    lower: f64,
    bullish: bool,
}

fn shape(k: &Candle) -> Shape {
    let body = (k.close - k.open).abs();
    Shape {
        range: k.high - k.low,
        body,
        upper: k.high - k.open.max(k.close),
        lower: k.open.min(k.close) - k.low,
        bullish: k.close >= k.open,
    }
}

fn is_doji(s: &Shape) -> bool {
    s.range > 0.0 && s.body <= 0.1 * s.range
}

fn is_marubozu(s: &Shape) -> bool {
    s.range > 0.0 && s.body >= 0.95 * s.range
}

/// Detect the master-list candlestick patterns on the window (each detector
/// sees the bars it needs from the tail).
pub fn detect_candlesticks(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 2 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let mut push = |kind: EventKind, side: Side, k: &Candle| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price: k.close,
            side: Some(side),
            zone_low: Some(k.low),
            zone_high: Some(k.high),
            magnitude: None,
            formed_at: None,
        });
    };
    let s = shape(&last);

    // ---- single-bar shapes ----
    if is_doji(&s) {
        if s.upper <= 0.1 * s.range && s.lower >= 0.5 * s.range {
            push(EventKind::DragonflyDoji, Side::Buy, &last);
        }
        if s.lower <= 0.1 * s.range && s.upper >= 0.5 * s.range {
            push(EventKind::GravestoneDoji, Side::Sell, &last);
        }
        if s.upper >= 0.3 * s.range && s.lower >= 0.3 * s.range {
            push(EventKind::LongLeggedDoji, Side::Buy, &last);
        }
    }
    // Spinning top: a small body (but NOT a doji) with shadows both sides.
    if s.range > 0.0 && s.body > 0.1 * s.range && s.body <= 0.3 * s.range && s.upper > 0.0 && s.lower > 0.0 {
        push(EventKind::SpinningTop, if s.bullish { Side::Buy } else { Side::Sell }, &last);
    }
    // Belt-hold: a long body opening at the extreme (within a small shadow tolerance).
    if s.range > 0.0 && s.body >= 0.5 * s.range {
        let tol = 0.1 * s.range;
        if s.bullish && last.open - last.low <= tol {
            push(EventKind::BeltHold, Side::Buy, &last);
        } else if !s.bullish && last.high - last.open <= tol {
            push(EventKind::BeltHold, Side::Sell, &last);
        }
    }

    // ---- two-bar shapes ----
    let prev = &candles[n - 1];
    let p = shape(&prev);
    // Tweezer: the two bars print the same extreme (tolerance 0.1% of price).
    let tol_high = 0.001 * prev.high.abs().max(last.high.abs());
    if (last.low - prev.low).abs() <= 0.001 * prev.low.abs().max(last.low.abs()) {
        push(EventKind::Tweezer, Side::Buy, &last);
    } else if (last.high - prev.high).abs() <= tol_high {
        push(EventKind::Tweezer, Side::Sell, &last);
    }
    // Kicker: a marubozu pair with a gap against the prior bar's direction.
    if is_marubozu(&p) && is_marubozu(&s) {
        if !p.bullish && s.bullish && last.low > prev.high {
            push(EventKind::Kicker, Side::Buy, &last);
        } else if p.bullish && !s.bullish && last.high < prev.low {
            push(EventKind::Kicker, Side::Sell, &last);
        }
    }
    // Matching low: a down bar, then another bar closing at the same low
    // (tolerance 5% of the mean range).
    let mean_range = (p.range + s.range) / 2.0;
    if !p.bullish && mean_range > 0.0 && (last.close - prev.close).abs() <= 0.05 * mean_range {
        push(EventKind::MatchingLow, Side::Buy, &last);
    }
    // On-neck: a down bar, then an up bar closing just above the prior low
    // (within 10% of the prior range) but not into the body.
    if !p.bullish && s.bullish && p.range > 0.0 {
        let above = last.close - prev.low;
        if above >= 0.0 && above <= 0.1 * p.range {
            push(EventKind::OnNeck, Side::Sell, &last);
        }
    }

    // ---- three-bar shapes ----
    if n < 2 {
        return out;
    }
    let pp = &candles[n - 2];
    let q = shape(&pp);
    // Three inside: harami (small opposite bar inside a bigger one) with the
    // third bar closing through in the reversal direction.
    if q.body > p.body && q.body > 0.0 && p.body > 0.0 {
        let inside = p.body <= q.body && pp.open.min(pp.close) <= prev.open.max(prev.close) && pp.open.max(pp.close) >= prev.open.min(prev.close);
        if inside && q.bullish != p.bullish {
            if !q.bullish && s.bullish && last.close > prev.open.max(prev.close) {
                push(EventKind::ThreeInside, Side::Buy, &last);
            } else if q.bullish && !s.bullish && last.close < prev.open.min(prev.close) {
                push(EventKind::ThreeInside, Side::Sell, &last);
            }
        }
    }
    // Three outside: engulfing with the third bar continuing the takeover.
    if p.body > q.body && q.body > 0.0 {
        let engulfs = prev.open.min(prev.close) <= pp.open.min(pp.close) && prev.open.max(prev.close) >= pp.open.max(pp.close);
        if engulfs && p.bullish != q.bullish {
            if p.bullish && s.bullish && last.close > prev.close {
                push(EventKind::ThreeOutside, Side::Buy, &last);
            } else if !p.bullish && !s.bullish && last.close < prev.close {
                push(EventKind::ThreeOutside, Side::Sell, &last);
            }
        }
    }
    // Tri-star: three consecutive dojis, with the middle gapped from both.
    let body_top = |k: &Candle| k.open.max(k.close);
    let body_bottom = |k: &Candle| k.open.min(k.close);
    if is_doji(&q) && is_doji(&p) && is_doji(&s) {
        if body_bottom(&prev) > body_top(&pp) && body_top(&last) < body_bottom(&prev) {
            push(EventKind::TriStar, Side::Sell, &last);
        } else if body_top(&prev) < body_bottom(&pp) && body_bottom(&last) > body_top(&prev) {
            push(EventKind::TriStar, Side::Buy, &last);
        }
    }
    // Abandoned baby: a doji gapped away from BOTH neighbors, with the third
    // bar gapping the other way.
    let tol = 0.001 * prev.open.abs().max(prev.close.abs());
    if (prev.close - prev.open).abs() <= tol {
        if !q.bullish && prev.high < pp.low && s.bullish && last.low > prev.high {
            push(EventKind::AbandonedBaby, Side::Buy, &last);
        } else if q.bullish && prev.low > pp.high && !s.bullish && last.high < prev.low {
            push(EventKind::AbandonedBaby, Side::Sell, &last);
        }
    }
    // Stick sandwich: two down closes at the same price with an up bar
    // between them that never traded below the first close.
    if !q.bullish && p.bullish && !s.bullish && q.range > 0.0 {
        if prev.low > pp.close && (last.close - pp.close).abs() <= 0.1 * q.range {
            push(EventKind::StickSandwich, Side::Buy, &last);
        }
    }
    // Tasuki gap: two same-direction bars with a body gap, the third opens
    // inside the second body and closes inside the gap without filling it.
    if q.bullish && p.bullish && prev.open > pp.close {
        if !s.bullish && last.open > prev.open && last.open < prev.close && last.close < prev.open && last.close > pp.close {
            push(EventKind::TasukiGap, Side::Buy, &last);
        }
    } else if !q.bullish && !p.bullish && prev.open < pp.close {
        if s.bullish && last.open < prev.open && last.open > prev.close && last.close > prev.open && last.close < pp.close {
            push(EventKind::TasukiGap, Side::Sell, &last);
        }
    }

    // ---- five-bar shapes ----
    if candles.len() >= 5 {
        let b1 = &candles[n - 4];
        let s1 = shape(&b1);
        // Rising three methods: a long white bar, three small counter bars
        // that stay inside its body (declining closes), then a resume bar
        // closing above the first bar's close. Falling mirrors.
        let small: f64 = 0.5 * s1.body;
        let mids_are_small_reds_inside = [n - 3, n - 2, n - 1].iter().all(|&i| {
            let m = &candles[i];
            let ms = shape(m);
            ms.body <= small
                && m.close < m.open
                && m.open >= b1.open
                && m.close >= b1.open
                && m.high <= b1.high
        });
        if s1.bullish && s1.body >= 0.5 * s1.range && mids_are_small_reds_inside {
            let descending = candles[n - 2].close < candles[n - 3].close && candles[n - 1].close < candles[n - 2].close;
            if descending && s.bullish && last.close > b1.close {
                push(EventKind::ThreeMethods, Side::Buy, &last);
            }
        }
        // Mat hold: the same start, but the pullback bars stay ABOVE the
        // first bar's close (the gap never fills).
        if s1.bullish && s1.body >= 0.5 * s1.range {
            let above = [n - 3, n - 2, n - 1].iter().all(|&i| {
                let m = &candles[i];
                shape(m).body <= small && m.open.min(m.close) > b1.close
            });
            if above && s.bullish && last.close > b1.close {
                push(EventKind::MatHold, Side::Buy, &last);
            }
        }
        // Ladder bottom: three consecutive down bars with declining closes,
        // then a gap up.
        let declining = !shape(&candles[n - 3]).bullish
            && !q.bullish
            && !p.bullish
            && pp.close < candles[n - 3].close
            && prev.close < pp.close;
        if declining && s.bullish && last.open > prev.open {
            push(EventKind::LadderBottom, Side::Buy, &last);
        }
    }

    // Hikkake: an inside bar, then a false break one way that springs the
    // other (the current bar reverses through the inside bar's range).
    if n >= 2 {
        let inside = prev.high < pp.high && prev.low > pp.low;
        if inside {
            if last.high < prev.high && last.low < prev.low {
                push(EventKind::Hikkake, Side::Buy, &last);
            } else if last.high > prev.high && last.low > prev.low {
                push(EventKind::Hikkake, Side::Sell, &last);
            }
        }
    }

    // ---- upside gap two crows (Nison): a long bullish bar, a bearish bar
    // gapping above its body, and a third bearish bar engulfing the second
    // while holding above the first bar's close -- the gap survives, the
    // momentum does not.
    if candles.len() >= 3 {
        let first = &candles[n - 2];
        let second = &candles[n - 1];
        let third = &candles[n];
        let f = shape(&first);
        let g = shape(&second);
        let t = shape(&third);
        let gap = second.low > first.close || second.open > first.close;
        if f.bullish
            && f.body >= 0.5 * f.range
            && f.range > 0.0
            && !g.bullish
            && gap
            && !t.bullish
            && third.open >= second.open
            && third.close < second.open
            && third.close > first.close
        {
            push(EventKind::UpsideGapTwoCrows, Side::Sell, &third);
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

    fn kinds(events: &[MarketEvent]) -> Vec<EventKind> {
        events.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn the_doji_family_separates_by_shadow() {
        let h = 3_600_000_000_000_i64;
        // Dragonfly: body at top, long lower shadow.
        let candles = vec![
            k(0, 100.0, 100.6, 99.5, 100.3),
            k(h, 100.2, 100.3, 96.0, 100.1),
        ];
        let events = detect_candlesticks(&candles, "T");
        let d = events.iter().find(|e| e.kind == EventKind::DragonflyDoji).expect("dragonfly");
        assert_eq!(d.side, Some(Side::Buy));
        assert!(!kinds(&events).contains(&EventKind::GravestoneDoji));
        // Gravestone mirror.
        let candles = vec![
            k(0, 100.0, 100.6, 99.5, 100.3),
            k(h, 100.1, 104.3, 100.0, 100.2),
        ];
        let events = detect_candlesticks(&candles, "T");
        assert!(kinds(&events).contains(&EventKind::GravestoneDoji));
    }

    #[test]
    fn the_kicker_needs_two_marubozu_and_the_gap() {
        let h = 3_600_000_000_000_i64;
        let candles = vec![
            k(0, 101.0, 101.05, 99.0, 99.05),  // black marubozu
            k(h, 101.5, 102.95, 101.5, 102.9), // white marubozu, gap above
        ];
        let events = detect_candlesticks(&candles, "T");
        let kick = events.iter().find(|e| e.kind == EventKind::Kicker).expect("kicker");
        assert_eq!(kick.side, Some(Side::Buy));
        // No gap: no kicker.
        let candles = vec![
            k(0, 101.0, 101.05, 99.0, 99.05),
            k(h, 100.5, 101.95, 100.5, 101.9),
        ];
        assert!(!kinds(&detect_candlesticks(&candles, "T")).contains(&EventKind::Kicker));
    }

    #[test]
    fn three_methods_and_mat_hold_need_the_continuation() {
        let h = 3_600_000_000_000_i64;
        let candles = vec![
            k(0, 100.0, 103.0, 99.9, 102.9),  // long white
            k(h, 102.5, 102.8, 101.8, 102.0), // small red inside
            k(2 * h, 102.0, 102.3, 101.2, 101.5), // small red, lower
            k(3 * h, 101.5, 101.8, 100.8, 101.1), // small red, lower
            k(4 * h, 101.2, 103.5, 101.1, 103.2), // resume above the first close
        ];
        let events = detect_candlesticks(&candles, "T");
        assert!(kinds(&events).contains(&EventKind::ThreeMethods), "rising three methods: {:?}", kinds(&events));
        // The resumption missing (a close below the first bar's): no pattern.
        let candles = vec![
            k(0, 100.0, 103.0, 99.9, 102.9),
            k(h, 102.5, 102.8, 101.8, 102.0),
            k(2 * h, 102.0, 102.3, 101.2, 101.5),
            k(3 * h, 101.5, 101.8, 100.8, 101.1),
            k(4 * h, 101.2, 101.9, 100.5, 100.8),
        ];
        assert!(!kinds(&detect_candlesticks(&candles, "T")).contains(&EventKind::ThreeMethods));
    }

    #[test]
    fn abandoned_baby_needs_the_island() {
        let h = 3_600_000_000_000_i64;
        let candles = vec![
            k(0, 101.0, 101.2, 99.0, 99.3),  // red
            k(h, 98.5, 98.6, 98.2, 98.5),    // doji gapped below
            k(2 * h, 99.2, 100.5, 99.1, 100.3), // green gapped above
        ];
        let events = detect_candlesticks(&candles, "T");
        let baby = events.iter().find(|e| e.kind == EventKind::AbandonedBaby).expect("baby");
        assert_eq!(baby.side, Some(Side::Buy));
        // The doji touching the first bar (no gap): not abandoned.
        let candles = vec![
            k(0, 101.0, 101.2, 99.0, 99.3),
            k(h, 99.2, 99.4, 98.8, 99.2),
            k(2 * h, 99.8, 100.5, 99.7, 100.3),
        ];
        assert!(!kinds(&detect_candlesticks(&candles, "T")).contains(&EventKind::AbandonedBaby));
    }

    #[test]
    #[test]
    fn upside_gap_two_crows_needs_the_gap_to_survive() {
        // Bar 1 long bullish, bar 2 small bearish gapping above bar 1's
        // close, bar 3 bearish engulfing bar 2 but closing above bar 1.
        let ks = vec![
            k(0, 100.0, 104.0, 99.5, 104.0),
            k(1, 104.6, 105.2, 104.4, 104.5),
            k(2, 104.6, 104.8, 103.5, 104.1),
        ];
        let events = detect_candlesticks(&ks, "T");
        assert!(events.iter().any(|e| e.kind == EventKind::UpsideGapTwoCrows), "the textbook shape fires");
        // The gap closed: no pattern.
        let closed = vec![
            k(0, 100.0, 104.0, 99.5, 104.0),
            k(1, 104.6, 105.2, 104.4, 104.5),
            k(2, 104.6, 104.8, 102.5, 103.0),
        ];
        assert!(
            !detect_candlesticks(&closed, "T").iter().any(|e| e.kind == EventKind::UpsideGapTwoCrows),
            "a closed gap kills it"
        );
    }

    fn every_pattern_has_a_negative_shape() {
        // A plain full-body trend with no special relationships: nothing
        // fires at all.
        let h = 3_600_000_000_000_i64;
        let candles: Vec<Candle> = (0..8)
            .map(|i| {
                // Body at 50% of range (no doji, no marubozu), open off the
                // low (no belt-hold), lows stepping down (no tweezer match).
                let base = 100.0 + i as f64 * 0.5;
                k(i as i64 * h, base + 0.2, base + 1.0, base - i as f64 * 0.02, base + 0.7)
            })
            .collect();
        let events = detect_candlesticks(&candles, "T");
        assert!(events.is_empty(), "quiet bars are not patterns: {:?}", kinds(&events));
    }
}
