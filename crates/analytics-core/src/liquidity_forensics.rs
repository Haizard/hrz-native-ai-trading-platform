//! Liquidity engineering and manipulation proxies from order-book snapshots.
//!
//! These are deterministic measurements of visible book dynamics. A flag is a
//! hypothesis for a researcher, not an accusation or a trading signal.

use serde::{Deserialize, Serialize};

use crate::types::OrderBookSnapshot;

/// One book-based observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookObservation {
    /// Bid/ask imbalance in `[-1, 1]`.
    pub imbalance: f64,
    /// Best bid price, when present.
    pub best_bid: Option<f64>,
    /// Best ask price, when present.
    pub best_ask: Option<f64>,
    /// Total displayed bid quantity.
    pub bid_depth: f64,
    /// Total displayed ask quantity.
    pub ask_depth: f64,
}

/// Read one book snapshot without fetching anything.
#[must_use]
pub fn observe_book(book: &OrderBookSnapshot) -> Option<BookObservation> {
    let bid_depth: f64 = book.bids.iter().map(|level| level.quantity.max(0.0)).sum();
    let ask_depth: f64 = book.asks.iter().map(|level| level.quantity.max(0.0)).sum();
    if bid_depth + ask_depth <= 0.0 {
        return None;
    }
    Some(BookObservation {
        imbalance: (bid_depth - ask_depth) / (bid_depth + ask_depth),
        best_bid: book
            .bids
            .iter()
            .map(|level| level.price)
            .fold(None, |acc, price| {
                Some(acc.map_or(price, |best: f64| best.max(price)))
            }),
        best_ask: book
            .asks
            .iter()
            .map(|level| level.price)
            .fold(None, |acc, price| {
                Some(acc.map_or(price, |best: f64| best.min(price)))
            }),
        bid_depth,
        ask_depth,
    })
}

/// Evidence of liquidity manipulation patterns across snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiquidityForensics {
    /// Largest absolute imbalance observed.
    pub max_imbalance: f64,
    /// Number of snapshots whose imbalance reversed sign.
    pub reversals: usize,
    /// Number of snapshots where depth vanished relative to the previous one.
    pub vanishing_depth_events: usize,
    /// Composite suspicion score in `[0, 1]`; not a probability.
    pub suspicion_score: f64,
    /// Explicit limitation.
    pub limitation: String,
}

/// Detect layering/spoofing-like dynamics in a sequence of books.
#[must_use]
pub fn analyze_liquidity_forensics(books: &[OrderBookSnapshot]) -> Option<LiquidityForensics> {
    if books.len() < 3 {
        return None;
    }
    let observations: Vec<BookObservation> = books.iter().filter_map(observe_book).collect();
    if observations.len() < 3 {
        return None;
    }
    let mut max_imbalance = 0.0_f64;
    let mut reversals = 0usize;
    let mut vanishing = 0usize;
    for pair in observations.windows(2) {
        max_imbalance = max_imbalance
            .max(pair[0].imbalance.abs())
            .max(pair[1].imbalance.abs());
        if pair[0].imbalance * pair[1].imbalance < 0.0
            && pair[0].imbalance.abs() > 0.3
            && pair[1].imbalance.abs() > 0.3
        {
            reversals += 1;
        }
        let before = pair[0].bid_depth + pair[0].ask_depth;
        let after = pair[1].bid_depth + pair[1].ask_depth;
        if before > 0.0 && after / before < 0.35 {
            vanishing += 1;
        }
    }
    let pairs = observations.len() - 1;
    let suspicion_score = (0.5 * max_imbalance
        + 0.3 * (reversals as f64 / pairs as f64)
        + 0.2 * (vanishing as f64 / pairs as f64))
        .clamp(0.0, 1.0);
    Some(LiquidityForensics {
        max_imbalance,
        reversals,
        vanishing_depth_events: vanishing,
        suspicion_score,
        limitation: "book-shape heuristic; cannot distinguish a spoof from genuine liquidity"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OrderBookLevel;

    fn book(bid: f64, ask: f64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 0,
            bids: vec![OrderBookLevel {
                price: 99.0,
                quantity: bid,
            }],
            asks: vec![OrderBookLevel {
                price: 101.0,
                quantity: ask,
            }],
        }
    }

    #[test]
    fn a_stable_book_has_low_suspicion() {
        let books: Vec<OrderBookSnapshot> = (0..5).map(|_| book(10.0, 10.0)).collect();
        let report = analyze_liquidity_forensics(&books).unwrap();
        assert_eq!(report.max_imbalance, 0.0);
        assert_eq!(report.suspicion_score, 0.0);
    }

    #[test]
    fn reversals_and_vanishing_depth_raise_the_score() {
        let books = vec![book(90.0, 10.0), book(10.0, 90.0), book(1.0, 1.0)];
        let report = analyze_liquidity_forensics(&books).unwrap();
        assert!(report.reversals >= 1);
        assert!(report.vanishing_depth_events >= 1);
        assert!(report.suspicion_score > 0.0 && report.suspicion_score <= 1.0);
        assert!(report.limitation.contains("cannot distinguish"));
    }

    #[test]
    fn too_few_books_are_refused() {
        assert!(analyze_liquidity_forensics(&[book(1.0, 1.0)]).is_none());
        assert!(observe_book(&book(0.0, 0.0)).is_none());
    }
}
