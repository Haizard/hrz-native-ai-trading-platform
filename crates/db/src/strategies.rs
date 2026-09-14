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

/// Remove a strategy and its backtests.
///
/// Not part of any request path: it exists so an integration test can create
/// real rows and leave the database as it found it.
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
