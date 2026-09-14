//! Persistence for the bot trading engine (`docs/11-BOT-TRADING-ENGINE.md`,
//! `docs/13-DATABASE-SCHEMA.md`).
//!
//! ## Why the row types live here
//!
//! `docs/03` forbids `trading-engine` from depending on `backtester`, and the
//! same instinct applies in the other direction: the database layer should not
//! know about `PaperBot` or `DecisionOutcome`. So this module takes plain row
//! structs, and the caller maps its own types into them. The alternative --
//! `db` depending on `trading-engine` -- would make the schema follow the bot
//! instead of the bot following the schema.
//!
//! ## Appends, not updates
//!
//! `audit_log` is append-only by design (`docs/15`): nothing here updates or
//! deletes a row. A trade that was later corrected is a *new* event, not an
//! edit, because an audit trail you can rewrite is not an audit trail.

use serde_json::Value;
use sqlx::{PgPool, QueryBuilder, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::ns_to_dt;

/// Rows per multi-row INSERT, matching `repositories.rs`.
const INSERT_CHUNK: usize = 500;

/// One completed simulated or real trade, as `trades_executed` stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedTrade {
    /// The bot that produced it.
    pub bot_id: Uuid,
    /// Symbol traded.
    pub symbol: String,
    /// `long` or `short`.
    pub side: String,
    /// Fill price.
    pub entry_price: f64,
    /// Resolved stop.
    pub stop_price: Option<f64>,
    /// Resolved target.
    pub target_price: Option<f64>,
    /// Exit price, when closed.
    pub exit_price: Option<f64>,
    /// Result in R.
    pub r_multiple: Option<f64>,
    /// Entry time, unix nanos.
    pub opened_at: i64,
    /// Exit time, unix nanos.
    pub closed_at: Option<i64>,
    /// The conditions that fired, and what closed it.
    pub conditions_fired: Value,
}

/// One `audit_log` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    /// Whose bot it was, when it belongs to someone.
    pub user_id: Option<Uuid>,
    /// A stable, greppable event name, e.g. `bot.decision`.
    pub event_type: String,
    /// The event's detail. **Credentials must never reach this field.**
    pub payload: Value,
    /// When it happened, unix nanos.
    pub ts: i64,
}

/// Find or create the `strategies` row for a document.
///
/// The table has no unique key on `(user_id, name, version)`, so this selects
/// first rather than relying on `ON CONFLICT`. Strategies are versioned and
/// never mutated in place (`docs/13`), which is why the lookup includes the
/// version: the same name at a new version is a new row on purpose.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_or_create_strategy(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    version: &str,
    document: &Value,
    created_by: &str,
) -> Result<Uuid, DbError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM strategies \
         WHERE user_id = $1 AND name = $2 AND version = $3 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .fetch_optional(pool)
    .await?;

    if let Some(id) = existing {
        return Ok(id);
    }

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

/// Record a new bot.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_bot(
    pool: &PgPool,
    user_id: Uuid,
    strategy_id: Uuid,
    mode: &str,
    venue: Option<&str>,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO bots (user_id, strategy_id, mode, status, venue) \
         VALUES ($1, $2, $3, 'running', $4) RETURNING id",
    )
    .bind(user_id)
    .bind(strategy_id)
    .bind(mode)
    .bind(venue)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Move a bot to a new status: `running`, `paused`, `stopped` or `killed`.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn set_bot_status(pool: &PgPool, bot_id: Uuid, status: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE bots SET status = $2 WHERE id = $1")
        .bind(bot_id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

/// Persist executed trades.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_executed_trades(
    pool: &PgPool,
    trades: &[ExecutedTrade],
) -> Result<(), DbError> {
    for chunk in trades.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO trades_executed (bot_id, symbol, side, entry_price, stop_price, \
             target_price, exit_price, r_multiple, opened_at, closed_at, conditions_fired) ",
        );
        query.push_values(chunk, |mut row, trade| {
            row.push_bind(trade.bot_id)
                .push_bind(trade.symbol.as_str())
                .push_bind(trade.side.as_str())
                .push_bind(trade.entry_price)
                .push_bind(trade.stop_price)
                .push_bind(trade.target_price)
                .push_bind(trade.exit_price)
                .push_bind(trade.r_multiple)
                .push_bind(ns_to_dt(trade.opened_at))
                .push_bind(trade.closed_at.map(ns_to_dt))
                .push_bind(trade.conditions_fired.clone());
        });
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Append audit events.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_audit_events(pool: &PgPool, events: &[AuditEvent]) -> Result<(), DbError> {
    for chunk in events.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO audit_log (user_id, event_type, payload, ts) ",
        );
        query.push_values(chunk, |mut row, event| {
            row.push_bind(event.user_id)
                .push_bind(event.event_type.as_str())
                .push_bind(event.payload.clone())
                .push_bind(ns_to_dt(event.ts));
        });
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Look up a user by email.
///
/// Deliberately a lookup and not a `find_or_create`: a bot run must attach to
/// a real account, and silently inventing a user row would put trades in the
/// name of somebody who does not exist.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_user_by_email(pool: &PgPool, email: &str) -> Result<Option<Uuid>, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}

/// Count the audit events of one type for one user, newest first.
///
/// Exists so a caller can assert that a decision really was written without
/// reading the whole table.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn count_audit_events(
    pool: &PgPool,
    user_id: Uuid,
    event_type: &str,
) -> Result<i64, DbError> {
    let row =
        sqlx::query("SELECT count(*) AS n FROM audit_log WHERE user_id = $1 AND event_type = $2")
            .bind(user_id)
            .bind(event_type)
            .fetch_one(pool)
            .await?;
    Ok(row.try_get::<i64, _>("n")?)
}
