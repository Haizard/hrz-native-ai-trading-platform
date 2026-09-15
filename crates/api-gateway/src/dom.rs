//! The DOM's view of the order book.
//!
//! A depth-of-market panel needs more than a snapshot gives it. A ladder is
//! read by the *shape* of the two sides, which means every row wants its
//! cumulative size, and how big that is next to the deepest row on either
//! side.
//!
//! Both are arithmetic over market data, and `docs/14` forbids doing that in
//! JavaScript -- not because a running total is hard, but because the moment
//! the shell derives a number there are two implementations of it, and the
//! chart and the panel can end up disagreeing about the same book. So the
//! ladder is built here, in Rust, and the shell only formats what it is given.
//! `bar_pct` exists precisely so the panel never computes a proportion.
//!
//! The result is deliberately a **superset** of [`OrderBookSnapshot`]: the same
//! `symbol`, `timestamp`, `bids` and `asks`, with fields added per level.
//! Nothing downstream has to learn a second shape, and `OrderBookLevel` -- which
//! the database persists -- stays exactly as it is.

use analytics_core::{OrderBookLevel, OrderBookSnapshot};
use serde::Serialize;

/// One row of a ladder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LadderRow {
    /// Price of the level.
    pub price: f64,
    /// Size resting at this level.
    pub quantity: f64,
    /// Size from the best price to and including this level.
    pub cumulative: f64,
    /// How wide to draw this row's depth bar, 0..=100.
    ///
    /// A percentage rather than a fraction so the panel sets a width without
    /// computing one. It is scaled against the deepest row on *either* side,
    /// because comparing the two sides is the entire point of a ladder: scaled
    /// per side, a book with 0.01 resting against 100 would draw two full bars.
    pub bar_pct: f64,
}

/// A book rendered for a depth-of-market panel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ladder {
    /// Symbol the book is for.
    pub symbol: String,
    /// When the underlying snapshot was taken, unix nanoseconds.
    pub timestamp: i64,
    /// Best ask minus best bid, when both sides exist.
    ///
    /// `null` rather than `0` when one side is missing: an empty side means the
    /// spread is unknown, and zero means it is nothing.
    pub spread: Option<f64>,
    /// Bids, best first (descending price).
    pub bids: Vec<LadderRow>,
    /// Asks, best first (ascending price).
    pub asks: Vec<LadderRow>,
}

/// Build the ladder for a snapshot.
///
/// Pure, and synchronous on purpose: the numbers a panel draws are the kind of
/// thing that has to be testable without a socket.
#[must_use]
pub fn ladder(snapshot: &OrderBookSnapshot) -> Ladder {
    // Both sides are already best-first coming out of `OrderBook::snapshot`,
    // so the running total is just the book walked from the inside out.
    let bid_total: f64 = snapshot.bids.iter().map(|level| level.quantity).sum();
    let ask_total: f64 = snapshot.asks.iter().map(|level| level.quantity).sum();
    let deepest = bid_total.max(ask_total);

    let spread = match (snapshot.bids.first(), snapshot.asks.first()) {
        (Some(bid), Some(ask)) => Some(ask.price - bid.price),
        _ => None,
    };

    Ladder {
        symbol: snapshot.symbol.clone(),
        timestamp: snapshot.timestamp,
        spread,
        bids: side(&snapshot.bids, deepest),
        asks: side(&snapshot.asks, deepest),
    }
}

fn side(levels: &[OrderBookLevel], deepest: f64) -> Vec<LadderRow> {
    let mut cumulative = 0.0;
    levels
        .iter()
        .map(|level| {
            cumulative += level.quantity;
            LadderRow {
                price: level.price,
                quantity: level.quantity,
                cumulative,
                bar_pct: if deepest > 0.0 {
                    cumulative / deepest * 100.0
                } else {
                    0.0
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, quantity: f64) -> OrderBookLevel {
        OrderBookLevel { price, quantity }
    }

    fn book(bids: Vec<OrderBookLevel>, asks: Vec<OrderBookLevel>) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "BTCUSDT".into(),
            timestamp: 1_000,
            bids,
            asks,
        }
    }

    #[test]
    fn cumulative_walks_outwards_from_the_best_price() {
        let ladder = ladder(&book(
            vec![level(100.0, 2.0), level(99.0, 3.0), level(98.0, 1.0)],
            vec![level(101.0, 1.5), level(102.0, 4.0)],
        ));

        let bid_totals: Vec<f64> = ladder.bids.iter().map(|r| r.cumulative).collect();
        let ask_totals: Vec<f64> = ladder.asks.iter().map(|r| r.cumulative).collect();
        assert_eq!(bid_totals, vec![2.0, 5.0, 6.0]);
        assert_eq!(ask_totals, vec![1.5, 5.5]);
    }

    #[test]
    fn both_sides_are_scaled_against_the_same_deepest_row() {
        // Bids hold 6, asks hold 5.5. Asks must therefore be scaled against 6,
        // not against 5.5 -- otherwise a thin side draws a full bar and the
        // ladder stops being a comparison.
        let ladder = ladder(&book(
            vec![level(100.0, 2.0), level(99.0, 3.0), level(98.0, 1.0)],
            vec![level(101.0, 1.5), level(102.0, 4.0)],
        ));

        assert_eq!(ladder.bids.last().unwrap().bar_pct, 100.0);
        let last_ask = ladder.asks.last().unwrap().bar_pct;
        assert!(
            (last_ask - 5.5 / 6.0 * 100.0).abs() < 1e-9,
            "the ask side must be scaled by the bid total: {last_ask}"
        );
        assert!(last_ask < 100.0, "a thinner side must not draw a full bar");
    }

    #[test]
    fn the_ladder_is_a_superset_of_the_snapshot() {
        let snapshot = book(vec![level(100.0, 2.0)], vec![level(101.0, 3.0)]);
        let ladder = ladder(&snapshot);

        // Everything the snapshot said, unchanged, in the same order.
        assert_eq!(ladder.symbol, snapshot.symbol);
        assert_eq!(ladder.timestamp, snapshot.timestamp);
        assert_eq!(ladder.bids[0].price, 100.0);
        assert_eq!(ladder.bids[0].quantity, 2.0);
        assert_eq!(ladder.asks[0].price, 101.0);
        assert_eq!(ladder.asks[0].quantity, 3.0);
    }

    #[test]
    fn an_empty_book_has_no_spread_and_no_bars() {
        let ladder = ladder(&book(vec![], vec![]));
        assert!(ladder.bids.is_empty());
        assert!(ladder.asks.is_empty());
        assert_eq!(ladder.spread, None, "no spread is not a spread of zero");
    }

    #[test]
    fn a_one_sided_book_has_no_spread_but_still_has_bars() {
        let ladder = ladder(&book(vec![level(100.0, 2.0)], vec![]));
        assert_eq!(ladder.spread, None);
        assert_eq!(ladder.bids[0].bar_pct, 100.0);
    }

    #[test]
    fn a_zero_size_book_does_not_produce_nan_bars() {
        // NaN would serialise to `null` and become a CSS width the browser
        // silently drops, which looks like a rendering bug.
        let ladder = ladder(&book(vec![level(100.0, 0.0)], vec![level(101.0, 0.0)]));
        assert!(ladder.bids[0].bar_pct.is_finite());
        assert!(ladder.asks[0].bar_pct.is_finite());
    }
}
