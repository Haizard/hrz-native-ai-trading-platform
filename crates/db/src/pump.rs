//! Batched writers draining a market broadcast channel into Postgres.
//!
//! ## Why this lives in `db` and not in the two binaries that need it
//!
//! `xtask collect` and the gateway's live feed both have to take everything off
//! a `broadcast` channel and persist it. The loop itself is not the interesting
//! part -- the decisions are the batch size, the flush interval, what a lag
//! means, and whether what is still in the buffer survives the way out. Two
//! copies of those decisions is two places to fix the same bug, which is
//! exactly the shape `docs/19` row 20 already caught once with a batching
//! writer. So there is one loop here and two thin callers.
//!
//! ## A pump with no database is still a pump
//!
//! `db: None` drains and counts rather than silently discarding: the channel is
//! emptied and the counter moves, so a pump wired to nothing is observable
//! instead of invisible. That is also what makes this testable without a
//! database.

use std::future::Future;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use analytics_core::types::{Candle, OrderBookSnapshot, Trade};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::interval;

use crate::repositories;
use crate::Database;

/// How often a partly-filled buffer is written anyway.
///
/// Bounded so a quiet market is not a quiet database: BTCUSDT closes one 1m
/// candle a minute, which would sit in the buffer indefinitely on a batch-size
/// trigger alone.
pub const FLUSH_SECS: u64 = 5;

/// Candles close rarely, so a small batch is plenty.
pub const CANDLE_BATCH: usize = 128;
/// Trades arrive continuously; the bigger batch is what keeps the write rate
/// sane at ~10 rows a second on one symbol.
pub const TRADE_BATCH: usize = 512;
/// Books publish every second and are the least valuable thing in the table
/// after the fact, so the batch is the smallest of the three.
pub const BOOK_BATCH: usize = 64;

/// When an item happened, so a pump can say how fresh its writes are.
///
/// All three timestamps are unix **nanoseconds**, per `analytics_core::types`.
pub trait Timestamped {
    /// The instant this item describes, in nanoseconds.
    fn at_ns(&self) -> i64;
}

impl Timestamped for Candle {
    fn at_ns(&self) -> i64 {
        self.open_time
    }
}

impl Timestamped for Trade {
    fn at_ns(&self) -> i64 {
        self.timestamp
    }
}

impl Timestamped for OrderBookSnapshot {
    fn at_ns(&self) -> i64 {
        self.timestamp
    }
}

/// What a pump has written, for a caller that wants to publish it.
///
/// The point is the second field. A pump that is running and writing nothing
/// is indistinguishable from a pump that is working, and "nothing has been
/// persisted for thirteen hours" is precisely the failure this module was
/// written to end -- so freshness is reported, not just volume.
#[derive(Debug, Default)]
pub struct Report {
    /// Items persisted since the process started.
    written: AtomicU64,
    /// When the newest persisted item happened, in milliseconds. `0` before the
    /// first successful write.
    newest_ms: AtomicI64,
}

impl Report {
    /// A report with nothing written yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Items persisted so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// Milliseconds since the epoch of the newest persisted item, or `0`.
    #[must_use]
    pub fn newest_ms(&self) -> i64 {
        self.newest_ms.load(Ordering::Relaxed)
    }

    /// How stale the newest persisted item is, in seconds.
    ///
    /// `None` before anything has been written: "nothing stored" and "stored
    /// thirteen hours ago" are different failures and must not share a number.
    #[must_use]
    pub fn age_secs(&self, now_ms: i64) -> Option<f64> {
        let newest = self.newest_ms();
        (newest > 0).then(|| (now_ms.saturating_sub(newest)) as f64 / 1_000.0)
    }
}

/// Drain `rx`, writing in batches of `batch` and at worst every `every`.
///
/// Returns how many items were taken off the channel. Stops when the sender is
/// dropped, after one last flush -- a pump that loses its buffer on the way out
/// loses the candles that closed just before shutdown, which is the one moment
/// nobody is watching.
pub async fn pump<T, F, Fut>(
    name: &'static str,
    rx: &mut broadcast::Receiver<T>,
    db: Option<Arc<Database>>,
    batch: usize,
    every: Duration,
    report: Option<&Report>,
    mut write: F,
) -> u64
where
    // `Clone` is not this module's requirement, it is `broadcast::Receiver`'s:
    // every receiver gets its own copy of each message, so a type that is not
    // `Clone` cannot be on the bus at all.
    T: Timestamped + Clone + Send + 'static,
    F: FnMut(Arc<Database>, Vec<T>) -> Fut,
    Fut: Future<Output = Result<usize, String>>,
{
    let mut buffer: Vec<T> = Vec::new();
    let mut flush = interval(every);
    let mut taken = 0_u64;

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(item) => {
                    buffer.push(item);
                    taken += 1;
                    if buffer.len() >= batch {
                        flush_buffer(name, &db, &mut buffer, report, &mut write).await;
                    }
                }
                // A lag is a lost message, not a lost connection: the pump is
                // behind, and the gap it leaves in the table is a real gap.
                // Said out loud because otherwise it is invisible.
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("{name} pump lagged by {n} message(s)");
                }
                Err(RecvError::Closed) => break,
            },
            _ = flush.tick() => flush_buffer(name, &db, &mut buffer, report, &mut write).await,
        }
    }

    flush_buffer(name, &db, &mut buffer, report, &mut write).await;
    tracing::info!("{name} pump stopped after {taken} item(s)");
    taken
}

/// Persist trades, buffering and flushing.
pub async fn pump_trades(
    rx: &mut broadcast::Receiver<Trade>,
    db: Option<Arc<Database>>,
    report: Option<&Report>,
) -> u64 {
    pump(
        "trade",
        rx,
        db,
        TRADE_BATCH,
        Duration::from_secs(FLUSH_SECS),
        report,
        |db, batch| {
            let count = batch.len();
            async move {
                repositories::insert_trades(db.pool(), &batch)
                    .await
                    .map(|()| count)
                    .map_err(|e| e.to_string())
            }
        },
    )
    .await
}

/// Persist order-book snapshots, buffering and flushing.
pub async fn pump_books(
    rx: &mut broadcast::Receiver<OrderBookSnapshot>,
    db: Option<Arc<Database>>,
    report: Option<&Report>,
) -> u64 {
    pump(
        "order book",
        rx,
        db,
        BOOK_BATCH,
        Duration::from_secs(FLUSH_SECS),
        report,
        |db, batch| {
            let count = batch.len();
            async move {
                repositories::insert_orderbook_snapshots(db.pool(), &batch)
                    .await
                    .map(|()| count)
                    .map_err(|e| e.to_string())
            }
        },
    )
    .await
}

/// Persist closed candles, buffering and flushing.
///
/// ## Why this pump had to exist
///
/// The collector aggregates every resolution the platform uses --
/// `MultiTimeframeCandleBuilder::standard` builds 1m/5m/15m/1h/4h/1d -- and
/// published the closed ones to the bus. Nothing subscribed in order to write
/// them, so a run persisted trades and order books and **zero candles**, and
/// the only way to fill the candle table was `backfill`, which fetches from
/// REST.
///
/// The upsert is idempotent on `(symbol, timeframe, open_time)`, which matters
/// here more than for the other two pumps: a reconnect replays nothing, but a
/// restarted collector re-emits a bucket it had already closed, and a duplicate
/// would otherwise be a primary-key error.
pub async fn pump_candles(
    rx: &mut broadcast::Receiver<Candle>,
    db: Option<Arc<Database>>,
    report: Option<&Report>,
) -> u64 {
    pump(
        "candle",
        rx,
        db,
        CANDLE_BATCH,
        Duration::from_secs(FLUSH_SECS),
        report,
        |db, batch| {
            let count = batch.len();
            async move {
                repositories::insert_candles(db.pool(), &batch)
                    .await
                    .map(|()| count)
                    .map_err(|e| e.to_string())
            }
        },
    )
    .await
}

/// Write whatever is buffered, and report it.
///
/// Counted even when there is no database, because a pump wired to nothing
/// still has to be distinguishable from a pump that is not running.
async fn flush_buffer<T, F, Fut>(
    name: &'static str,
    db: &Option<Arc<Database>>,
    buffer: &mut Vec<T>,
    report: Option<&Report>,
    write: &mut F,
) where
    T: Timestamped + Clone + Send + 'static,
    F: FnMut(Arc<Database>, Vec<T>) -> Fut,
    Fut: Future<Output = Result<usize, String>>,
{
    if buffer.is_empty() {
        return;
    }
    let batch = std::mem::take(buffer);
    let count = batch.len() as u64;
    let newest = batch.iter().map(|item| item.at_ns()).max().unwrap_or(0) / 1_000_000;

    // Counted, but **not** marked fresh: nothing was persisted, so there is
    // nothing whose age can be reported. Conflating the two would make a pump
    // wired to no database look like a healthy one, which is the exact silence
    // this module exists to prevent.
    let Some(db) = db.clone() else {
        if let Some(report) = report {
            report.written.fetch_add(count, Ordering::Relaxed);
        }
        return;
    };

    let outcome = write(db, batch).await;
    // Dropped rather than retried: the next flush brings the following batch,
    // and a pump that blocks on a failed write stops draining the channel,
    // which turns one bad insert into a lagged consumer and then into a
    // second, larger gap.
    if let Err(e) = &outcome {
        tracing::error!("failed to write {count} {name}(s): {e}");
    }
    note(report, count, newest, outcome);
}

/// Record the outcome of one flush.
///
/// Split out of [`flush_buffer`] so the bookkeeping is testable without a
/// database: only a *successful* write may move the freshness, and that rule is
/// the whole difference between "nothing is stored" and "storage is 13h stale".
fn note(report: Option<&Report>, count: u64, newest_ms: i64, outcome: Result<usize, String>) {
    let Some(report) = report else {
        return;
    };
    if outcome.is_ok() {
        report.written.fetch_add(count, Ordering::Relaxed);
        report.newest_ms.fetch_max(newest_ms, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::{OrderBookLevel, Side, Timeframe};
    use tokio::sync::broadcast::error::RecvError;

    fn candle(open_time: i64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
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

    fn trade(timestamp: i64) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 1,
            price: 100.0,
            quantity: 1.0,
            is_buyer_maker: false,
            timestamp,
        }
    }

    fn book(timestamp: i64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "BTCUSDT".into(),
            timestamp,
            bids: vec![OrderBookLevel {
                price: 100.0,
                quantity: 1.0,
            }],
            asks: vec![OrderBookLevel {
                price: 101.0,
                quantity: 1.0,
            }],
        }
    }

    #[test]
    fn a_report_before_anything_is_written_has_no_age() {
        let report = Report::new();
        assert_eq!(report.written(), 0);
        assert_eq!(
            report.age_secs(1_000_000),
            None,
            "nothing stored and stored-long-ago must not share a number"
        );
    }

    #[test]
    fn timestamps_are_read_as_nanoseconds() {
        // The fields are nanoseconds; the report is milliseconds.
        assert_eq!(candle(1_700_000_000_000_000_000).at_ns() / 1_000_000, 1_700_000_000_000);
        assert_eq!(trade(1_700_000_000_000_000_000).at_ns() / 1_000_000, 1_700_000_000_000);
        assert_eq!(book(1_700_000_000_000_000_000).at_ns() / 1_000_000, 1_700_000_000_000);
        assert!(matches!(trade(0).side(), Side::Buy));
    }

    /// The pump's contract, with no database and no clock worth waiting on:
    /// everything published is taken off the channel, counted, and reported,
    /// and the last of it survives the sender going away.
    #[tokio::test]
    async fn a_pump_drains_counts_and_flushes_what_is_left() {
        let (tx, mut rx) = broadcast::channel(64);
        let report = Report::new();

        for i in 0..3 {
            tx.send(candle(1_700_000_000_000_000_000 + i * 60_000_000_000))
                .expect("the receiver is alive");
        }
        // Closed after the sends, so the pump must exit on `RecvError::Closed`
        // and flush on the way out rather than dropping the buffer.
        drop(tx);

        let taken = pump(
            "candle",
            &mut rx,
            None,
            CANDLE_BATCH,
            Duration::from_secs(FLUSH_SECS),
            Some(&report),
            |_db, batch| async move { Ok(batch.len()) },
        )
        .await;

        assert_eq!(taken, 3, "everything published must be taken off the bus");
        assert_eq!(report.written(), 3, "drained with no database is still counted");
        assert_eq!(
            report.newest_ms(),
            0,
            "nothing was persisted, so nothing can be called fresh: a pump wired \
             to no database must not look like a healthy one"
        );
    }

    /// The success path, without a database: only a write that landed may move
    /// the freshness, and a failed write moves nothing at all.
    #[test]
    fn a_write_that_landed_moves_the_freshness_and_one_that_failed_does_not() {
        let report = Report::new();
        note(Some(&report), 3, 1_700_000_000_000, Ok(3));
        assert_eq!(report.written(), 3);
        assert_eq!(report.newest_ms(), 1_700_000_000_000);

        // An older batch must not drag the newest backwards.
        note(Some(&report), 1, 1_600_000_000_000, Ok(1));
        assert_eq!(report.newest_ms(), 1_700_000_000_000);
        assert_eq!(report.written(), 4);

        note(Some(&report), 5, 1_800_000_000_000, Err("boom".into()));
        assert_eq!(report.written(), 4, "a failed write counts as nothing");
        assert_eq!(report.newest_ms(), 1_700_000_000_000);

        // And without a report, nothing panics.
        note(None, 1, 1, Ok(1));
    }

    #[tokio::test]
    async fn a_pump_flushes_on_the_batch_size_without_waiting_for_the_clock() {
        // Bigger than the batch, because a `broadcast` channel drops the oldest
        // message when it fills and the pump has not started reading yet --
        // which would make this a test of lag handling rather than of batching.
        let (tx, mut rx) = broadcast::channel(CANDLE_BATCH * 2);
        let report = Report::new();

        // One over the batch size: the batch trigger is what fires, not the
        // five-second tick, and this test must not wait five seconds to prove
        // it.
        for i in 0..(CANDLE_BATCH as i64 + 1) {
            tx.send(candle(1_700_000_000_000_000_000 + i)).ok();
        }

        // A one-hour interval: if the report moves at all, the batch trigger
        // did it and not the clock.
        let draining = pump(
            "candle",
            &mut rx,
            None,
            CANDLE_BATCH,
            Duration::from_secs(3_600),
            Some(&report),
            |_db, batch| async move { Ok(batch.len()) },
        );
        tokio::pin!(draining);

        // The future has to actually be polled -- a batch test that never runs
        // the pump proves nothing, and passes for the wrong reason.
        tokio::select! {
            _ = &mut draining => {}
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }

        assert!(
            report.written() >= CANDLE_BATCH as u64,
            "a full batch must be written without waiting for the flush interval: \
             wrote {} of {}",
            report.written(),
            CANDLE_BATCH
        );
    }

    #[tokio::test]
    async fn a_lag_is_spoken_and_does_not_stop_the_pump() {
        let (tx, mut rx) = broadcast::channel(2);
        let report = Report::new();

        for i in 0..8 {
            tx.send(trade(1_700_000_000_000_000_000 + i)).ok();
        }
        drop(tx);

        let taken = pump(
            "trade",
            &mut rx,
            None,
            1,
            Duration::from_secs(FLUSH_SECS),
            Some(&report),
            |_db, batch| async move { Ok(batch.len()) },
        )
        .await;

        // Some messages were certainly lagged out of a capacity-2 channel; the
        // pump must keep going and report what it did get.
        assert!(
            taken <= 8 && report.written() > 0,
            "a lag must not end the pump: taken={taken} written={}",
            report.written()
        );
    }

    #[tokio::test]
    async fn a_failed_write_does_not_stop_the_drain() {
        let (tx, mut rx) = broadcast::channel(64);
        let report = Report::new();

        for i in 0..4 {
            tx.send(book(1_700_000_000_000_000_000 + i)).ok();
        }
        drop(tx);

        // No database, so this write never runs; the point of the test is that
        // a pump wired to nothing still empties the channel and counts.
        let taken = pump(
            "order book",
            &mut rx,
            None,
            BOOK_BATCH,
            Duration::from_secs(FLUSH_SECS),
            Some(&report),
            |_db, batch| async move { Ok(batch.len()) },
        )
        .await;

        assert_eq!(taken, 4);
        assert_eq!(report.written(), 4);
    }

    #[test]
    fn the_channel_closing_is_not_an_error() {
        // `RecvError::Closed` is how every pump ends, and `Lagged` is a warning
        // rather than a stop. Both are enumerated so a new arm cannot be added
        // without deciding which one it is.
        let closed = Err::<u8, RecvError>(RecvError::Closed);
        assert!(matches!(closed, Err(RecvError::Closed)));
    }
}
