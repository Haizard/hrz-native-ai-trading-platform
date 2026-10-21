//! SMT divergence: the crack in correlation between two related markets.
//!
//! ICT's Smart Money Technique: two correlated instruments should print the
//! same structure. When one makes a lower low and the other makes a *higher*
//! low, the move on the first is engineered -- the liquidity was taken on
//! one side of the pair only. The detection is honest about what it needs:
//! TWO series, their swings, and nothing else.
//!
//! A pair concept by construction, so it lives here as a pure function over
//! two candle slices and is served by its own route, not the per-symbol
//! engine stream.

use crate::types::{Candle, Side};

/// One detected divergence.
#[derive(Debug, Clone, PartialEq)]
pub struct SmtDivergence {
    /// The open time of the divergent swing bar on symbol A, unix nanos.
    pub bar_time: i64,
    /// Bullish: A swept a low B did not confirm (A's downside is engineered).
    /// Bearish: the mirror at the highs.
    pub direction: Side,
    /// A's divergent swing level.
    pub a_level: f64,
    /// B's corresponding swing level.
    pub b_level: f64,
    /// The prior swing's open time on A (the span the divergence covers).
    pub prior_time: i64,
    /// The newest swing's open time on A.
    pub newest_time: i64,
}

/// The swings of one series (strict local extremes, lookback bars either
/// side), newest last: (open_time, price).
fn swings(candles: &[Candle], lookback: usize, highs: bool) -> Vec<(i64, f64)> {
    let mut out = Vec::new();
    let n = candles.len();
    for i in lookback..n.saturating_sub(lookback) {
        let k = &candles[i];
        if highs {
            if (i - lookback..=i + lookback).all(|j| j == i || candles[j].high < k.high) {
                out.push((k.open_time, k.high));
            }
        } else if (i - lookback..=i + lookback).all(|j| j == i || candles[j].low > k.low) {
            out.push((k.open_time, k.low));
        }
    }
    out
}

/// Detect SMT divergences between series `a` and series `b`.
///
/// The newest two swing lows (or highs) on each: A sweeps (lower low /
/// higher high) while B does NOT confirm (higher low / lower high). The
/// swings need not share bars -- correlated markets print their swings a
/// bar or two apart, so the comparison is between each series's own newest
/// structure, matched in time order.
#[must_use]
pub fn detect_smt(a: &[Candle], b: &[Candle], lookback: usize) -> Vec<SmtDivergence> {
    let mut out = Vec::new();
    if a.len() < lookback * 2 + 4 || b.len() < lookback * 2 + 4 {
        return out;
    }
    let a_lows = swings(a, lookback, false);
    let b_lows = swings(b, lookback, false);
    let a_highs = swings(a, lookback, true);
    let b_highs = swings(b, lookback, true);

    // Bullish divergence: A's newest swing low is LOWER than its prior, but
    // B's newest swing low is HIGHER than its prior.
    if a_lows.len() >= 2 && b_lows.len() >= 2 {
        let (a_prior, a_new) = (a_lows[a_lows.len() - 2], a_lows[a_lows.len() - 1]);
        let (b_prior, b_new) = (b_lows[b_lows.len() - 2], b_lows[b_lows.len() - 1]);
        if a_new.1 < a_prior.1 && b_new.1 > b_prior.1 {
            out.push(SmtDivergence {
                bar_time: a_new.0,
                direction: Side::Buy,
                a_level: a_new.1,
                b_level: b_new.1,
                prior_time: a_prior.0,
                newest_time: a_new.0,
            });
        }
    }
    // Bearish divergence: A's newest swing high is HIGHER, B's is LOWER.
    if a_highs.len() >= 2 && b_highs.len() >= 2 {
        let (a_prior, a_new) = (a_highs[a_highs.len() - 2], a_highs[a_highs.len() - 1]);
        let (b_prior, b_new) = (b_highs[b_highs.len() - 2], b_highs[b_highs.len() - 1]);
        if a_new.1 > a_prior.1 && b_new.1 < b_prior.1 {
            out.push(SmtDivergence {
                bar_time: a_new.0,
                direction: Side::Sell,
                a_level: a_new.1,
                b_level: b_new.1,
                prior_time: a_prior.0,
                newest_time: a_new.0,
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

    /// A series with two swing lows: first at `l1`, second at `l2`.
    fn two_lows(l1: f64, l2: f64) -> Vec<Candle> {
        let h = 3_600_000_000_000_i64;
        vec![
            k(0, 100.0, 100.5, 99.5, 100.2),
            k(h, 100.2, 100.4, 100.0, 100.3),
            k(2 * h, 100.3, 100.6, l1, l1 + 0.3),
            k(3 * h, l1 + 0.3, l1 + 0.8, l1 + 0.2, l1 + 0.6),
            k(4 * h, l1 + 0.6, l1 + 0.9, l1 + 0.5, l1 + 0.7),
            k(5 * h, l1 + 0.7, l1 + 1.0, l1 + 0.6, l1 + 0.8),
            k(6 * h, l1 + 0.8, l1 + 1.1, l1 + 0.7, l1 + 0.9),
            k(7 * h, l1 + 0.9, l1 + 1.2, l2, l2 + 0.3),
            k(8 * h, l2 + 0.3, l2 + 0.8, l2 + 0.2, l2 + 0.6),
            k(9 * h, l2 + 0.6, l2 + 0.9, l2 + 0.5, l2 + 0.7),
            k(10 * h, l2 + 0.7, l2 + 1.0, l2 + 0.6, l2 + 0.8),
        ]
    }

    #[test]
    fn a_lower_low_unconfirmed_is_a_bullish_divergence() {
        // A sweeps: second low (98.0) below the first (99.0).
        let a = two_lows(99.0, 98.0);
        // B does not confirm: second low (98.7) ABOVE the first (98.5).
        let b = two_lows(98.5, 98.7);
        let smt = detect_smt(&a, &b, 2);
        assert_eq!(smt.len(), 1, "exactly one divergence: {:?}", smt);
        assert_eq!(smt[0].direction, Side::Buy);
        assert_eq!(smt[0].a_level, 98.0);
        assert_eq!(smt[0].b_level, 98.7);
    }

    #[test]
    fn a_confirmed_structure_is_no_divergence() {
        // Both sweep: both second lows below their firsts.
        let a = two_lows(99.0, 98.0);
        let b = two_lows(99.2, 98.1);
        assert!(detect_smt(&a, &b, 2).is_empty());
        // And the mirror: neither sweeps.
        let a = two_lows(98.0, 98.5);
        let b = two_lows(98.2, 98.6);
        assert!(detect_smt(&a, &b, 2).is_empty());
    }
}
