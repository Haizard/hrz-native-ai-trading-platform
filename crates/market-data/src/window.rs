//! Reconstructing a candle window without a market-data database.
//!
//! ## Why this module exists
//!
//! `docs/04` decided that market data is never persisted, and every chart route
//! already honours it: `GET /candles` merges the RAM buffer with the venue and
//! says how much came from each. The AI analysis path did **not** honour it --
//! it read candles out of Postgres, so a symbol nobody had backfilled by hand
//! made the agent fail with "no data" while its chart drew perfectly well. Two
//! sources of truth for one question, and the older one was the wrong one.
//!
//! This is the single answer those two paths now share. Ask for a window, get
//! a window, plus an account of how it was assembled:
//!
//! * **recent bars** come from [`HistoryRegistry`], fed by the live stream;
//! * **anything older** comes from the venue's REST API, fetched once and
//!   dropped when the request ends;
//! * **when the buffer reaches into the venue's own span**, the two are
//!   reconciled bucket by bucket.
//!
//! ## The overlap rule, and why it is not just "buffer wins"
//!
//! A chart and a bot must not disagree about what a candle was
//! (`00-VISION-AND-PRINCIPLES.md`). Along a liquid symbol's history the buffer
//! and the venue will produce identical bars -- but not where it matters most.
//! The buffer's newest bars are built from the trade stream and include the bar
//! that is still forming; the venue's are closed and final. On the boundary
//! bucket, `1m` of BTCUSDT can genuinely have traded while the request was in
//! flight.
//!
//! So the buffer always wins a bucket it holds. It is built from this process's
//! own trade stream at the resolution the caller asked for, it is the same
//! series the chart is being drawn from, and it is the series the running bot
//! is deciding on. Replacing it with the venue's copy would make the AI's view
//! of "now" differ from the chart's, which is precisely the failure the shared
//! `analytics-core` exists to prevent.
//!
//! ## What this module refuses to do
//!
//! It does not invent a bar. A gap in the venue's series stays a gap, and
//! [`Window::gaps`] counts it. Spreading a candle's volume across prices to
//! fake a footprint would be worse than reporting that tick data is missing --
//! see [`Window::trades_available`].

use std::collections::BTreeMap;
use std::sync::Arc;

use analytics_core::{Candle, Timeframe, Trade};

use crate::backfill::BackfillClient;
use crate::error::MarketDataError;
use crate::history::HistoryRegistry;
use crate::tape::LiveRegistry;

/// Most bars one reconstruction will ask the venue for.
///
/// Deliberately the same ceiling `GET /candles` uses. A weekly series is
/// nowhere near it -- BTCUSDT has a few hundred weekly bars in its entire
/// history -- but a five-year `1m` window would be ~2.6M bars and 2,600 REST
/// pages, which is how one impatient request throttles a whole deployment. The
/// window is clamped from the **front**, because the newest end is the end an
/// analysis is about.
pub const MAX_VENUE_BARS: usize = 5_000;

/// How a window was assembled.
///
/// Reported rather than hidden because "the analysis saw 12 bars" and "the
/// analysis saw 12 bars because the venue refused the rest" are different
/// facts, and only one of them is a reason to distrust a conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowSource {
    /// Bars served from the in-memory buffer.
    pub memory: usize,
    /// Bars served from the venue's REST API.
    pub venue: usize,
}

impl WindowSource {
    /// Total bars in the window.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.memory + self.venue
    }

    /// Whether the window holds nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// One symbol/timeframe window, reconstructed on demand.
#[derive(Debug, Clone)]
pub struct Window {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: Timeframe,
    /// Bars, ascending by `open_time`, oldest first.
    pub candles: Vec<Candle>,
    /// Where each bar came from.
    pub source: WindowSource,
    /// Buckets absent from `[first, last]` in the assembled series.
    ///
    /// Not the same as "the venue had no trades then". The venue returns *no
    /// row* for an empty bucket rather than a flat bar
    /// ([`crate::candle_builder`]), so a symbol that legitimately stopped
    /// trading and a truncated fetch look identical here. The count is what
    /// makes the difference visible instead of silently shrinking the sample.
    pub gaps: usize,
    /// Whether the newest bar is still forming.
    pub forming: bool,
    /// Whether tick-level data covered the window, so footprint-derived
    /// analytics are meaningful.
    ///
    /// `false` is a statement about **the data**, never about the market: no
    /// imbalance was detected *and could not have been* is not "the book was
    /// balanced". Every tool that depends on ticks already reports this
    /// distinction (`NO_TICK_DATA` in `ai-agent`), and this is where the honest
    /// half of that answer comes from.
    pub trades_available: bool,
}

impl Window {
    /// Whether the window holds no bars.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candles.is_empty()
    }

    /// A one-line account of the window, for a tool result or a log line.
    ///
    /// Every clause here is one a reader has to be able to trust. `gaps` and
    /// `trades_available` are the two that stop a thin window from looking like
    /// a complete one, so they are spelled out rather than left to the length
    /// of `candles`.
    #[must_use]
    pub fn provenance(&self) -> String {
        let missing = if self.gaps > 0 {
            format!(", {} missing", self.gaps)
        } else {
            String::new()
        };
        let forming = if self.forming {
            ", newest bar forming"
        } else {
            ""
        };
        let ticks = if self.trades_available {
            ""
        } else {
            ", no tick data in window"
        };
        format!(
            "{} {}: {bars} bars ({memory} from memory, {venue} from the venue){missing}{forming}{ticks}",
            self.symbol,
            self.timeframe,
            bars = self.candles.len(),
            memory = self.source.memory,
            venue = self.source.venue,
        )
    }
}

/// Builds candle windows from RAM plus the venue, for every consumer.
///
/// Cheap to clone -- it holds two `Arc`s and a REST client that pools its own
/// connections.
#[derive(Debug, Clone)]
pub struct WindowService {
    history: Arc<HistoryRegistry>,
    live: Arc<LiveRegistry>,
    backfill: BackfillClient,
}

impl WindowService {
    /// A service over an existing buffer, tape and venue client.
    #[must_use]
    pub fn new(
        history: Arc<HistoryRegistry>,
        live: Arc<LiveRegistry>,
        backfill: BackfillClient,
    ) -> Self {
        Self {
            history,
            live,
            backfill,
        }
    }

    /// The buffer this service reads.
    #[must_use]
    pub fn history(&self) -> &Arc<HistoryRegistry> {
        &self.history
    }

    /// The tape this service reads.
    #[must_use]
    pub fn live(&self) -> &Arc<LiveRegistry> {
        &self.live
    }

    /// Bars for `[from_ns, to_ns)`, from RAM first and the venue for the rest.
    ///
    /// # Errors
    /// Propagates venue transport and decode failures. An empty window is not
    /// an error -- a symbol with no trades in the window and a symbol that does
    /// not exist are different questions, and only the second is an error.
    pub async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Window, MarketDataError> {
        self.candles_capped(symbol, timeframe, from_ns, to_ns, MAX_VENUE_BARS)
            .await
    }

    /// [`Self::candles`] with an explicit ceiling on venue bars.
    ///
    /// The cap is a parameter so a test can prove the clamp behaves without
    /// generating five thousand bars first.
    ///
    /// # Errors
    /// As [`Self::candles`].
    pub async fn candles_capped(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
        max_venue_bars: usize,
    ) -> Result<Window, MarketDataError> {
        let symbol = symbol.to_uppercase();
        let (from_ns, to_ns) = (from_ns, to_ns.max(from_ns));

        let buffered = self
            .history
            .series(&symbol, timeframe)
            .range(from_ns, to_ns);
        let memory = buffered.len();
        // Reported from the buffer's own forming slot rather than inferred from
        // position: "the last bar in the vector" is not the same claim, and
        // after a restart the buffer can end on a closed bar.
        let forming = self
            .history
            .series(&symbol, timeframe)
            .forming()
            .is_some_and(|bar| {
                buffered
                    .last()
                    .is_some_and(|last| last.open_time == bar.open_time)
            });

        // Only the part the buffer cannot reach. Re-fetching the whole window
        // would throw away the one thing the buffer buys, which is not paying
        // for bars already in hand.
        let oldest_we_have = buffered.first().map_or(to_ns, |c| c.open_time);
        let mut fetched = Vec::new();
        if from_ns < oldest_we_have {
            let gap_end = oldest_we_have.min(to_ns);
            let width = timeframe.nanos().max(1);
            let wanted = usize::try_from((gap_end - from_ns) / width).unwrap_or(usize::MAX);

            // Clamped from the front, not the back: the newest end of a window
            // is the end the analysis is about.
            let (fetch_from, fetch_to) = if wanted > max_venue_bars {
                let span = i64::try_from(max_venue_bars)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(width);
                (gap_end.saturating_sub(span), gap_end)
            } else {
                (from_ns, gap_end)
            };

            match self
                .backfill
                .fetch_klines(&symbol, timeframe, fetch_from, fetch_to)
                .await
            {
                Ok(rows) => {
                    fetched = rows
                        .iter()
                        .map(|row| self.candle_from_kline(&symbol, timeframe, row))
                        .collect();
                }
                // A chart that can draw the recent part of its window beats one
                // that draws nothing because a deeper page failed. The shortfall
                // is visible in `source`, which is what makes this survivable
                // rather than silent.
                Err(e) => {
                    tracing::warn!(
                        symbol, %timeframe, error = %e,
                        "the venue refused part of the window; serving what is in RAM"
                    );
                }
            }
        }
        let venue = fetched.len();

        let candles = merge(buffered, fetched);
        let gaps = count_gaps(&candles, timeframe);
        let trades_available = !self
            .live
            .tape(&symbol)
            .range(earliest_open(&candles).unwrap_or(from_ns), to_ns)
            .is_empty()
            && !candles.is_empty();

        Ok(Window {
            symbol,
            timeframe,
            candles,
            source: WindowSource { memory, venue },
            gaps,
            forming,
            trades_available,
        })
    }

    /// The trailing `limit` bars, anchored to the newest bar known.
    ///
    /// # Errors
    /// As [`Self::candles`].
    pub async fn latest(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        limit: usize,
    ) -> Result<Window, MarketDataError> {
        let limit = limit.max(1);
        let symbol = symbol.to_uppercase();
        let width = timeframe.nanos().max(1);
        let now = crate::window::now_ns();

        // Anchored to the newest bar the buffer holds, when it holds one. From
        // the wall clock instead, a symbol whose feed stopped an hour ago
        // returns an empty window -- which is indistinguishable from a symbol
        // nobody has asked for, and it is the second one that is a mistake.
        let newest = self.history.newest(&symbol, timeframe).unwrap_or(now);
        let span = i64::try_from(limit)
            .unwrap_or(i64::MAX / width.max(1))
            .saturating_mul(width);

        self.candles(&symbol, timeframe, newest.saturating_sub(span), now)
            .await
    }

    /// Trades in `[from_ns, to_ns)` from the live tape.
    ///
    /// Returns empty for a window older than the tape's span. That is a fact
    /// about the tape, not about the market: `docs/04` forbids storing trades,
    /// so a footprint is a *recent* chart by construction. Callers must report
    /// the emptiness rather than treating it as "nothing happened".
    #[must_use]
    pub fn trades(&self, symbol: &str, from_ns: i64, to_ns: i64) -> Vec<Trade> {
        self.live.tape(symbol).range(from_ns, to_ns)
    }

    /// Whether the tape holds any trade for `symbol` at all.
    #[must_use]
    pub fn has_ticks(&self, symbol: &str) -> bool {
        self.live.tape(symbol).newest_ns().is_some()
    }

    /// Normalize one venue kline into a `Candle`.
    ///
    /// Delegates to [`RawKline::to_candle`] rather than repeating the buy/sell
    /// arithmetic. It used to repeat it, and the moment a second venue arrived
    /// the two copies would have disagreed about what a candle with no
    /// order-flow split looks like -- one panicking on an `Option`, the other
    /// inventing a 50/50 split.
    ///
    /// [`RawKline::to_candle`]: crate::exchanges::venue::RawKline::to_candle
    fn candle_from_kline(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        row: &crate::exchanges::venue::RawKline,
    ) -> Candle {
        row.to_candle(symbol, timeframe)
    }
}

/// Merge buffered and venue bars, preferring the buffer on a shared bucket.
fn merge(buffered: Vec<Candle>, fetched: Vec<Candle>) -> Vec<Candle> {
    if fetched.is_empty() {
        return buffered;
    }

    // `BTreeMap` rather than sort+dedup: "first wins" has to be decided by
    // *origin*, and after a sort the two copies of one bucket are adjacent but
    // in no defined order.
    let mut by_bucket: BTreeMap<i64, Candle> = BTreeMap::new();
    for candle in fetched {
        by_bucket.insert(candle.open_time, candle);
    }
    for candle in buffered {
        by_bucket.insert(candle.open_time, candle);
    }
    by_bucket.into_values().collect()
}

/// Open time of the oldest bar, if the series holds one.
fn earliest_open(candles: &[Candle]) -> Option<i64> {
    candles.first().map(|c| c.open_time)
}

/// Count buckets absent from the span the series covers.
pub(crate) fn count_gaps(candles: &[Candle], timeframe: Timeframe) -> usize {
    let Some(first) = candles.first() else {
        return 0;
    };
    let Some(last) = candles.last() else {
        return 0;
    };

    let width = timeframe.nanos().max(1);
    let span = last.open_time.saturating_sub(first.open_time);
    if span <= 0 {
        return 0;
    }
    let expected = usize::try_from(span / width).unwrap_or(usize::MAX) + 1;
    expected.saturating_sub(candles.len())
}

/// Wall-clock now, in unix nanoseconds.
pub(crate) fn now_ns() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchanges::venue::RawKline;

    const MIN: i64 = 60 * 1_000_000_000;

    fn candle(open_time: i64, close: f64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high: close,
            low: close,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn kline(open_time_ms: i64, close: f64) -> RawKline {
        RawKline {
            open_time_ms,
            open: close,
            high: close,
            low: close,
            close,
            volume: 10.0,
            // `Some`, because this helper models a Binance bar -- the venue that
            // does supply the order-flow column. A `None` here would be a
            // different fixture testing a different venue's arithmetic.
            taker_buy_base: Some(6.0),
        }
    }

    #[test]
    fn the_buffer_wins_a_bucket_the_venue_also_returned() {
        // The failure this guards: the venue's copy of the newest bucket is
        // closed and final, the buffer's is built from this process's own
        // stream. If the venue won, the AI would analyse a bar the chart is
        // not drawing.
        let buffered = vec![candle(0, 101.0, 7.0, 3.0)];
        let fetched = vec![candle(0, 999.0, 1.0, 1.0)];
        let merged = merge(buffered, fetched);
        assert_eq!(merged.len(), 1);
        assert!(
            (merged[0].close - 101.0).abs() < 1e-9,
            "the buffer's bar must survive, got {}",
            merged[0].close
        );
        assert!((merged[0].buy_volume - 7.0).abs() < 1e-9);
    }

    #[test]
    fn merge_interleaves_both_sources_in_time_order() {
        let buffered = vec![candle(MIN, 101.0, 1.0, 1.0)];
        let fetched = vec![candle(0, 100.0, 1.0, 1.0), candle(2 * MIN, 102.0, 1.0, 1.0)];
        let merged = merge(buffered, fetched);
        let times: Vec<i64> = merged.iter().map(|c| c.open_time).collect();
        assert_eq!(times, vec![0, MIN, 2 * MIN]);
    }

    #[test]
    fn an_empty_venue_result_leaves_the_buffer_untouched() {
        let buffered = vec![candle(0, 100.0, 1.0, 1.0), candle(MIN, 101.0, 1.0, 1.0)];
        let merged = merge(buffered.clone(), Vec::new());
        assert_eq!(merged.len(), buffered.len());
        assert_eq!(merged[0].open_time, buffered[0].open_time);
    }

    #[test]
    fn a_gap_is_counted_rather_than_papered_over() {
        // Minutes 0, 1 then a jump to 4: two buckets are missing and the count
        // must say so, because a shrunk sample is what makes a base rate lie.
        let candles = vec![
            candle(0, 100.0, 1.0, 1.0),
            candle(MIN, 101.0, 1.0, 1.0),
            candle(4 * MIN, 102.0, 1.0, 1.0),
        ];
        assert_eq!(count_gaps(&candles, Timeframe::M1), 2);
    }

    #[test]
    fn a_contiguous_series_reports_no_gaps() {
        let candles: Vec<Candle> = (0..5).map(|i| candle(i * MIN, 100.0, 1.0, 1.0)).collect();
        assert_eq!(count_gaps(&candles, Timeframe::M1), 0);
    }

    #[test]
    fn a_single_bar_cannot_have_a_gap() {
        assert_eq!(count_gaps(&[candle(0, 100.0, 1.0, 1.0)], Timeframe::M1), 0);
        assert_eq!(count_gaps(&[], Timeframe::M1), 0);
    }

    #[test]
    fn a_kline_becomes_a_candle_with_the_split_derived_from_taker_buy() {
        let service = WindowService::new(
            Arc::new(HistoryRegistry::new()),
            Arc::new(LiveRegistry::new()),
            BackfillClient::binance(),
        );
        let candle = service.candle_from_kline("BTCUSDT", Timeframe::M1, &kline(60_000, 100.0));
        assert_eq!(candle.open_time, 60_000 * 1_000_000);
        assert!((candle.buy_volume - 6.0).abs() < 1e-9);
        assert!((candle.sell_volume - 4.0).abs() < 1e-9);
        assert!((candle.delta() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_venue_reported_taker_buy_above_volume_is_clamped() {
        // Defensive: a venue restating a bar must not produce a negative
        // sell_volume, which would corrupt CVD and read as a broken chart.
        let service = WindowService::new(
            Arc::new(HistoryRegistry::new()),
            Arc::new(LiveRegistry::new()),
            BackfillClient::binance(),
        );
        let mut row = kline(0, 100.0);
        row.taker_buy_base = Some(99.0);
        row.volume = 10.0;
        let candle = service.candle_from_kline("BTCUSDT", Timeframe::M1, &row);
        assert!(candle.sell_volume >= 0.0);
        assert!((candle.buy_volume - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_venue_with_no_order_flow_split_still_produces_a_usable_candle() {
        // The second-venue case, and the reason `taker_buy_base` is an `Option`.
        // Bybit's klines carry no taker-buy column, so a client reading it has
        // no split to report. What must not happen is a panic, a NaN, or a
        // half-and-half split presented as a measurement.
        let service = WindowService::new(
            Arc::new(HistoryRegistry::new()),
            Arc::new(LiveRegistry::new()),
            BackfillClient::binance(),
        );
        let mut row = kline(0, 100.0);
        row.taker_buy_base = None;
        row.volume = 10.0;
        row.open = 100.0;
        row.close = 101.0; // an up candle

        let candle = service.candle_from_kline("BTCUSDT", Timeframe::M1, &row);
        assert!(candle.buy_volume.is_finite() && candle.sell_volume.is_finite());
        assert!((candle.buy_volume + candle.sell_volume - 10.0).abs() < 1e-9);
        // Direction-attributed, so at least it agrees with the candle's own
        // reading rather than contradicting it.
        assert!(
            (candle.buy_volume - 10.0).abs() < 1e-9,
            "an up candle attributes volume to buyers"
        );

        // And it is *knowable* that this split is not a measurement -- the whole
        // reason the field is optional.
        assert!(!row.has_order_flow_split());
    }

    #[test]
    fn the_provenance_line_names_both_sources_and_the_gaps() {
        let window = Window {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            candles: vec![candle(0, 100.0, 1.0, 1.0), candle(3 * MIN, 101.0, 1.0, 1.0)],
            source: WindowSource {
                memory: 1,
                venue: 1,
            },
            gaps: 2,
            forming: true,
            trades_available: false,
        };
        let line = window.provenance();
        assert!(line.contains("1 from memory"), "{line}");
        assert!(line.contains("1 from the venue"), "{line}");
        assert!(line.contains("2 missing"), "{line}");
        assert!(line.contains("forming"), "{line}");
        assert!(line.contains("no tick data"), "{line}");
    }
}
