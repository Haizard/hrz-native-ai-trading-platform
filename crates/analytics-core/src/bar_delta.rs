//! Bar delta statistics -- what happened *inside* the bar.
//!
//! The end-of-bar delta says who won. The extremes of the running delta
//! inside the bar say who was winning *at any point* -- and whether that
//! changed. A bar that printed a strongly positive peak delta but closed with
//! a negative delta trapped late buyers near the top: the "max/min delta"
//! feature of the "Order Flow IQ" suite, and the evidence behind theses about
//! trapped positions and absorption.
//!
//! Every number here is computed from the bar's own trade tape. Without tick
//! data none of them can be produced, and the honest answer is "unavailable"
//! -- the same rule the footprint tools follow (`docs/09` principle: the agent
//! is never handed a fabricated number).

use serde::{Deserialize, Serialize};

use crate::types::Candle;

/// One candle's intra-bar order-flow statistics, computed from its trades.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BarDeltaStats {
    /// Open time of the candle these statistics describe.
    pub open_time: i64,
    /// The bar's closing delta: `buy - sell` over all trades.
    pub delta: f64,
    /// Highest value the running cumulative delta reached inside the bar.
    /// Zero-valued when the bar has no trades (see [`BarDeltaStats::is_empty`]).
    pub max_delta: f64,
    /// Lowest value the running cumulative delta reached inside the bar.
    pub min_delta: f64,
    /// Volume-weighted average trade price inside the bar.
    /// Zero when the bar has no trades.
    pub intrabar_vwap: f64,
    /// Number of trades in the bar.
    pub trades: usize,
}

impl BarDeltaStats {
    /// Whether the bar had no trades, in which case every derived field is
    /// zero-valued and callers should present "unavailable" rather than zeros.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.trades == 0
    }

    /// Where the closing delta sits between the extremes, in `[0, 1]`:
    /// `0` at `min_delta`, `1` at `max_delta`.
    ///
    /// A value near `0` on a bar whose peak was strongly positive is the
    /// trapped-late-buyers signature. `None` when the bar has no trades.
    #[must_use]
    pub fn delta_close_position(&self) -> Option<f64> {
        let span = self.max_delta - self.min_delta;
        if self.is_empty() || span.abs() < f64::EPSILON {
            return None;
        }
        Some((self.delta - self.min_delta) / span)
    }
}

/// Compute [`BarDeltaStats`] for one candle from exactly its own trades.
///
/// Trades must be in chronological order (the tape's order); the running
/// cumulative delta walks them in that order.
#[must_use]
pub fn bar_delta_stats(candle: &Candle, trades: &[crate::types::Trade]) -> BarDeltaStats {
    let mut running = 0.0_f64;
    let mut max_delta = 0.0_f64;
    let mut min_delta = 0.0_f64;
    let mut notional_sum = 0.0_f64;
    let mut volume_sum = 0.0_f64;

    for trade in trades {
        running += trade.signed_quantity();
        max_delta = max_delta.max(running);
        min_delta = min_delta.min(running);
        notional_sum += trade.price * trade.quantity;
        volume_sum += trade.quantity;
    }

    BarDeltaStats {
        open_time: candle.open_time,
        delta: candle.delta(),
        max_delta,
        min_delta,
        intrabar_vwap: if volume_sum > 0.0 {
            notional_sum / volume_sum
        } else {
            0.0
        },
        trades: trades.len(),
    }
}

/// Statistics for a window of candles, oldest first.
///
/// Trades are bucketed into candles by time with a binary search per candle,
/// matching [`crate::footprint::build_footprints`], so the output is
/// index-aligned with the input and a candle with no trades reads as empty.
#[must_use]
pub fn bar_delta_stats_window(
    candles: &[Candle],
    trades: &[crate::types::Trade],
) -> Vec<BarDeltaStats> {
    candles
        .iter()
        .map(|candle| {
            let end = candle.open_time + candle.timeframe.nanos();
            let from = trades.partition_point(|t| t.timestamp < candle.open_time);
            let to = trades.partition_point(|t| t.timestamp < end);
            bar_delta_stats(candle, &trades[from..to])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Timeframe, Trade};

    fn candle(open_time: i64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn trade(price: f64, qty: f64, buyer_maker: bool, ts: i64) -> Trade {
        Trade {
            symbol: "TEST".into(),
            trade_id: ts as u64,
            price,
            quantity: qty,
            is_buyer_maker: buyer_maker,
            timestamp: ts,
        }
    }

    #[test]
    fn peak_then_reversal_is_visible() {
        // A bar that bought +10 early, then sold off to close at -4:
        // max_delta 10, min_delta -4, closing delta -4.
        let c = candle(0, 6.0, 10.0);
        let trades = vec![
            trade(100.0, 10.0, false, 1_000), // +10
            trade(100.0, 6.0, true, 2_000),   // +4
            trade(100.0, 10.0, true, 3_000),  // -6
        ];
        let stats = bar_delta_stats(&c, &trades);

        assert!((stats.max_delta - 10.0).abs() < 1e-9);
        assert!((stats.min_delta - (-6.0)).abs() < 1e-9);
        assert!((stats.delta - (-4.0)).abs() < 1e-9);
        // Closing delta sits near the bottom of its range.
        let position = stats.delta_close_position().expect("non-empty bar");
        assert!(position < 0.35, "close should sit near min: {position}");
    }

    #[test]
    fn intrabar_vwap_is_volume_weighted() {
        let c = candle(0, 1.0, 1.0);
        let trades = vec![
            trade(100.0, 1.0, false, 1_000),
            trade(110.0, 3.0, true, 2_000),
        ];
        let stats = bar_delta_stats(&c, &trades);
        // (100*1 + 110*3) / 4 = 107.5
        assert!((stats.intrabar_vwap - 107.5).abs() < 1e-9);
    }

    #[test]
    fn empty_bar_reads_as_empty() {
        let c = candle(0, 0.0, 0.0);
        let stats = bar_delta_stats(&c, &[]);
        assert!(stats.is_empty());
        assert!(stats.delta_close_position().is_none());
    }

    #[test]
    fn window_is_index_aligned() {
        let candles = vec![candle(0, 1.0, 0.0), candle(60_000_000_000, 0.0, 0.0)];
        let trades = vec![trade(100.0, 1.0, false, 1_000)];
        let stats = bar_delta_stats_window(&candles, &trades);

        assert_eq!(stats.len(), 2);
        assert!((stats[0].delta - 1.0).abs() < 1e-9);
        assert!(stats[1].is_empty(), "second bar has no trades");
    }
}
