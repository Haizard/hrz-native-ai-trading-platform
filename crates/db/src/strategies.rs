//! Strategies and backtests (`docs/13-DATABASE-SCHEMA.md`).
//!
//! ## Strategies are versioned, never edited
//!
//! `docs/13` is explicit: "Strategies -- the DSL document, versioned like
//! skills". An edit writes a **new row**, which is why nothing here updates a
//! `strategies` row. A backtest stored last month has to keep pointing at the
//! exact document that produced it, or its numbers stop meaning anything.
//!
//! ## Everything is scoped by owner
//!
//! Every read takes a `user_id` and filters on it. A strategy belonging to
//! somebody else is reported as *absent* rather than *forbidden*: a 403 would
//! confirm that the id exists, which is a fact a caller has no business
//! learning from a guess.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// A stored strategy document.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyRow {
    /// The row id.
    pub id: Uuid,
    /// The document's own name.
    pub name: String,
    /// The document's own version.
    pub version: String,
    /// The full DSL document.
    pub document: Value,
    /// Who produced it: `ai_agent`, `visual_builder` or `developer_sdk`.
    pub created_by: String,
    /// When it was stored, unix nanos.
    pub created_at: i64,
}

/// A stored backtest report.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestRow {
    /// The row id.
    pub id: Uuid,
    /// The strategy it ran.
    pub strategy_id: Uuid,
    /// Symbol tested.
    pub symbol: String,
    /// Window start, unix nanos.
    pub date_from: i64,
    /// Window end, unix nanos.
    pub date_to: i64,
    /// The full performance report.
    pub report: Value,
    /// When it ran, unix nanos.
    pub created_at: i64,
}

fn strategy_from_row(row: &sqlx::postgres::PgRow) -> Result<StrategyRow, sqlx::Error> {
    Ok(StrategyRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        version: row.try_get("version")?,
        document: row.try_get("document")?,
        created_by: row.try_get("created_by")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Store a new strategy version.
///
/// Always an insert. Two rows with the same name and version is not a
/// conflict to resolve -- it is what versioning looks like, and the newest
/// wins on read.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn create_strategy(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    version: &str,
    document: &Value,
    created_by: &str,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO strategies (user_id, name, version, document, created_by) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .bind(document)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Read one strategy, if this user owns it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn get_strategy(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<StrategyRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, name, version, document, created_by, created_at \
         FROM strategies WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(strategy_from_row)
        .transpose()
        .map_err(Into::into)
}

/// List this user's strategies, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_strategies(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<StrategyRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, name, version, document, created_by, created_at \
         FROM strategies WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(strategy_from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Store a backtest report.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn create_backtest(
    pool: &PgPool,
    strategy_id: Uuid,
    symbol: &str,
    date_from: i64,
    date_to: i64,
    report: &Value,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO backtests (strategy_id, symbol, date_from, date_to, report) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(strategy_id)
    .bind(symbol)
    .bind(ns_to_dt(date_from))
    .bind(ns_to_dt(date_to))
    .bind(report)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Read one backtest, if this user owns the strategy it belongs to.
///
/// The join is the ownership check: `backtests` has no `user_id` of its own,
/// because a backtest belongs to a strategy and a strategy belongs to a user.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn get_backtest(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<BacktestRow>, DbError> {
    let row = sqlx::query(
        "SELECT b.id, b.strategy_id, b.symbol, b.date_from, b.date_to, b.report, b.created_at \
         FROM backtests b JOIN strategies s ON s.id = b.strategy_id \
         WHERE b.id = $1 AND s.user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else { return Ok(None) };
    Ok(Some(BacktestRow {
        id: row.try_get("id")?,
        strategy_id: row.try_get("strategy_id")?,
        symbol: row.try_get("symbol")?,
        date_from: dt_to_ns(row.try_get("date_from")?),
        date_to: dt_to_ns(row.try_get("date_to")?),
        report: row.try_get("report")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    }))
}

/// The newest stored backtest for a symbol, across every user.
///
/// ## Why this has no ownership filter
///
/// Every other read here is scoped to a user, and this one deliberately is not.
/// It exists for offline verification — `strategy-cli verify` reads back a run
/// the platform already produced and re-checks its trades against the candle
/// table — and that is an operator reading the platform's own output, not a
/// request for someone's data. It is not reachable over HTTP; `docs/12` has no
/// route that could serve it.
///
/// ## Why `require_trades` exists
///
/// A stored run with an empty `trades` array is legitimate and common: an empty
/// window, or a strategy whose conditions never fired over the period. It is
/// also useless to a caller whose whole purpose is to check trades, and
/// "the newest run happens to be empty" would otherwise be indistinguishable
/// from "the symbol has no runs". The caller asks for what it needs and says
/// which run it got.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn newest_backtest_for_symbol(
    pool: &PgPool,
    symbol: &str,
    require_trades: bool,
) -> Result<Option<BacktestRow>, DbError> {
    // `jsonb_array_length` is safe here because `report` is written by
    // `serde_json::to_value` of a `BacktestReport`, whose `trades` is always an
    // array. `COALESCE` guards the shape rather than the intent: a hand-edited
    // row without the key would make the whole query error, taking every other
    // row with it.
    let sql = if require_trades {
        "SELECT id, strategy_id, symbol, date_from, date_to, report, created_at \
         FROM backtests \
         WHERE symbol = $1 AND COALESCE(jsonb_array_length(report->'trades'), 0) > 0 \
         ORDER BY created_at DESC LIMIT 1"
    } else {
        "SELECT id, strategy_id, symbol, date_from, date_to, report, created_at \
         FROM backtests WHERE symbol = $1 ORDER BY created_at DESC LIMIT 1"
    };

    let row = sqlx::query(sql).bind(symbol).fetch_optional(pool).await?;

    let Some(row) = row else { return Ok(None) };
    Ok(Some(BacktestRow {
        id: row.try_get("id")?,
        strategy_id: row.try_get("strategy_id")?,
        symbol: row.try_get("symbol")?,
        date_from: dt_to_ns(row.try_get("date_from")?),
        date_to: dt_to_ns(row.try_get("date_to")?),
        report: row.try_get("report")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    }))
}

/// List the backtests of one strategy, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_backtests(
    pool: &PgPool,
    user_id: Uuid,
    strategy_id: Uuid,
    limit: i64,
) -> Result<Vec<BacktestRow>, DbError> {
    let rows = sqlx::query(
        "SELECT b.id, b.strategy_id, b.symbol, b.date_from, b.date_to, b.report, b.created_at \
         FROM backtests b JOIN strategies s ON s.id = b.strategy_id \
         WHERE b.strategy_id = $1 AND s.user_id = $2 \
         ORDER BY b.created_at DESC LIMIT $3",
    )
    .bind(strategy_id)
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(BacktestRow {
                id: row.try_get("id")?,
                strategy_id: row.try_get("strategy_id")?,
                symbol: row.try_get("symbol")?,
                date_from: dt_to_ns(row.try_get("date_from")?),
                date_to: dt_to_ns(row.try_get("date_to")?),
                report: row.try_get("report")?,
                created_at: dt_to_ns(row.try_get("created_at")?),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(Into::into)
}

/// How many bots run this strategy.
///
/// `bots.strategy_id` references `strategies (id)`, so deleting a strategy that
/// still has bots is a foreign-key error. Counted first so the API can refuse
/// with a reason the caller can act on, instead of a 500 naming a constraint.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn count_bots_for_strategy(pool: &PgPool, id: Uuid) -> Result<i64, DbError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bots WHERE strategy_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// Remove a strategy and its backtests.
///
/// Used by `DELETE /strategies/{id}` and by integration tests, which create
/// real rows and have to leave the database as they found it.
///
/// A backtest is deleted with its strategy because a report without the
/// document that produced it is a set of numbers with no provenance. The
/// caller owns the ownership check and the bots check -- see
/// [`count_bots_for_strategy`].
///
/// # Errors
/// Returns [`DbError::Pool`] if the deletes fail.
pub async fn delete_strategy(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM backtests WHERE strategy_id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM strategies WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
