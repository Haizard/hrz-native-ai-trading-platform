//! Footprint -- bid vs ask volume at every price inside a candle.
//!
//! A candle tells you *how much* traded. A footprint tells you *who* traded at
//! *which price*: how much lifted the ask (aggressive buyers) and how much hit
//! the bid (aggressive sellers) at each level. That per-level split is what
//! makes imbalance, absorption and liquidity detection possible at all -- none
//! of them can be computed from OHLCV alone.
//!
//! ## Bucketing
//!
//! Levels are fixed-width buckets spanning the traded range of the candle,
//! using the same convention as [`volume_profile`](crate::volume_profile):
//! **contiguous** buckets including empty ones, `price_level` = bucket
//! midpoint. Empty levels are kept on purpose -- an imbalance is defined
//! against a *neighbouring* level, so removing the empty ones would make two
//! unrelated prices look adjacent.
//!
//! ## Getting the trades
//!
//! [`build_footprint`] derives the candle from the trades themselves and infers
//! the timeframe (the finest standard resolution that contains every trade in
//! one bucket). The live pipeline already knows both, so it uses
//! [`build_footprint_for_candle`] or [`build_footprints`] instead.

use serde::{Deserialize, Serialize};

use crate::imbalance::{detect_imbalances, ImbalanceEvent};
use crate::types::{Candle, FootprintCell, Timeframe, Trade};
use crate::volume_profile::MAX_BUCKETS;

/// One candle's footprint: the OHLCV candle plus the per-price bid/ask split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FootprintCandle {
    /// The candle this footprint describes.
    pub candle: Candle,
    /// Per-price cells, **ascending** by price, contiguous and including empty
    /// levels.
    pub cells: Vec<FootprintCell>,
    /// Imbalances detected with [`ImbalanceConfig::default()`](crate::imbalance::ImbalanceConfig::default).
    ///
    /// Populated by the trade-based constructors. It is **always empty** for
    /// footprints built by [`build_footprint_from_candle`], which cannot
    /// support the calculation -- see that function's docs.
    ///
    /// Callers that want a different ratio should re-run
    /// [`detect_imbalances_with`](crate::imbalance::detect_imbalances_with);
    /// this field is the default-ratio convenience view.
    pub imbalances: Vec<ImbalanceEvent>,
}

impl FootprintCandle {
    /// A footprint with no cells, e.g. when the input was empty.
    #[must_use]
    pub fn empty(candle: Candle) -> Self {
        Self {
            candle,
            cells: Vec::new(),
            imbalances: Vec::new(),
        }
    }

    /// Whether any price level traded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Total volume across every cell.
    #[must_use]
    pub fn total_volume(&self) -> f64 {
        self.cells.iter().map(FootprintCell::total_volume).sum()
    }

    /// Candle delta: `ask - bid` over the whole footprint.
    ///
    /// Equals [`Candle::delta`] when the footprint was built from the same
    /// trades that built the candle.
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.cells.iter().map(|c| c.delta).sum()
    }

    /// The cell containing `price`, if any.
    #[must_use]
    pub fn cell_at(&self, price: f64) -> Option<&FootprintCell> {
        self.cells
            .iter()
            .find(|c| (c.price_level - price).abs() <= f64::EPSILON)
    }

    /// Price level with the most volume in this candle.
    #[must_use]
    pub fn poc(&self) -> Option<f64> {
        self.cells
            .iter()
            .max_by(|a, b| a.total_volume().total_cmp(&b.total_volume()))
            .filter(|c| c.total_volume() > 0.0)
            .map(|c| c.price_level)
    }

    /// The lowest price level that actually traded.
    #[must_use]
    pub fn lowest_traded_level(&self) -> Option<f64> {
        self.cells
            .iter()
            .find(|c| c.total_volume() > 0.0)
            .map(|c| c.price_level)
    }

    /// The highest price level that actually traded.
    #[must_use]
    pub fn highest_traded_level(&self) -> Option<f64> {
        self.cells
            .iter()
            .rev()
            .find(|c| c.total_volume() > 0.0)
            .map(|c| c.price_level)
    }
}

/// Build a footprint from the trades of a single candle.
///
/// The candle is **derived** from the trades (OHLCV plus the buy/sell split,
/// aggressor side taken from `is_buyer_maker`). The timeframe is inferred as
/// the finest standard resolution that fits every trade into one bucket, and
/// the candle's `open_time` is that bucket's start.
///
/// `trades` must be in chronological order -- `first()` becomes the open and
/// `last()` the close. When you already have the candle, prefer
/// [`build_footprint_for_candle`]: inference is a convenience, not the path the
/// live pipeline takes.
#[must_use]
pub fn build_footprint(trades: &[Trade], bucket_size: f64) -> FootprintCandle {
    let candle = derive_candle(trades);
    build_footprint_for_candle(&candle, trades, bucket_size)
}

/// Build a footprint for a candle you already have.
///
/// `trades` must be exactly the trades belonging to `candle`; they are not
/// filtered here. Use [`build_footprints`] to do the bucketing for you.
#[must_use]
pub fn build_footprint_for_candle(
    candle: &Candle,
    trades: &[Trade],
    bucket_size: f64,
) -> FootprintCandle {
    let cells = cells_from_trades(trades, bucket_size);
    let mut footprint = FootprintCandle {
        candle: candle.clone(),
        cells,
        imbalances: Vec::new(),
    };
    footprint.imbalances = detect_imbalances(&footprint, crate::imbalance::DEFAULT_IMBALANCE_RATIO);
    footprint
}

/// Footprint for every candle, distributing `trades` into their buckets.
///
/// Both slices must be sorted ascending by time; the split is done with a
/// binary search per candle, so the cost is `O(candles * log(trades))` rather
/// than a full scan per candle.
#[must_use]
pub fn build_footprints(
    candles: &[Candle],
    trades: &[Trade],
    bucket_size: f64,
) -> Vec<FootprintCandle> {
    candles
        .iter()
        .map(|candle| {
            let end = candle.open_time + candle.timeframe.nanos();
            let from = trades.partition_point(|t| t.timestamp < candle.open_time);
            let to = trades.partition_point(|t| t.timestamp < end);
            build_footprint_for_candle(candle, &trades[from..to], bucket_size)
        })
        .collect()
}

/// Footprint derived from a candle alone, spreading volume uniformly across
/// `[low, high]`.
///
/// **This cannot detect imbalances, and does not pretend to.** With no tick
/// data there is no way to know which side aggressed at which level, so every
/// cell inherits the candle's *aggregate* buy/sell ratio. On a candle that
/// closed 3:1 bullish, every single level then looks like a 3x buy imbalance --
/// a fabricated stack of signals produced by nothing but arithmetic. So this
/// constructor returns `imbalances: Vec::new()` regardless.
///
/// The cells are still useful for coarse volume-at-price work on historical
/// candles that predate trade capture. For anything footprint-level, use
/// [`build_footprint_for_candle`] or [`build_footprints`] with real trades.
#[must_use]
pub fn build_footprint_from_candle(candle: &Candle, bucket_size: f64) -> FootprintCandle {
    FootprintCandle {
        candle: candle.clone(),
        cells: cells_from_candle(candle, bucket_size),
        imbalances: Vec::new(),
    }
}

/// Bucket trades into contiguous cells with the bid/ask split applied.
fn cells_from_trades(trades: &[Trade], bucket_size: f64) -> Vec<FootprintCell> {
    if trades.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return Vec::new();
    }

    let min_price = trades.iter().map(|t| t.price).fold(f64::INFINITY, f64::min);
    let max_price = trades
        .iter()
        .map(|t| t.price)
        .fold(f64::NEG_INFINITY, f64::max);

    let Some(cells) = empty_cells(min_price, max_price, bucket_size) else {
        return Vec::new();
    };

    let count = cells.len();
    let mut cells = cells;
    for trade in trades {
        let index = bucket_index(trade.price, min_price, bucket_size, count);
        let cell = &mut cells[index];
        if trade.is_buyer_maker {
            // Buyer was the maker => the seller aggressed => volume hit the bid.
            cell.bid_volume += trade.quantity;
        } else {
            cell.ask_volume += trade.quantity;
        }
    }

    for cell in &mut cells {
        cell.delta = cell.ask_volume - cell.bid_volume;
    }
    cells
}

/// Spread a candle's volume uniformly across its range (an approximation).
fn cells_from_candle(candle: &Candle, bucket_size: f64) -> Vec<FootprintCell> {
    if !bucket_size.is_finite() || bucket_size <= 0.0 || candle.high < candle.low {
        return Vec::new();
    }

    let Some(mut cells) = empty_cells(candle.low, candle.high, bucket_size) else {
        return Vec::new();
    };

    let count = cells.len();
    let first = bucket_index(candle.low, candle.low, bucket_size, count);
    let last = bucket_index(candle.high, candle.low, bucket_size, count);
    let spanned = last - first + 1;

    #[allow(clippy::cast_precision_loss)]
    let divisor = spanned as f64;
    let per_cell_bid = candle.sell_volume / divisor;
    let per_cell_ask = candle.buy_volume / divisor;

    for cell in cells.iter_mut().take(last + 1).skip(first) {
        cell.bid_volume = per_cell_bid;
        cell.ask_volume = per_cell_ask;
        cell.delta = cell.ask_volume - cell.bid_volume;
    }

    cells
}

/// Contiguous empty cells spanning `[min_price, max_price]`, or `None` when the
/// range is unusable or would exceed [`MAX_BUCKETS`].
fn empty_cells(min_price: f64, max_price: f64, bucket_size: f64) -> Option<Vec<FootprintCell>> {
    if !min_price.is_finite() || !max_price.is_finite() || max_price < min_price {
        return None;
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = ((max_price - min_price) / bucket_size).floor() as usize + 1;
    if count == 0 || count > MAX_BUCKETS {
        return None;
    }

    Some(
        (0..count)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let offset = (i as f64 + 0.5) * bucket_size;
                FootprintCell {
                    price_level: min_price + offset,
                    bid_volume: 0.0,
                    ask_volume: 0.0,
                    delta: 0.0,
                }
            })
            .collect(),
    )
}

/// Index of the bucket holding `price`, clamped to the last bucket so a price
/// exactly on the upper edge cannot fall off the end.
fn bucket_index(price: f64, min_price: f64, bucket_size: f64, count: usize) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = ((price - min_price) / bucket_size).floor() as usize;
    index.min(count - 1)
}

/// Derive an OHLCV candle from a chronological trade slice.
fn derive_candle(trades: &[Trade]) -> Candle {
    let Some(first) = trades.first() else {
        return Candle {
            symbol: String::new(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        };
    };

    let last = trades.last().unwrap_or(first);
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut volume = 0.0;
    let mut buy_volume = 0.0;
    let mut sell_volume = 0.0;

    for trade in trades {
        high = high.max(trade.price);
        low = low.min(trade.price);
        volume += trade.quantity;
        if trade.is_buyer_maker {
            sell_volume += trade.quantity;
        } else {
            buy_volume += trade.quantity;
        }
    }

    let timeframe = infer_timeframe(trades);

    Candle {
        symbol: first.symbol.clone(),
        timeframe,
        open_time: timeframe.bucket_of(first.timestamp),
        open: first.price,
        high,
        low,
        close: last.price,
        volume,
        buy_volume,
        sell_volume,
    }
}

/// Finest standard resolution that keeps every trade inside a single bucket.
///
/// Falls back to [`Timeframe::D1`] when the slice spans more than a day, which
/// means the caller handed us trades from more than one candle.
fn infer_timeframe(trades: &[Trade]) -> Timeframe {
    let Some(first) = trades.first() else {
        return Timeframe::M1;
    };

    let min_ts = trades
        .iter()
        .map(|t| t.timestamp)
        .min()
        .unwrap_or(first.timestamp);
    let max_ts = trades
        .iter()
        .map(|t| t.timestamp)
        .max()
        .unwrap_or(first.timestamp);

    Timeframe::all()
        .iter()
        .rev()
        .copied()
        .find(|tf| tf.bucket_of(min_ts) == tf.bucket_of(max_ts))
        .unwrap_or(Timeframe::D1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(price: f64, quantity: f64, buyer_maker: bool, timestamp: i64) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 0,
            price,
            quantity,
            is_buyer_maker: buyer_maker,
            timestamp,
        }
    }

    fn candle(open_time: i64, high: f64, low: f64, close: f64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high,
            low,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    #[test]
    fn cells_split_bid_and_ask_by_aggressor() {
        let trades = vec![
            trade(100.0, 4.0, false, 0), // buyer aggressed -> ask
            trade(100.0, 6.0, true, 0),  // seller aggressed -> bid
        ];
        let fp =
            build_footprint_for_candle(&candle(0, 100.0, 100.0, 100.0, 4.0, 6.0), &trades, 1.0);
        let cell = &fp.cells[0];
        assert!((cell.ask_volume - 4.0).abs() < 1e-9);
        assert!((cell.bid_volume - 6.0).abs() < 1e-9);
        assert!((cell.delta + 2.0).abs() < 1e-9);
    }

    #[test]
    fn cells_are_ascending_contiguous_and_keep_empty_levels() {
        let trades = vec![trade(100.0, 1.0, false, 0), trade(103.0, 1.0, false, 0)];
        let fp =
            build_footprint_for_candle(&candle(0, 103.0, 100.0, 103.0, 2.0, 0.0), &trades, 1.0);
        assert_eq!(fp.cells.len(), 4, "empty levels must survive");
        for pair in fp.cells.windows(2) {
            assert!((pair[1].price_level - pair[0].price_level - 1.0).abs() < 1e-9);
        }
        // The two middle levels traded nothing.
        assert!((fp.cells[1].total_volume()).abs() < 1e-9);
        assert!((fp.cells[2].total_volume()).abs() < 1e-9);
    }

    #[test]
    fn cell_volumes_sum_to_candle_volume() {
        let trades = vec![
            trade(100.0, 2.0, false, 0),
            trade(101.0, 3.0, true, 0),
            trade(102.0, 5.0, false, 0),
        ];
        let fp =
            build_footprint_for_candle(&candle(0, 102.0, 100.0, 102.0, 7.0, 3.0), &trades, 1.0);
        assert!((fp.total_volume() - 10.0).abs() < 1e-9);
        assert!((fp.delta() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn derived_candle_matches_the_trades() {
        let trades = vec![
            trade(100.0, 1.0, false, 0),
            trade(105.0, 2.0, true, 10),
            trade(99.0, 3.0, false, 20),
            trade(102.0, 4.0, true, 30),
        ];
        let fp = build_footprint(&trades, 1.0);
        assert_eq!(fp.candle.symbol, "BTCUSDT");
        assert!((fp.candle.open - 100.0).abs() < 1e-9);
        assert!((fp.candle.close - 102.0).abs() < 1e-9);
        assert!((fp.candle.high - 105.0).abs() < 1e-9);
        assert!((fp.candle.low - 99.0).abs() < 1e-9);
        assert!((fp.candle.volume - 10.0).abs() < 1e-9);
        assert!((fp.candle.buy_volume - 4.0).abs() < 1e-9);
        assert!((fp.candle.sell_volume - 6.0).abs() < 1e-9);
    }

    #[test]
    fn inferred_timeframe_is_the_finest_that_fits() {
        // 90 seconds of trades -> must be 5m, because 1m would split them.
        let trades = vec![
            trade(100.0, 1.0, false, 0),
            trade(101.0, 1.0, false, 90 * 1_000_000_000),
        ];
        let fp = build_footprint(&trades, 1.0);
        assert_eq!(fp.candle.timeframe, Timeframe::M5);
        assert_eq!(fp.candle.open_time, 0);
    }

    #[test]
    fn short_slice_infers_one_minute() {
        let trades = vec![
            trade(100.0, 1.0, false, 0),
            trade(101.0, 1.0, false, 30 * 1_000_000_000),
        ];
        assert_eq!(
            build_footprint(&trades, 1.0).candle.timeframe,
            Timeframe::M1
        );
    }

    #[test]
    fn build_footprints_splits_trades_per_candle() {
        let minute = Timeframe::M1.nanos();
        let candles = vec![
            candle(0, 101.0, 100.0, 101.0, 1.0, 0.0),
            candle(minute, 102.0, 101.0, 102.0, 1.0, 0.0),
        ];
        let trades = vec![
            trade(100.0, 1.0, false, 0),
            trade(101.0, 2.0, false, minute / 2),
            trade(102.0, 3.0, false, minute + 1),
        ];
        let footprints = build_footprints(&candles, &trades, 1.0);
        assert_eq!(footprints.len(), 2);
        assert!((footprints[0].total_volume() - 3.0).abs() < 1e-9);
        assert!((footprints[1].total_volume() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn empty_input_is_degenerate_but_does_not_panic() {
        let fp = build_footprint(&[], 1.0);
        assert!(fp.is_empty());
        assert!(fp.poc().is_none());
        assert!((fp.total_volume()).abs() < 1e-9);
    }

    #[test]
    fn invalid_bucket_size_yields_no_cells() {
        let trades = vec![trade(100.0, 1.0, false, 0)];
        let c = candle(0, 100.0, 100.0, 100.0, 1.0, 0.0);
        assert!(build_footprint_for_candle(&c, &trades, 0.0).is_empty());
        assert!(build_footprint_for_candle(&c, &trades, -1.0).is_empty());
        assert!(build_footprint_for_candle(&c, &trades, f64::NAN).is_empty());
    }

    #[test]
    fn absurd_bucket_count_is_refused() {
        let trades = vec![trade(1.0, 1.0, false, 0), trade(100_000.0, 1.0, false, 0)];
        assert!(build_footprint(&trades, 0.000_001).is_empty());
    }

    #[test]
    fn poc_lowest_and_highest_traded_levels() {
        let trades = vec![
            trade(100.0, 1.0, false, 0),
            trade(101.0, 9.0, false, 0),
            trade(102.0, 2.0, false, 0),
        ];
        let fp =
            build_footprint_for_candle(&candle(0, 102.0, 100.0, 102.0, 12.0, 0.0), &trades, 1.0);
        // Midpoints 100.5 / 101.5 / 102.5
        assert!((fp.poc().unwrap() - 101.5).abs() < 1e-9);
        assert!((fp.lowest_traded_level().unwrap() - 100.5).abs() < 1e-9);
        assert!((fp.highest_traded_level().unwrap() - 102.5).abs() < 1e-9);
    }

    #[test]
    fn candle_only_footprint_is_flat_and_reports_no_imbalance() {
        // A 3:1 bullish candle. Spread uniformly, every level carries the same
        // 3:1 ratio -- which is exactly the default imbalance threshold.
        let fp = build_footprint_from_candle(&candle(0, 102.0, 100.0, 101.0, 9.0, 3.0), 1.0);
        assert_eq!(fp.cells.len(), 3);
        for cell in &fp.cells {
            assert!(
                (cell.delta - 2.0).abs() < 1e-9,
                "uniform spread => flat delta"
            );
        }

        // The raw cells really would trip the detector...
        let would_trip = detect_imbalances(&fp, crate::imbalance::DEFAULT_IMBALANCE_RATIO);
        assert!(
            !would_trip.is_empty(),
            "the uniform spread does reproduce the 3:1 ratio at every level"
        );

        // ...which is precisely why the constructor suppresses them.
        assert!(
            fp.imbalances.is_empty(),
            "a candle-derived footprint must not fabricate signals"
        );
    }

    #[test]
    fn price_on_the_upper_edge_lands_in_the_last_bucket() {
        let trades = vec![trade(100.0, 1.0, false, 0), trade(102.0, 1.0, false, 0)];
        let fp =
            build_footprint_for_candle(&candle(0, 102.0, 100.0, 102.0, 2.0, 0.0), &trades, 1.0);
        // 102.0 / 1.0 -> index 2, exactly the last of 3 buckets.
        assert_eq!(fp.cells.len(), 3);
        assert!((fp.cells[2].total_volume() - 1.0).abs() < 1e-9);
    }
}
