//! Aggregate candles from a finer resolution to a coarser one.
//!
//! ## Why this is exact
//!
//! OHLCV aggregation is lossless in one direction and only one. Summing volume
//! and taking first-open / max-high / min-low / last-close over a bucket
//! reproduces exactly the candle that would have been built had the trades been
//! bucketed at the coarser resolution in the first place -- **provided the finer
//! candles were themselves built from the trade stream**, which
//! `docs/04-MARKET-DATA-ENGINE.md` requires. In particular `buy_volume` and
//! `sell_volume` are sums, so the coarser candle's delta and CVD stay consistent
//! with the finer one's, which is the property that makes this safe to use
//! instead of re-ingesting trades at every resolution.
//!
//! The reverse is not true and is not offered: no amount of arithmetic recovers
//! a 1m candle from a 1h one.
//!
//! ## Ordering
//!
//! Input is expected ascending by `open_time`, but the implementation keys by
//! bucket and tracks the earliest and latest candle *by timestamp* within each
//! bucket, so out-of-order input still produces a correct result rather than a
//! silently wrong open or close.

use std::collections::BTreeMap;

use crate::types::{Candle, Timeframe};

/// One bucket being accumulated.
#[derive(Debug, Clone)]
struct Bucket {
    /// Candle with the earliest `open_time` seen in this bucket.
    first: Candle,
    /// Candle with the latest `open_time` seen in this bucket.
    last: Candle,
    /// Highest high seen.
    high: f64,
    /// Lowest low seen.
    low: f64,
    /// Summed volume.
    volume: f64,
    /// Summed buyer-aggressed volume.
    buy_volume: f64,
    /// Summed seller-aggressed volume.
    sell_volume: f64,
}

impl Bucket {
    fn new(candle: &Candle) -> Self {
        Self {
            first: candle.clone(),
            last: candle.clone(),
            high: candle.high,
            low: candle.low,
            volume: candle.volume,
            buy_volume: candle.buy_volume,
            sell_volume: candle.sell_volume,
        }
    }

    fn push(&mut self, candle: &Candle) {
        if candle.open_time < self.first.open_time {
            self.first = candle.clone();
        }
        if candle.open_time > self.last.open_time {
            self.last = candle.clone();
        }
        self.high = self.high.max(candle.high);
        self.low = self.low.min(candle.low);
        self.volume += candle.volume;
        self.buy_volume += candle.buy_volume;
        self.sell_volume += candle.sell_volume;
    }

    fn finish(self, target: Timeframe, bucket: i64) -> Candle {
        Candle {
            symbol: self.first.symbol,
            timeframe: target,
            open_time: bucket,
            open: self.first.open,
            high: self.high,
            low: self.low,
            close: self.last.close,
            volume: self.volume,
            buy_volume: self.buy_volume,
            sell_volume: self.sell_volume,
        }
    }
}

/// Aggregate `candles` into `target` resolution, ascending by `open_time`.
///
/// Returns a copy of the input when `target` is **not coarser** than the
/// candles already are. Aggregating cannot add resolution, and returning the
/// input unchanged is the honest answer to "give me 1m candles from these 1m
/// candles" -- inventing anything else would be a lie about the data.
///
/// The source resolution is taken from the first candle's `timeframe`. An empty
/// input yields an empty output.
#[must_use]
pub fn resample(candles: &[Candle], target: Timeframe) -> Vec<Candle> {
    let Some(first) = candles.first() else {
        return Vec::new();
    };

    if target.nanos() <= first.timeframe.nanos() {
        return candles.to_vec();
    }

    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    for candle in candles {
        let bucket = target.bucket_of(candle.open_time);
        match buckets.get_mut(&bucket) {
            Some(existing) => existing.push(candle),
            None => {
                buckets.insert(bucket, Bucket::new(candle));
            }
        }
    }

    buckets
        .into_iter()
        .map(|(bucket, accumulated)| accumulated.finish(target, bucket))
        .collect()
}

/// Aggregate several resolutions from one source series in a single pass per
/// resolution.
///
/// Convenience for loading a multi-timeframe strategy from a single fine-grained
/// backfill.
#[must_use]
pub fn resample_all(candles: &[Candle], targets: &[Timeframe]) -> Vec<(Timeframe, Vec<Candle>)> {
    targets
        .iter()
        .map(|target| (*target, resample(candles, *target)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NS_PER_SEC;

    const MIN: i64 = 60 * NS_PER_SEC;

    fn candle(
        open_time: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        buy: f64,
        sell: f64,
    ) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open,
            high,
            low,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    /// Five consecutive minutes with distinguishable values.
    fn five_minutes() -> Vec<Candle> {
        vec![
            candle(0, 100.0, 105.0, 99.0, 104.0, 3.0, 1.0),
            candle(MIN, 104.0, 108.0, 103.0, 107.0, 2.0, 2.0),
            candle(2 * MIN, 107.0, 109.0, 101.0, 102.0, 1.0, 4.0),
            candle(3 * MIN, 102.0, 103.0, 95.0, 96.0, 1.0, 5.0),
            candle(4 * MIN, 96.0, 101.0, 96.0, 100.0, 6.0, 1.0),
        ]
    }

    #[test]
    fn an_empty_input_yields_an_empty_output() {
        assert!(resample(&[], Timeframe::M5).is_empty());
    }

    #[test]
    fn five_minutes_become_one_candle_with_the_right_ohlc() {
        let out = resample(&five_minutes(), Timeframe::M5);
        assert_eq!(out.len(), 1);

        let candle = &out[0];
        assert_eq!(candle.timeframe, Timeframe::M5);
        assert_eq!(candle.open_time, 0);
        assert!((candle.open - 100.0).abs() < 1e-9, "open is the first open");
        assert!(
            (candle.close - 100.0).abs() < 1e-9,
            "close is the last close"
        );
        assert!((candle.high - 109.0).abs() < 1e-9, "high is the max high");
        assert!((candle.low - 95.0).abs() < 1e-9, "low is the min low");
    }

    #[test]
    fn volume_and_the_buy_sell_split_are_preserved_exactly() {
        let source = five_minutes();
        let out = resample(&source, Timeframe::M5);

        let buy: f64 = source.iter().map(|c| c.buy_volume).sum();
        let sell: f64 = source.iter().map(|c| c.sell_volume).sum();
        let volume: f64 = source.iter().map(|c| c.volume).sum();

        assert!((out[0].buy_volume - buy).abs() < 1e-9);
        assert!((out[0].sell_volume - sell).abs() < 1e-9);
        assert!((out[0].volume - volume).abs() < 1e-9);
    }

    #[test]
    fn delta_survives_aggregation() {
        // The whole point: a 5m candle's delta must equal the sum of the 1m
        // deltas, or CVD built on top of it would disagree with the source.
        let source = five_minutes();
        let expected: f64 = source.iter().map(Candle::delta).sum();
        let out = resample(&source, Timeframe::M5);
        assert!((out[0].delta() - expected).abs() < 1e-9);
    }

    #[test]
    fn buckets_align_to_the_target_boundary_not_the_first_candle() {
        // Starting at minute 3, a 5m bucket still begins at minute 0.
        let source: Vec<Candle> = (3..8)
            .map(|i| candle(i * MIN, 100.0, 101.0, 99.0, 100.5, 1.0, 1.0))
            .collect();
        let out = resample(&source, Timeframe::M5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open_time, 0);
        assert_eq!(out[1].open_time, 5 * MIN);
    }

    #[test]
    fn a_coarser_target_never_gains_resolution() {
        // Asking for 1m from 1m is a copy, not an invention.
        let source = five_minutes();
        let out = resample(&source, Timeframe::M1);
        assert_eq!(out.len(), source.len());
        assert_eq!(out[0].open_time, source[0].open_time);

        // And asking for something finer than the source is refused the same way.
        let hourly = vec![Candle {
            timeframe: Timeframe::H1,
            ..five_minutes()[0].clone()
        }];
        let out = resample(&hourly, Timeframe::M5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].timeframe, Timeframe::H1);
    }

    #[test]
    fn resampling_is_idempotent() {
        let once = resample(&five_minutes(), Timeframe::M5);
        let twice = resample(&once, Timeframe::M5);
        assert_eq!(once, twice);
    }

    #[test]
    fn out_of_order_input_still_picks_the_right_open_and_close() {
        // Shuffled: the open must come from the earliest timestamp, not the
        // first one encountered.
        let mut source = five_minutes();
        source.reverse();
        let out = resample(&source, Timeframe::M5);
        assert!((out[0].open - 100.0).abs() < 1e-9);
        assert!((out[0].close - 100.0).abs() < 1e-9);
        assert!((out[0].high - 109.0).abs() < 1e-9);
        assert!((out[0].low - 95.0).abs() < 1e-9);
    }

    #[test]
    fn resample_all_produces_each_requested_resolution() {
        let source: Vec<Candle> = (0..120)
            .map(|i| candle(i * MIN, 100.0, 101.0, 99.0, 100.5, 1.0, 1.0))
            .collect();
        let out = resample_all(&source, &[Timeframe::M5, Timeframe::H1]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].1.len(), 24, "120 minutes is 24 five-minute candles");
        assert_eq!(out[1].1.len(), 2, "120 minutes is 2 hourly candles");
    }

    #[test]
    fn a_gap_in_the_source_does_not_invent_a_candle() {
        // Minutes 0 and 1, then a jump to minute 7. The 5m bucket for minute 5
        // has one candle; the bucket for minute 0 has two. No bucket is created
        // for the minutes that never traded.
        let source = vec![
            candle(0, 100.0, 101.0, 99.0, 100.0, 1.0, 1.0),
            candle(MIN, 100.0, 102.0, 99.0, 101.0, 1.0, 1.0),
            candle(7 * MIN, 101.0, 103.0, 100.0, 102.0, 1.0, 1.0),
        ];
        let out = resample(&source, Timeframe::M5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open_time, 0);
        assert_eq!(out[1].open_time, 5 * MIN);
    }
}
