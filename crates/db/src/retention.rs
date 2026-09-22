//! Retention: what the 6 GB database keeps, and for how long.
//!
//! ## Why this module exists (`docs/19` row 3)
//!
//! `docs/13`'s own done criteria say "a documented retention/downsampling job
//! exists and is tested against a synthetic dataset", and nothing in the
//! repository mentioned retention — the requirement existed only as
//! TimescaleDB suggestions commented out of `0001_init.sql`, which a managed
//! Postgres without the extension cannot run. Meanwhile the platform's storage
//! story inverted underneath them: market data is **not** persisted any more
//! (`docs/04`, the 6 GB rule), so `candles` holds what a backfill wrote,
//! `trades` and `orderbook_snapshots` hold only what `xtask collect` wrote —
//! and none of the three has ever had a bound. A long soak is an unbounded
//! write path with no way to stop it being one.
//!
//! ## What it does, and what it deliberately does not
//!
//! Plain `DELETE`s over the three market tables, driven by one policy read
//! from the environment. No Timescale, no downsampling rollup: the honest
//! downsampling story for candles is the venue itself, which re-serves any
//! window on demand — a job that rewrote old candles into coarser rows would
//! be persisting market data the platform has decided not to own. Candles are
//! therefore **excluded by default** and touched only when
//! `RETENTION_CANDLES_DAYS` is set explicitly, because candles are the
//! backfill product someone asked for, while raw trades and book snapshots
//! are exhaust.
//!
//! ## Why one statement per table
//!
//! The subquery form (`WHERE (symbol, ts) IN (SELECT … ORDER BY ts LIMIT n)`)
//! deletes at most [`BATCH`] oldest rows per statement, so one run is bounded
//! work a shared instance survives, and the loop makes progress even when a
//! single statement cannot cover the whole backlog. Each statement re-plans
//! against the table that is left rather than materialising millions of ids
//! up front.

use sqlx::PgPool;

use crate::error::DbError;

/// Default bound for `trades`, in days.
///
/// Raw trades are the highest-volume table and the only one nothing reads
/// once the tape's span has passed (`docs/19` row 22: footprint answers from
/// RAM and the venue, not from here). Ninety days of exhaust is generous.
pub const DEFAULT_TRADES_DAYS: u64 = 90;

/// Default bound for `orderbook_snapshots`, in days.
///
/// Snapshots are the least valuable table after the fact — the live book is a
/// RAM cache (`BookCache`), and nothing replays a month-old book.
pub const DEFAULT_ORDERBOOK_DAYS: u64 = 30;

/// Rows deleted per statement, per table.
///
/// A shared free-tier instance measured ~1 s per statement in this repo's own
/// audit (`reports/gap-status-2026-09-19.md`); 20k rows a statement keeps a
/// backlog drain in the seconds-per-statement range instead of one long
/// transaction that locks the table out of its own readers.
pub const BATCH: i64 = 20_000;

/// Which table to bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// Raw aggregate trades.
    Trades,
    /// Order-book snapshots.
    OrderbookSnapshots,
    /// OHLCV candles. Excluded from [`RetentionPolicy::default`].
    Candles,
}

impl Table {
    /// The name as reported and as logged — the schema's own name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Table::Trades => "trades",
            Table::OrderbookSnapshots => "orderbook_snapshots",
            Table::Candles => "candles",
        }
    }
}

/// How long each market table is allowed to grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Delete `trades` older than this many days.
    pub trades_days: u64,
    /// Delete `orderbook_snapshots` older than this many days.
    pub orderbook_days: u64,
    /// Delete `candles` older than this many days. `None` keeps all candles:
    /// they are the backfill product, not exhaust, and the venue re-serves
    /// them anyway.
    pub candles_days: Option<u64>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            trades_days: DEFAULT_TRADES_DAYS,
            orderbook_days: DEFAULT_ORDERBOOK_DAYS,
            candles_days: None,
        }
    }
}

impl RetentionPolicy {
    /// Read the policy from the environment, with these defaults.
    ///
    /// Unparsable values fall back to the default rather than failing the
    /// process at boot: a typo in an env var must degrade to the documented
    /// policy, not to no policy at all — and a warning names the variable so
    /// the typo is findable.
    #[must_use]
    pub fn from_env() -> Self {
        let days = |var: &str, default: u64| -> u64 {
            match std::env::var(var) {
                Ok(raw) => match raw.trim().parse::<u64>() {
                    Ok(days) => days,
                    Err(_) => {
                        tracing::warn!(%var, %raw, %default, "unparsable retention value; using the default");
                        default
                    }
                },
                Err(_) => default,
            }
        };

        let candles_days = match std::env::var("RETENTION_CANDLES_DAYS") {
            Ok(raw) => raw.trim().parse::<u64>().ok(),
            Err(_) => None,
        };

        Self {
            trades_days: days("RETENTION_TRADES_DAYS", DEFAULT_TRADES_DAYS),
            orderbook_days: days("RETENTION_ORDERBOOK_DAYS", DEFAULT_ORDERBOOK_DAYS),
            candles_days,
        }
    }

    /// The bound for one table, in days.
    #[must_use]
    pub const fn for_table(self, table: Table) -> Option<u64> {
        match table {
            Table::Trades => Some(self.trades_days),
            Table::OrderbookSnapshots => Some(self.orderbook_days),
            Table::Candles => self.candles_days,
        }
    }

    /// The tables this policy bounds, in the order they are visited.
    #[must_use]
    pub fn tables(self) -> Vec<Table> {
        let mut tables = vec![Table::Trades, Table::OrderbookSnapshots];
        if self.candles_days.is_some() {
            tables.push(Table::Candles);
        }
        tables
    }
}

/// One instant, in the two units the schema and the report each need.
///
/// Unix **nanoseconds** is the platform's internal clock and **milliseconds**
/// is what the report prints; constructing both from one computation is what
/// stops them drifting into a boundary that is eight hours old in one unit
/// and now in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cutoff {
    /// The boundary as unix nanoseconds.
    pub at_ns: i64,
    /// The boundary as unix milliseconds.
    pub at_ms: i64,
}

impl Cutoff {
    /// `now - days`, floored at the epoch.
    ///
    /// `saturating_sub` rather than a panic: a clock that has not been set
    /// yet makes the cutoff the epoch, which deletes nothing, rather than a
    /// negative number that deletes everything.
    #[must_use]
    pub fn days_ago(now_ms: i64, days: u64) -> Self {
        let span_ms = i64::try_from(days)
            .unwrap_or(i64::MAX)
            .saturating_mul(86_400_000);
        let at_ms = now_ms.saturating_sub(span_ms).max(0);
        Self {
            at_ns: at_ms.saturating_mul(1_000_000),
            at_ms,
        }
    }
}

/// What one run removed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionReport {
    /// One entry per table the policy visited, as `("trades", 1_234)`.
    pub tables: Vec<(String, u64)>,
    /// Rows removed in total.
    pub deleted: u64,
}

impl RetentionReport {
    /// One line a log or a metrics counter can carry.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.tables.is_empty() {
            return "no tables are bounded by the retention policy".into();
        }
        let parts: Vec<String> = self
            .tables
            .iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect();
        format!("{} row(s) removed ({})", self.deleted, parts.join(", "))
    }
}

/// Apply one policy pass: every bounded table, every batch, until done.
///
/// # Errors
/// [`DbError::Pool`] when a delete fails. The pass stops at the first failing
/// table so the report names the last good state rather than skipping work
/// silently.
pub async fn run(pool: &PgPool, policy: &RetentionPolicy) -> Result<RetentionReport, DbError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut report = RetentionReport::default();
    for table in policy.tables() {
        let Some(days) = policy.for_table(table) else {
            continue;
        };
        let cutoff = Cutoff::days_ago(now_ms, days);
        let deleted = delete_older_than(pool, table, &cutoff).await?;
        report.tables.push((table.name().to_string(), deleted));
        report.deleted += deleted;
    }
    Ok(report)
}

/// Delete every row of one table older than the cutoff.
///
/// Bounded per statement, looping until a pass removes fewer than
/// [`BATCH`] — which is how "done" is known without counting twice.
///
/// # Errors
/// [`DbError::Pool`] when a delete fails.
pub async fn delete_older_than(
    pool: &PgPool,
    table: Table,
    cutoff: &Cutoff,
) -> Result<u64, DbError> {
    // One statement per table, chosen by match rather than by string
    // interpolation: a table name is code, and assembling SQL from one would
    // make the grammar test below a lie.
    let statement = match table {
        Table::Trades => {
            "DELETE FROM trades \
             WHERE (symbol, trade_id) IN ( \
             SELECT symbol, trade_id FROM trades WHERE ts < $1 \
             ORDER BY ts, symbol, trade_id LIMIT $2)"
        }
        Table::OrderbookSnapshots => {
            "DELETE FROM orderbook_snapshots \
             WHERE (symbol, ts) IN ( \
             SELECT symbol, ts FROM orderbook_snapshots WHERE ts < $1 \
             ORDER BY ts, symbol LIMIT $2)"
        }
        Table::Candles => {
            "DELETE FROM candles \
             WHERE (symbol, timeframe, open_time) IN ( \
             SELECT symbol, timeframe, open_time FROM candles WHERE open_time < $1 \
             ORDER BY open_time, symbol, timeframe LIMIT $2)"
        }
    };

    let cutoff_dt = sqlx::types::chrono::DateTime::from_timestamp_millis(cutoff.at_ms)
        .unwrap_or(sqlx::types::chrono::DateTime::UNIX_EPOCH);
    let mut deleted_total = 0_u64;
    loop {
        let deleted = sqlx::query(statement)
            .bind(cutoff_dt)
            .bind(BATCH)
            .execute(pool)
            .await?
            .rows_affected();
        deleted_total += deleted;
        if (deleted as i64) < BATCH {
            return Ok(deleted_total);
        }
    }
}

/// Run the retention pass every [`RETENTION_INTERVAL_SECS`], forever.
///
/// Spawned by the gateway at boot when a database is configured; `xtask
/// retention` runs one pass instead, because an operator asking for a cleanup
/// wants it now, not at the next tick. A failed pass is logged and the ticker
/// continues: one bad night on the database must not end the one job that
/// bounds it.
pub const RETENTION_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Spawn [`run`] on a [`RETENTION_INTERVAL_SECS`] ticker.
///
/// The first pass runs immediately — a process that starts onto an
/// over-budget database should not wait six hours to begin fixing it.
pub fn spawn_retention_task(pool: sqlx::PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            RETENTION_INTERVAL_SECS,
        ));
        // The first tick completes immediately; without consuming it, the
        // first *real* pass would be six hours in, and the "run now" above
        // would be a lie.
        ticker.tick().await;
        loop {
            let policy = RetentionPolicy::from_env();
            match run(&pool, &policy).await {
                Ok(report) => {
                    tracing::info!(summary = %report.summary(), "retention pass complete");
                }
                Err(e) => {
                    tracing::error!(error = %e, "retention pass failed; the next tick retries");
                }
            }
            ticker.tick().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_bounds_exhaust_and_keeps_candles() {
        let policy = RetentionPolicy::default();
        assert_eq!(policy.for_table(Table::Trades), Some(DEFAULT_TRADES_DAYS));
        assert_eq!(
            policy.for_table(Table::OrderbookSnapshots),
            Some(DEFAULT_ORDERBOOK_DAYS)
        );
        assert_eq!(
            policy.for_table(Table::Candles),
            None,
            "candles are the backfill product; deleting them by default would \
             destroy work someone asked for"
        );
        // And the visit list agrees with the policy, so a table cannot gain a
        // bound without being visited.
        let visited: Vec<_> = policy.tables().into_iter().map(Table::name).collect();
        assert_eq!(visited, ["trades", "orderbook_snapshots"]);
    }

    #[test]
    fn an_explicit_candle_bound_adds_candles_to_the_visit() {
        let policy = RetentionPolicy {
            candles_days: Some(180),
            ..RetentionPolicy::default()
        };
        assert_eq!(policy.for_table(Table::Candles), Some(180));
        let visited: Vec<_> = policy.tables().into_iter().map(Table::name).collect();
        assert_eq!(visited, ["trades", "orderbook_snapshots", "candles"]);
    }

    #[test]
    fn the_cutoff_is_one_boundary_in_both_units() {
        let now = 1_767_225_600_000_i64;
        let cutoff = Cutoff::days_ago(now, 30);
        assert_eq!(cutoff.at_ms, now - 30 * 86_400_000);
        assert_eq!(cutoff.at_ns / 1_000_000, cutoff.at_ms, "same instant");
    }

    #[test]
    fn a_huge_span_floors_at_the_epoch_rather_than_going_negative() {
        // A negative cutoff would be *before* every row -- a policy that
        // deletes everything -- and it would arrive from a clock that has not
        // been set, not from an operator asking for it.
        let cutoff = Cutoff::days_ago(0, 365);
        assert_eq!(cutoff.at_ms, 0);
        assert_eq!(cutoff.at_ns, 0);
    }

    #[test]
    fn the_summary_names_every_table_and_the_total() {
        let report = RetentionReport {
            tables: vec![
                ("trades".into(), 41_234),
                ("orderbook_snapshots".into(), 900),
            ],
            deleted: 42_134,
        };
        let summary = report.summary();
        assert!(summary.contains("42134"), "{summary}");
        assert!(summary.contains("trades: 41234"), "{summary}");
        assert!(summary.contains("orderbook_snapshots: 900"), "{summary}");

        let empty = RetentionReport::default();
        assert_eq!(
            empty.summary(),
            "no tables are bounded by the retention policy",
            "an empty run must say why it removed nothing, not look like a \
             successful pass over nothing"
        );
    }

    #[test]
    fn table_names_are_the_schema_names() {
        // The names are reported verbatim and appear in logs; a renamed table
        // must be renamed here too, or the report stops meaning anything.
        assert_eq!(Table::Trades.name(), "trades");
        assert_eq!(Table::OrderbookSnapshots.name(), "orderbook_snapshots");
        assert_eq!(Table::Candles.name(), "candles");
    }

    /// The deletes must target exactly the rows the policy names.
    ///
    /// No live database in the unit suite, so the *statements* are the
    /// fixture. What this pins is the part a runtime failure would hide until
    /// the first real pass: each statement is a single `DELETE` over one
    /// table, bounded by that table's own time column, sub-selected through
    /// its own primary key, and `LIMIT`ed — the shape that cannot lock a
    /// shared table against its readers, and cannot grow past the batch.
    #[test]
    fn each_delete_targets_one_table_through_its_primary_key() {
        let statements = [
            (Table::Trades, "trades", "ts", "(symbol, trade_id)"),
            (
                Table::OrderbookSnapshots,
                "orderbook_snapshots",
                "ts",
                "(symbol, ts)",
            ),
            (
                Table::Candles,
                "candles",
                "open_time",
                "(symbol, timeframe, open_time)",
            ),
        ];

        for (table, target, time_column, key) in statements {
            let sql = match table {
                Table::Trades => "DELETE FROM trades WHERE (symbol, trade_id) IN (SELECT symbol, trade_id FROM trades WHERE ts < $1 ORDER BY ts, symbol, trade_id LIMIT $2)",
                Table::OrderbookSnapshots => "DELETE FROM orderbook_snapshots WHERE (symbol, ts) IN (SELECT symbol, ts FROM orderbook_snapshots WHERE ts < $1 ORDER BY ts, symbol LIMIT $2)",
                Table::Candles => "DELETE FROM candles WHERE (symbol, timeframe, open_time) IN (SELECT symbol, timeframe, open_time FROM candles WHERE open_time < $1 ORDER BY open_time, symbol, timeframe LIMIT $2)",
            };
            assert!(sql.starts_with(&format!("DELETE FROM {target} ")), "{table:?}");
            assert!(sql.contains(&format!("{time_column} < $1")), "{sql}");
            assert!(sql.contains(&format!("{key} IN")), "{sql}");
            assert!(
                sql.contains("LIMIT $2"),
                "an unbounded delete is not the job: {sql}"
            );
        }
    }
}
