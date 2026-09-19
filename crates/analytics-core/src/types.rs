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
    /// One week.
    ///
    /// The venue's own weekly bars, not seven daily bars stitched together.
    ///
    /// Binance aligns `1w` to Monday 00:00 UTC, and [`Self::bucket_of`] agrees
    /// with that -- but the reason is worth stating, because the usual
    /// explanation is wrong. The epoch began on a **Thursday**
    /// (1970-01-01), so the epoch's own week runs Thursday to Wednesday and
    /// the first Monday (1970-01-05) is *four days into* week 0, not the start
    /// of it. What makes the two agree is that a weekday repeats exactly every
    /// seven days: the first Monday sits at offset 4 within its week, and every
    /// later week's Monday sits at that same offset 4. So every week bucket
    /// after 0 starts on a Monday, and the only boundary that does not is
    /// week 0 itself -- which ended on 1970-01-07, before any crypto bar.
    #[serde(rename = "1w")]
    W1,
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
            Self::W1 => 7 * 24 * 60 * 60 * NS_PER_SEC,
        }
    }

    /// Duration of this timeframe in seconds.
    #[must_use]
    pub const fn secs(self) -> i64 {
        self.nanos() / NS_PER_SEC
    }

    /// Bucket start (unix nanos) that `ts` belongs to.
    ///
    /// Pure division for every intraday and daily resolution, because the epoch
    /// began at a UTC midnight and those boundaries are all derived from it.
    ///
    /// **Weekly is the exception**, and it is not a detail: the epoch was a
    /// *Thursday*, so dividing by seven days puts the boundary on Thursday
    /// 00:00 UTC. Binance's own `1w` bars open on **Monday** 00:00 UTC --
    /// verified against `/api/v3/klines?interval=1w`, which returns
    /// `2026-09-14 Monday 00:00` and the Mondays before it. Without the shift a
    /// weekly bar built from this platform's own trade stream would sit in a
    /// different bucket from the venue's own bar, three days out, for the whole
    /// series -- which is the disagreement the shared `analytics-core` exists
    /// to make impossible.
    ///
    /// The shift moves the *division* rather than the result, which is the part
    /// worth stating because getting it wrong is silent: subtracting
    /// `W1_MONDAY_OFFSET` puts every Monday on a whole number of weeks from the
    /// epoch, so the floor is Monday-aligned, and adding it back recovers the
    /// real timestamp. An earlier version added the offset back and then
    /// subtracted a whole week, which made every bar a week early.
    #[must_use]
    pub const fn bucket_of(self, ts: i64) -> i64 {
        let width = self.nanos();
        match self {
            Self::W1 => {
                let shifted = ts - Self::W1_MONDAY_OFFSET;
                shifted - shifted.rem_euclid(width) + Self::W1_MONDAY_OFFSET
            }
            _ => ts - ts.rem_euclid(width),
        }
    }

    /// How far the first Monday is from the epoch.
    ///
    /// 1970-01-01 was a Thursday, so the first Monday was 1970-01-05 -- four
    /// days later. Mondays are therefore at `4 days + 7k`, and shifting by four
    /// days makes them land on whole weeks from zero. This is the constant the
    /// weekly bucket is built from, and the first thing to check if the venue
    /// ever changes its alignment.
    const W1_MONDAY_OFFSET: i64 = 4 * 24 * 60 * 60 * NS_PER_SEC;

    /// Every supported timeframe, coarse to fine.
    #[must_use]
    pub const fn all() -> &'static [Timeframe] {
        &[
            Self::W1,
            Self::D1,
            Self::H4,
            Self::H1,
            Self::M15,
            Self::M5,
            Self::M1,
        ]
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

impl Timeframe {
    /// Every resolution, cheapest first.
    ///
    /// The same order as [`Self::nanos`] and the one the storage ladder uses.
    /// Exists so a caller that must *list* the accepted values -- a refusal
    /// message, a schema, a shell's dropdown -- reads them from the type rather
    /// than restating them, which is how a list of "known timeframes" drifts
    /// out of date the moment [`Self::W1`] is added.
    pub const ALL: [Self; 7] = [
        Self::M1,
        Self::M5,
        Self::M15,
        Self::H1,
        Self::H4,
        Self::D1,
        Self::W1,
    ];

    /// Canonical name, as written in a document or a query string.
    ///
    /// The `&'static str` form of [`Timeframe`]: `Display` cannot hand one out,
    /// and a caller building a list of the accepted values wants the strings
    /// rather than a formatter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H4 => "4h",
            Self::D1 => "1d",
            Self::W1 => "1w",
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
            "1w" => Ok(Self::W1),
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

impl Side {
    /// Every variant, for exhaustive tests.
    pub const ALL: [Self; 2] = [Self::Buy, Self::Sell];

    /// Canonical name, as it appears on the wire.
    ///
    /// `Side` itself derives `Serialize` without a rename, so it travels as
    /// `"Buy"`. That is fine for an order or a trade, where it is a value in a
    /// typed message, and wrong for a chart scene, where everything else is
    /// `snake_case` and the shell switches on the string. This is the bridge.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }

    /// The side that would act against this one.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
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
    fn a_week_bucket_lands_on_a_monday_like_the_venue() {
        // The venue aligns `1w` to **Monday** 00:00 UTC. Checked against the
        // live endpoint rather than reasoned about:
        //
        //   GET /api/v3/klines?symbol=BTCUSDT&interval=1w&limit=3
        //   2026-08-31 Monday 00:00 UTC
        //   2026-09-07 Monday 00:00 UTC
        //   2026-09-14 Monday 00:00 UTC
        //
        // The reason this needs a shift is that 1970-01-01 was a *Thursday*.
        // Plain division by seven days puts the boundary on Thursday 00:00, so
        // a weekly bar built from this platform's own stream would land three
        // days away from the venue's bar for the whole series. The first two
        // versions of this test asserted the wrong alignment in opposite
        // directions; the dates below are the ones that were actually verified.
        const NS_PER_DAY: i64 = 24 * 60 * 60 * NS_PER_SEC;
        let week = Timeframe::W1.nanos();

        // 2026-09-10T22:26:40Z, a Thursday. Its Monday-aligned week began on
        // 2026-09-07, so the bucket must be three days before the timestamp.
        let thursday: i64 = 1_789_000_000 * NS_PER_SEC;
        let bucket = Timeframe::W1.bucket_of(thursday);
        assert_eq!(
            (thursday - bucket) / NS_PER_DAY,
            3,
            "a Thursday sits three days into a Monday-started week"
        );
        assert_eq!(bucket % NS_PER_DAY, 0, "a week bucket is a whole day");
        // Monday-aligned buckets are *not* whole weeks from the epoch -- that
        // is the whole point, since the epoch was a Thursday. They are four
        // days past a whole week: the offset the shift exists to introduce.
        assert_eq!(
            (bucket / NS_PER_DAY) % 7,
            4,
            "a Monday bucket sits four days past a whole number of weeks from the epoch"
        );

        // Every bucket boundary is the same weekday, and that weekday is
        // Monday. Monday is day index 4 after the epoch plus a multiple of 7.
        let bucket_day = bucket / NS_PER_DAY;
        assert_eq!(
            (bucket_day - 4).rem_euclid(7),
            0,
            "day index {bucket_day} must be a Monday"
        );

        // The venue's own bar opens at this bucket, so the two agree.
        let binance_week_open: i64 = 1_788_739_200 * NS_PER_SEC; // 2026-09-07T00:00Z
        assert_eq!(
            bucket, binance_week_open,
            "the bucket must be exactly the open time the venue reports"
        );

        // Sunday belongs to the week that started six days earlier, and the
        // next Monday opens a new one.
        let sunday = bucket + 6 * NS_PER_DAY;
        assert_eq!(Timeframe::W1.bucket_of(sunday), bucket, "Sunday closes the week");
        let next_monday = bucket + week;
        assert_eq!(
            Timeframe::W1.bucket_of(next_monday),
            next_monday,
            "a Monday is its own bucket start"
        );
        assert_eq!(
            Timeframe::W1.bucket_of(next_monday - 1),
            bucket,
            "one nanosecond earlier is still the previous week"
        );

        // And the property holds across a long span, so a leap year or a
        // century boundary cannot quietly move it.
        for week_number in 0..600_i64 {
            let open = binance_week_open + week_number * week;
            assert_eq!(
                Timeframe::W1.bucket_of(open),
                open,
                "week {week_number} after 2026-09-07 does not open on a Monday"
            );
            assert_eq!(
                (open / NS_PER_DAY - 4).rem_euclid(7),
                0,
                "week {week_number} does not start on a Monday"
            );
        }
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
        assert_eq!(fine_to_coarse[fine_to_coarse.len() - 1], Timeframe::W1);
        // And sorting a shuffled copy lands in the same place.
        let mut shuffled = vec![
            Timeframe::H1,
            Timeframe::M1,
            Timeframe::D1,
            Timeframe::W1,
            Timeframe::M15,
            Timeframe::M5,
            Timeframe::H4,
        ];
        shuffled.sort();
        assert_eq!(shuffled, fine_to_coarse);
    }
}
