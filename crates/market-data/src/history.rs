//! In-memory candle history -- what a chart draws, and the only place it lives.
//!
//! ## Why this exists
//!
//! Storing market data costs disk, and disk is the one thing this platform does
//! not have: the database is a free tier with 6 GB **total**, and the plan is
//! many symbols across several markets (crypto now, forex later). One symbol's
//! trade stream is roughly 110 MB/day, so five symbols fill 6 GB in about
//! eleven days. That arithmetic is the whole reason for this module.
//!
//! So the model is:
//!
//! * **Recent bars live in RAM**, fed by the live trade stream. They are never
//!   written anywhere. A restart loses them -- which costs a REST fetch, not
//!   data.
//! * **Older bars are fetched from the venue on demand**, via
//!   [`BackfillClient`](crate::backfill::BackfillClient), the first time a
//!   chart asks for a window older than the buffer. See
//!   [`crate::history::HistoryRegistry::older_than`] for the boundary.
//!
//! RAM cost is the thing to watch, so it is stated up front: a `Candle` is
//! about 120 bytes, so the default 1500-bar buffer is ~180 KB per series,
//! ~1 MB per symbol across the six standard resolutions, and ~60 MB for sixty
//! symbols. That is two orders of magnitude cheaper than the same history on
//! disk, and it is bounded -- a new symbol costs RAM, not unstoppable growth.
//!
//! ## Two kinds of bar
//!
//! The collector publishes **closed** candles only
//! ([`MarketEventBus::publish_candle`](crate::bus::MarketEventBus::publish_candle)),
//! so the bar that is forming right now is not on the bus. A chart that opens
//! and never receives a tick would show a last bar that is up to one whole
//! resolution stale -- on `1d`, a day old. So this buffer keeps the forming bar
//! separately, and hands it out as the newest bar of the series.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use analytics_core::{Candle, Timeframe};

/// Bars kept per series by default.
///
/// 1500 `1m` bars is 25 hours -- more than a trading day, so a chart opening
/// cold on the most common resolution can be served entirely from RAM with no
/// REST call at all. On `1d` it is four years, which the venue will not even
/// return in one go, so the higher resolutions are bounded by history, not by
/// this number.
pub const DEFAULT_HISTORY_BARS: usize = 1500;

/// One series: the closed bars plus the bar being built right now.
///
/// All methods take `&self`; sharing a series across tasks is `Arc` and needs
/// no exterior lock.
#[derive(Debug)]
pub struct CandleHistory {
    capacity: usize,
    bars: RwLock<VecDeque<Candle>>,
    forming: RwLock<Option<Candle>>,
}

impl CandleHistory {
    /// An empty series holding at most `capacity` closed bars.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            bars: RwLock::new(VecDeque::new()),
            forming: RwLock::new(None),
        }
    }

    /// An empty series with [`DEFAULT_HISTORY_BARS`].
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_HISTORY_BARS)
    }

    /// How many closed bars this series holds on to.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record a closed bar.
    ///
    /// A bar whose `open_time` matches the newest one **replaces** it rather
    /// than being appended: venues restate the last bar of a series, and two
    /// entries for one bucket would make the chart draw a duplicate. A bar
    /// older than the newest is dropped -- it is a late restatement of a bucket
    /// this buffer has already rolled past, and inserting it would put the
    /// series out of order.
    pub fn push(&self, candle: Candle) {
        let mut bars = match self.bars.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        match bars.back() {
            Some(last) if last.open_time == candle.open_time => {
                *bars.back_mut().expect("checked above") = candle;
                return;
            }
            Some(last) if candle.open_time < last.open_time => return,
            _ => {}
        }

        bars.push_back(candle);
        while bars.len() > self.capacity {
            bars.pop_front();
        }
    }

    /// Record the bar currently being built.
    ///
    /// Replaces whatever forming bar was there: there is exactly one in-flight
    /// bar per series, and a stale one left behind would be drawn as if it were
    /// current.
    pub fn set_forming(&self, candle: Candle) {
        if let Ok(mut guard) = self.forming.write() {
            *guard = Some(candle);
        }
    }

    /// Drop the forming bar.
    ///
    /// Called when it closes and becomes a real bar, so it is not returned
    /// twice -- once as forming and once as closed.
    pub fn clear_forming(&self) {
        if let Ok(mut guard) = self.forming.write() {
            *guard = None;
        }
    }

    /// Every bar in `[from_ns, to_ns)`, oldest first, including the forming bar
    /// when its bucket falls inside the window.
    #[must_use]
    pub fn range(&self, from_ns: i64, to_ns: i64) -> Vec<Candle> {
        let bars = match self.bars.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let forming = self.forming.read().ok().and_then(|g| g.clone());

        let mut out: Vec<Candle> = bars
            .iter()
            .filter(|c| c.open_time >= from_ns && c.open_time < to_ns)
            .cloned()
            .collect();

        // The forming bar is only worth returning when it is newer than every
        // closed bar. If it is not, the close already landed and `clear_forming`
        // simply has not run yet.
        let newest_closed = out.last().map(|c| c.open_time);
        if let Some(candle) = forming {
            let in_window = candle.open_time >= from_ns && candle.open_time < to_ns;
            let is_newer = newest_closed.is_none_or(|newest| candle.open_time > newest);
            if in_window && is_newer {
                out.push(candle);
            }
        }

        out
    }

    /// The `limit` most recent bars, oldest first.
    ///
    /// Counts the forming bar as the newest, because that is what a chart means
    /// by "the last 500 candles".
    #[must_use]
    pub fn latest(&self, limit: usize) -> Vec<Candle> {
        let bars = match self.bars.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let forming = self.forming.read().ok().and_then(|g| g.clone());

        let start = bars.len().saturating_sub(limit);
        let mut out: Vec<Candle> = bars.iter().skip(start).cloned().collect();

        let newest_closed = out.last().map(|c| c.open_time);
        if let Some(candle) = forming {
            if newest_closed.is_none_or(|newest| candle.open_time > newest) {
                out.push(candle);
                if out.len() > limit {
                    out.remove(0);
                }
            }
        }

        out
    }

    /// The in-progress bar, if one has been recorded.
    ///
    /// Distinct from "the newest bar" on purpose. Anything reporting whether a
    /// series ends on a closed candle or a bar still being built needs to ask
    /// this, because a forming bar and a closed one are the same shape and
    /// differ only in whether more trades can still land in the bucket.
    #[must_use]
    pub fn forming(&self) -> Option<Candle> {
        self.forming.read().ok().and_then(|guard| guard.clone())
    }

    /// Open time of the oldest closed bar, nanoseconds.
    #[must_use]
    pub fn earliest(&self) -> Option<i64> {
        self.bars
            .read()
            .ok()
            .and_then(|bars| bars.front().map(|c| c.open_time))
    }

    /// Open time of the newest bar, counting the forming one.
    #[must_use]
    pub fn newest(&self) -> Option<i64> {
        let forming = self
            .forming
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.open_time));
        let closed = self
            .bars
            .read()
            .ok()
            .and_then(|bars| bars.back().map(|c| c.open_time));

        match (forming, closed) {
            (Some(forming), Some(closed)) => Some(forming.max(closed)),
            (Some(forming), None) => Some(forming),
            (None, closed) => closed,
        }
    }

    /// How many closed bars are buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bars.read().map(|bars| bars.len()).unwrap_or(0)
    }

    /// Whether the series holds no closed bars at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Which symbol and resolution a series is for.
type SeriesKey = (String, Timeframe);

/// One [`CandleHistory`] per (symbol, resolution), created on demand.
///
/// This is the chart's entire recent history. It is deliberately **not**
/// persisted: it is a cache with a bounded RAM cost, and cold-starting it from
/// the venue REST API is a request, not an outage.
#[derive(Debug, Default)]
pub struct HistoryRegistry {
    series: RwLock<HashMap<SeriesKey, Arc<CandleHistory>>>,
}

impl HistoryRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The series for `symbol`/`timeframe`, created on first access.
    #[must_use]
    pub fn series(&self, symbol: &str, timeframe: Timeframe) -> Arc<CandleHistory> {
        let key = (symbol.to_uppercase(), timeframe);
        if let Some(found) = self.series.read().ok().and_then(|m| m.get(&key).cloned()) {
            return found;
        }

        let mut map = match self.series.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(key)
            .or_insert_with(|| Arc::new(CandleHistory::with_default_capacity()))
            .clone()
    }

    /// Record a closed bar into its series.
    pub fn record_closed(&self, candle: &Candle) {
        let series = self.series(&candle.symbol, candle.timeframe);
        series.push(candle.clone());
        // The bar that just closed *is* the forming bar that was there a moment
        // ago; leaving it would return the same bucket twice.
        series.clear_forming();
    }

    /// Record an in-progress bar into its series.
    pub fn record_forming(&self, candle: &Candle) {
        self.series(&candle.symbol, candle.timeframe)
            .set_forming(candle.clone());
    }

    /// The oldest bar buffered for `symbol`/`timeframe`.
    ///
    /// A window reaching further back than this has to come from the venue.
    #[must_use]
    pub fn oldest_buffered_ns(&self, symbol: &str, timeframe: Timeframe) -> Option<i64> {
        self.series(symbol, timeframe).earliest()
    }

    /// Open time of the newest bar buffered, counting the forming one.
    ///
    /// What "the most recent `limit` bars" is measured from. Without the
    /// forming bar this would lag by up to one whole resolution, and on `1d`
    /// that is a day of chart missing off the right-hand edge.
    #[must_use]
    pub fn newest(&self, symbol: &str, timeframe: Timeframe) -> Option<i64> {
        self.series(symbol, timeframe).newest()
    }

    /// Every symbol with at least one buffered series.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .series
            .read()
            .map(|map| map.keys().map(|(symbol, _)| symbol.clone()).collect())
            .unwrap_or_default();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// How many series are being tracked.
    #[must_use]
    pub fn series_count(&self) -> usize {
        self.series.read().map(|map| map.len()).unwrap_or(0)
    }

    /// Which resolutions are being tracked for `symbol`.
    ///
    /// Only the ones that exist -- asking for a resolution nobody has asked for
    /// yet must not conjure an empty series into being, or `symbols()` would
    /// start reporting symbols that have no data at all.
    #[must_use]
    pub fn timeframes(&self, symbol: &str) -> Vec<Timeframe> {
        let symbol = symbol.to_uppercase();
        let mut out: Vec<Timeframe> = self
            .series
            .read()
            .map(|map| {
                map.keys()
                    .filter(|(s, _)| *s == symbol)
                    .map(|(_, timeframe)| *timeframe)
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|timeframe| timeframe.nanos());
        out
    }

    /// `(symbol, timeframe, buffered bars)` for every series, sorted.
    ///
    /// For the `market_data_history_bars` gauge. Sorted so a scrape is stable
    /// and a test can assert on order.
    #[must_use]
    pub fn depth(&self) -> Vec<(String, Timeframe, usize)> {
        let mut out: Vec<(String, Timeframe, usize)> = self
            .series
            .read()
            .map(|map| {
                map.iter()
                    .map(|((symbol, timeframe), series)| (symbol.clone(), *timeframe, series.len()))
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| (&a.0, a.1.to_string()).cmp(&(&b.0, b.1.to_string())));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(open_time: i64, timeframe: Timeframe) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe,
            open_time,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        }
    }

    const MINUTE: i64 = 60_000_000_000;

    #[test]
    fn a_restated_bar_replaces_rather_than_duplicating() {
        let history = CandleHistory::new(16);
        history.push(bar(MINUTE, Timeframe::M1));
        let mut restated = bar(MINUTE, Timeframe::M1);
        restated.close = 9.0;
        history.push(restated);

        assert_eq!(history.len(), 1);
        assert_eq!(history.range(0, MINUTE * 2)[0].close, 9.0);
    }

    #[test]
    fn a_late_bar_older_than_the_newest_is_dropped() {
        let history = CandleHistory::new(16);
        history.push(bar(MINUTE * 3, Timeframe::M1));
        history.push(bar(MINUTE, Timeframe::M1));

        assert_eq!(history.len(), 1);
        assert_eq!(history.earliest(), Some(MINUTE * 3));
    }

    #[test]
    fn the_buffer_keeps_only_its_capacity_oldest_first() {
        let history = CandleHistory::new(3);
        for i in 1..=5 {
            history.push(bar(MINUTE * i, Timeframe::M1));
        }

        assert_eq!(history.len(), 3);
        assert_eq!(history.earliest(), Some(MINUTE * 3));
    }

    #[test]
    fn the_forming_bar_is_returned_when_it_is_newer_than_every_closed_bar() {
        let history = CandleHistory::new(16);
        history.push(bar(MINUTE, Timeframe::M1));
        history.set_forming(bar(MINUTE * 2, Timeframe::M1));

        let out = history.range(0, MINUTE * 3);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].open_time, MINUTE * 2);
    }

    #[test]
    fn a_forming_bar_that_has_already_closed_is_not_returned_twice() {
        let history = CandleHistory::new(16);
        history.push(bar(MINUTE, Timeframe::M1));
        history.set_forming(bar(MINUTE, Timeframe::M1));

        assert_eq!(history.range(0, MINUTE * 2).len(), 1);
    }

    #[test]
    fn clearing_the_forming_bar_removes_it_from_the_series() {
        let history = CandleHistory::new(16);
        history.set_forming(bar(MINUTE, Timeframe::M1));
        assert_eq!(history.range(0, MINUTE * 2).len(), 1);

        history.clear_forming();
        assert!(history.range(0, MINUTE * 2).is_empty());
    }

    #[test]
    fn latest_returns_the_newest_bars_and_counts_the_forming_one() {
        let history = CandleHistory::new(16);
        for i in 1..=4 {
            history.push(bar(MINUTE * i, Timeframe::M1));
        }
        history.set_forming(bar(MINUTE * 5, Timeframe::M1));

        let out = history.latest(2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open_time, MINUTE * 4);
        assert_eq!(out[1].open_time, MINUTE * 5);
    }

    #[test]
    fn newest_prefers_the_forming_bar_over_the_last_closed_one() {
        let history = CandleHistory::new(16);
        history.push(bar(MINUTE, Timeframe::M1));
        assert_eq!(history.newest(), Some(MINUTE));

        history.set_forming(bar(MINUTE * 2, Timeframe::M1));
        assert_eq!(history.newest(), Some(MINUTE * 2));
    }

    #[test]
    fn an_empty_series_has_no_bounds() {
        let history = CandleHistory::with_default_capacity();
        assert!(history.is_empty());
        assert_eq!(history.earliest(), None);
        assert_eq!(history.newest(), None);
        assert_eq!(history.capacity(), DEFAULT_HISTORY_BARS);
    }

    #[test]
    fn the_registry_returns_one_series_per_symbol_and_resolution() {
        let registry = HistoryRegistry::new();
        let a = registry.series("btcusdt", Timeframe::M1);
        let again = registry.series("BTCUSDT", Timeframe::M1);
        let five = registry.series("BTCUSDT", Timeframe::M5);

        assert!(Arc::ptr_eq(&a, &again));
        assert!(!Arc::ptr_eq(&a, &five));
        assert_eq!(registry.series_count(), 2);
    }

    #[test]
    fn recording_a_closed_bar_clears_the_forming_one() {
        let registry = HistoryRegistry::new();
        registry.record_forming(&bar(MINUTE, Timeframe::M1));
        registry.record_closed(&bar(MINUTE, Timeframe::M1));

        let series = registry.series("BTCUSDT", Timeframe::M1);
        assert_eq!(series.len(), 1);
        assert_eq!(series.range(0, MINUTE * 2).len(), 1);
    }

    #[test]
    fn the_registry_lists_each_symbol_once_sorted() {
        let registry = HistoryRegistry::new();
        registry.record_closed(&bar(MINUTE, Timeframe::M1));
        registry.record_closed(&bar(MINUTE, Timeframe::M5));
        registry.record_closed(&Candle {
            symbol: "ETHUSDT".into(),
            ..bar(MINUTE, Timeframe::M1)
        });

        assert_eq!(registry.symbols(), vec!["BTCUSDT", "ETHUSDT"]);
    }

    #[test]
    fn oldest_buffered_says_how_far_back_ram_can_answer() {
        let registry = HistoryRegistry::new();
        assert_eq!(registry.oldest_buffered_ns("BTCUSDT", Timeframe::M1), None);

        registry.record_closed(&bar(MINUTE * 5, Timeframe::M1));
        registry.record_closed(&bar(MINUTE * 6, Timeframe::M1));
        assert_eq!(
            registry.oldest_buffered_ns("BTCUSDT", Timeframe::M1),
            Some(MINUTE * 5)
        );
    }
}
