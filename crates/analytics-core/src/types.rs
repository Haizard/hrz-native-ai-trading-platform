//! Core market-data types.
//!
//! These structs are defined **once** here and reused by every other crate
//! (market-data, db, strategy-runtime, backtester, sandbox, ai-agent,
//! api-gateway and the WASM chart engine). No crate may define a duplicate
//! `Candle`/`Trade`/`OrderBookSnapshot` -- see the cross-cutting contracts in
//! `docs/01-ARCHITECTURE-OVERVIEW.md`.

use serde::{Deserialize, Serialize};

use crate::error::AnalyticsError;
use std::fmt;
use std::str::FromStr;

/// Nanoseconds in one second. All internal timestamps are unix nanos, UTC.
pub const NS_PER_SEC: i64 = 1_000_000_000;

/// Candle resolution.
///
/// Serialized as the lowercase exchange-style string (`"1m"`, `"4h"`, `"1d"`)
/// so it round-trips cleanly through the Strategy DSL, the database and the
/// WebSocket API without a translation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    /// One minute.
    #[serde(rename = "1m")]
    M1,
    /// Five minutes.
    #[serde(rename = "5m")]
    M5,
    /// Fifteen minutes.
    #[serde(rename = "15m")]
    M15,
    /// One hour.
    #[serde(rename = "1h")]
    H1,
    /// Four hours.
    #[serde(rename = "4h")]
    H4,
    /// One day.
    #[serde(rename = "1d")]
    D1,
}

impl Timeframe {
    /// Duration of this timeframe in nanoseconds.
    #[must_use]
    pub const fn nanos(self) -> i64 {
        match self {
            Self::M1 => 60 * NS_PER_SEC,
            Self::M5 => 5 * 60 * NS_PER_SEC,
            Self::M15 => 15 * 60 * NS_PER_SEC,
            Self::H1 => 60 * 60 * NS_PER_SEC,
            Self::H4 => 4 * 60 * 60 * NS_PER_SEC,
            Self::D1 => 24 * 60 * 60 * NS_PER_SEC,
        }
    }

    /// Duration of this timeframe in seconds.
    #[must_use]
    pub const fn secs(self) -> i64 {
        self.nanos() / NS_PER_SEC
    }

    /// Bucket start (unix nanos) that `ts` belongs to.
    #[must_use]
    pub const fn bucket_of(self, ts: i64) -> i64 {
        let width = self.nanos();
        ts - ts.rem_euclid(width)
    }

    /// Every supported timeframe, coarse to fine.
    #[must_use]
    pub const fn all() -> &'static [Timeframe] {
        &[Self::D1, Self::H4, Self::H1, Self::M15, Self::M5, Self::M1]
    }
}

/// Timeframes are ordered by **duration**, not by declaration order.
///
/// Written by hand rather than derived on purpose. A derived `Ord` would
/// silently depend on the order the variants happen to appear in, so adding a
/// `1s` timeframe in the wrong place would produce a wrong sort with no compile
/// error. Every comparison routes through [`Timeframe::nanos`], which is the
/// same source of truth [`crate::resample`] uses to decide whether one
/// resolution can be aggregated into another.
impl Ord for Timeframe {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.nanos().cmp(&other.nanos())
    }
}

impl PartialOrd for Timeframe {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H4 => "4h",
            Self::D1 => "1d",
        };
        f.write_str(s)
    }
}

impl FromStr for Timeframe {
    type Err = AnalyticsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1m" => Ok(Self::M1),
            "5m" => Ok(Self::M5),
            "15m" => Ok(Self::M15),
            "1h" => Ok(Self::H1),
            "4h" => Ok(Self::H4),
            "1d" => Ok(Self::D1),
            other => Err(AnalyticsError::InvalidTimeframe(other.to_string())),
        }
    }
}

/// Aggressor side of a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    /// buyer was the aggressor (lifted the ask)
    Buy,
    /// seller was the aggressor (hit the bid)
    Sell,
}

/// An OHLCV candle with a buy/sell volume split.
///
/// `buy_volume`/`sell_volume` are required (not optional) because delta and CVD
/// depend on them. Candles are always built from the trade stream rather than
/// from exchange klines, so this split is consistent everywhere downstream
/// (`docs/04-MARKET-DATA-ENGINE.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    /// Symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution of this candle.
    pub timeframe: Timeframe,
    /// Candle open time, unix nanoseconds UTC.
    pub open_time: i64,
    /// Open price.
    pub open: f64,
    /// High price.
    pub high: f64,
    /// Low price.
    pub low: f64,
    /// Close price.
    pub close: f64,
    /// Total traded volume.
    pub volume: f64,
    /// Volume executed at ask (buyer-aggressed).
    pub buy_volume: f64,
    /// Volume executed at bid (seller-aggressed).
    pub sell_volume: f64,
}

impl Candle {
    /// Per-candle delta: `buy_volume - sell_volume`.
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.buy_volume - self.sell_volume
    }
}

/// A single executed trade (tick).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    /// Symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Exchange-assigned trade id; used for gap detection on reconnect.
    pub trade_id: u64,
    /// Execution price.
    pub price: f64,
    /// Executed quantity (base asset).
    pub quantity: f64,
    /// `true` when the **buyer** was the maker, i.e. the seller aggressed.
    ///
    /// This is Binance's field naming and is preserved verbatim so there is no
    /// ambiguity about direction: aggressor is `Sell` when this is `true`.
    pub is_buyer_maker: bool,
    /// Trade timestamp, unix nanoseconds UTC.
    pub timestamp: i64,
}

impl Trade {
    /// Aggressor side of this trade.
    #[must_use]
    pub fn side(&self) -> Side {
        if self.is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        }
    }

    /// Signed volume: positive for buy-aggressed, negative for sell-aggressed.
    #[must_use]
    pub fn signed_quantity(&self) -> f64 {
        match self.side() {
            Side::Buy => self.quantity,
            Side::Sell => -self.quantity,
        }
    }
}

/// One price level on one side of the order book.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrderBookLevel {
    /// Price of this level.
    pub price: f64,
    /// Resting quantity at this level.
    pub quantity: f64,
}

/// A full order-book snapshot at an instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    /// Symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Snapshot timestamp, unix nanoseconds UTC.
    pub timestamp: i64,
    /// Bids, best (highest price) first.
    pub bids: Vec<OrderBookLevel>,
    /// Asks, best (lowest price) first.
    pub asks: Vec<OrderBookLevel>,
}

/// Bid/ask volume traded at one price level inside one candle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FootprintCell {
    /// Price level (bucket mid/edge -- set by the bucket size in use).
    pub price_level: f64,
    /// Volume executed at the bid (seller-aggressed).
    pub bid_volume: f64,
    /// Volume executed at the ask (buyer-aggressed).
    pub ask_volume: f64,
    /// `ask_volume - bid_volume`.
    pub delta: f64,
}

impl FootprintCell {
    /// Total volume at this price level.
    #[must_use]
    pub fn total_volume(&self) -> f64 {
        self.bid_volume + self.ask_volume
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeframe_round_trips_through_str() {
        for tf in Timeframe::all() {
            let s = tf.to_string();
            assert_eq!(Timeframe::from_str(&s).unwrap(), *tf);
        }
    }

    #[test]
    fn timeframe_rejects_unknown() {
        assert!(Timeframe::from_str("7m").is_err());
        assert!(Timeframe::from_str("").is_err());
    }

    #[test]
    fn candle_delta_is_buy_minus_sell() {
        let c = Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 10.0,
            buy_volume: 7.0,
            sell_volume: 3.0,
        };
        assert!((c.delta() - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn buyer_maker_true_means_sell_aggressed() {
        let t = Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 1,
            price: 100.0,
            quantity: 2.0,
            is_buyer_maker: true,
            timestamp: 0,
        };
        assert_eq!(t.side(), Side::Sell);
        assert!((t.signed_quantity() + 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bucket_of_snaps_down_to_boundary() {
        let minute = Timeframe::M1.nanos();
        // 90 seconds in -> start of the first minute
        assert_eq!(Timeframe::M1.bucket_of(90 * NS_PER_SEC), minute);
        assert_eq!(Timeframe::M1.bucket_of(0), 0);
    }

    #[test]
    fn ordering_follows_duration_not_declaration_order() {
        // Sorted from the declaration-independent `all()` listing, which is
        // documented coarse-to-fine; the reverse must be strictly increasing by
        // duration, which is what a caller comparing coarseness depends on.
        let mut fine_to_coarse = Timeframe::all().to_vec();
        fine_to_coarse.reverse();
        for pair in fine_to_coarse.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{:?} should sort before {:?}",
                pair[0],
                pair[1]
            );
            assert!(pair[1] > pair[0]);
        }
        assert_eq!(fine_to_coarse[0], Timeframe::M1);
        assert_eq!(fine_to_coarse[fine_to_coarse.len() - 1], Timeframe::D1);
        // And sorting a shuffled copy lands in the same place.
        let mut shuffled = vec![
            Timeframe::H1,
            Timeframe::M1,
            Timeframe::D1,
            Timeframe::M15,
            Timeframe::M5,
            Timeframe::H4,
        ];
        shuffled.sort();
        assert_eq!(shuffled, fine_to_coarse);
    }
}
