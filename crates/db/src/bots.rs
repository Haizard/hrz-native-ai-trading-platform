//! Bots, scoped by owner (`docs/13-DATABASE-SCHEMA.md`).
//!
//! ## Why this is separate from `paper`
//!
//! `db::paper` is the *trading loop's* persistence: it writes decisions and
//! trades for a bot that is already running, and it does not care who owns it.
//! This module is the *control plane*: who owns which bot, what state it is in,
//! and listing them for a dashboard. Both touch the `bots` table, which is why
//! the two are next to each other rather than merged -- a trading loop should
//! not be able to change a bot's owner, and a dashboard should not be able to
//! write a trade.
//!
//! ## Every read is scoped by owner
//!
//! A bot belonging to somebody else reports as *absent*, never *forbidden*. A
//! 403 would confirm the id exists, which is a fact a caller has no business
//! learning by guessing.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A bot, as the control plane sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotRow {
    /// The bot.
    pub id: Uuid,
    /// Who owns it.
    pub user_id: Uuid,
    /// The strategy it runs.
    pub strategy_id: Uuid,
    /// `paper` or `live`.
    pub mode: String,
    /// `running`, `paused`, `stopped` or `killed`.
    pub status: String,
    /// Venue, for a bot that trades a real one.
    pub venue: Option<String>,
    /// When it was created, unix nanos.
    pub created_at: i64,
}

/// The statuses a bot may hold.
///
/// A closed set, because the column is read by a UI that has to render
/// something for every value it can contain.
pub const STATUSES: [&str; 4] = ["running", "paused", "stopped", "killed"];

fn from_row(row: &sqlx::postgres::PgRow) -> Result<BotRow, sqlx::Error> {
    Ok(BotRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        strategy_id: row.try_get("strategy_id")?,
        mode: row.try_get("mode")?,
        status: row.try_get("status")?,
        venue: row.try_get("venue")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Create a bot in the `running` state.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn create_bot(
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

/// Read one bot, if this user owns it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn get_bot(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<Option<BotRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, user_id, strategy_id, mode, status, venue, created_at \
         FROM bots WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).transpose().map_err(Into::into)
}

/// List this user's bots, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_bots(pool: &PgPool, user_id: Uuid, limit: i64) -> Result<Vec<BotRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, user_id, strategy_id, mode, status, venue, created_at \
         FROM bots WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Move a bot to a new status.
///
/// Returns `false` when the bot does not exist or is not this user's, so the
/// caller can answer 404 without a second query.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn set_status(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    status: &str,
) -> Result<bool, DbError> {
    let result = sqlx::query("UPDATE bots SET status = $3 WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Remove a bot and everything it wrote.
///
/// `DELETE /bots/{id}` is in `docs/12`, and deleting a bot means deleting its
/// trades and audit rows too: leaving them behind would attach them to nothing,
/// and `bots.id` is a foreign key on `trades_executed`.
///
/// Returns `false` when the bot does not exist or is not this user's.
///
/// # Errors
/// Returns [`DbError::Pool`] if the deletes fail.
pub async fn delete_bot(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<bool, DbError> {
    // Confirm ownership first: the deletes below are keyed on bot_id alone.
    if get_bot(pool, user_id, id).await?.is_none() {
        return Ok(false);
    }
    crate::paper::purge_bot(pool, id).await?;
    Ok(true)
}
