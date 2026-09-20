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
/// A one-line delegation to [`create_bot_with_key`] with no key, so every
/// existing caller keeps its meaning and there is only one INSERT to keep
/// correct.
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
    create_bot_with_key(pool, user_id, strategy_id, mode, venue, None)
        .await
        .map(|(id, _created)| id)
}

/// Create a bot in the `running` state, idempotent on a client-supplied key.
///
/// Returns the bot's id and whether **this call** created it. `true` means the
/// row is new; `false` means the key had already been used and the id belongs to
/// the bot that call made. A retry therefore returns the bot it already made
/// rather than starting a second one -- which for `mode: "live"` is the
/// difference between one bot placing orders and two.
///
/// ## Why the conflict is resolved by the database, not by a read
///
/// The obvious shape is "look for a bot with this key; if there is one, return
/// it; else insert". That has a time-of-check-to-time-of-use window: two
/// requests that overlap both read nothing, both insert, and the bug survives
/// its own fix. A retry usually arrives well after the first request finished,
/// so the window is small, easy to miss in review, and exactly the kind of thing
/// that passes a test and fails in production.
///
/// So the insert carries `ON CONFLICT ... DO NOTHING`, and a call that gets no
/// row back *knows* another request won and goes to read the winner. Two
/// concurrent requests cannot both insert, because there is nothing for the
/// second to insert into.
///
/// ## Why `None` cannot reach the conflict path
///
/// A unique index treats NULLs as **distinct** in PostgreSQL, so a keyless
/// insert never conflicts and always returns a row. That is what lets the
/// keyless path keep working unchanged, and it is why the branch below is an
/// invariant rather than a case.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn create_bot_with_key(
    pool: &PgPool,
    user_id: Uuid,
    strategy_id: Uuid,
    mode: &str,
    venue: Option<&str>,
    idempotency_key: Option<&str>,
) -> Result<(Uuid, bool), DbError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO bots (user_id, strategy_id, mode, status, venue, idempotency_key) \
         VALUES ($1, $2, $3, 'running', $4, $5) \
         ON CONFLICT (user_id, idempotency_key) DO NOTHING \
         RETURNING id",
    )
    .bind(user_id)
    .bind(strategy_id)
    .bind(mode)
    .bind(venue)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;

    if let Some(id) = inserted {
        return Ok((id, true));
    }

    // No row came back: the key is taken and another request made the bot. Read
    // it and hand it back.
    //
    // The `else` is unreachable -- see the note above, a keyless insert cannot
    // conflict -- but it returns an error rather than panicking. A broken
    // invariant should be a 500 the logs name, not a process that dies while
    // holding a live bot's creation.
    let Some(key) = idempotency_key else {
        return Err(DbError::Pool(sqlx::Error::RowNotFound));
    };

    let existing: Uuid =
        sqlx::query_scalar("SELECT id FROM bots WHERE user_id = $1 AND idempotency_key = $2")
            .bind(user_id)
            .bind(key)
            .fetch_one(pool)
            .await?;
    Ok((existing, false))
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
