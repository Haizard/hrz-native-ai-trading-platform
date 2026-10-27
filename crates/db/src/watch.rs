//! The zone watcher's tables (`docs/48`): monitors are standing watch
//! instructions, events are their firings.
//!
//! Cooldowns live here, in the events table, rather than in the watcher
//! loop's memory: a restarted watcher must not re-notify the same touch, so
//! "when did this zone last fire" is a query, never a variable.

use chrono::{DateTime, Utc};

use crate::error::DbError;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// One standing watch instruction.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WatchMonitor {
    /// The monitor's id.
    pub id: Uuid,
    /// Whose chart this watches.
    pub user_id: Uuid,
    /// The market scope's symbol, normalised uppercase on write.
    pub symbol: String,
    /// The market scope's resolution.
    pub timeframe: String,
    /// The skill pinned on the triggered run, when one is.
    pub skill_id: Option<String>,
    /// Whether the loop watches this scope at all.
    pub enabled: bool,
    /// Minutes before the same zone may fire again.
    pub cooldown_minutes: i32,
    /// When the monitor was created.
    pub created_at: DateTime<Utc>,
    /// When the monitor last changed.
    pub updated_at: DateTime<Utc>,
}

/// One firing of a monitor.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WatchEvent {
    /// The event's id.
    pub id: Uuid,
    /// The monitor that fired.
    pub monitor_id: Uuid,
    /// Whose agent was woken (denormalised from the monitor).
    pub user_id: Uuid,
    /// The market scope's symbol (denormalised).
    pub symbol: String,
    /// The market scope's resolution (denormalised).
    pub timeframe: String,
    /// The zone that was touched, when one was.
    pub drawing_id: Option<Uuid>,
    /// Human-readable trigger: what price did, and to which zone.
    pub reason: String,
    /// `done` with a summary, or `failed` with the error text.
    pub status: String,
    /// The run's thesis narrative, bounded at write.
    pub summary: Option<String>,
    /// When the watcher fired.
    pub created_at: DateTime<Utc>,
}

/// Create or refresh the monitor for one scope. The toggle in the UI is an
/// upsert by design: a second click updates the skill and re-enables, rather
/// than stacking duplicate watches on the same chart.
pub async fn upsert_watch_monitor(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    timeframe: &str,
    skill_id: Option<&str>,
    cooldown_minutes: i32,
) -> Result<WatchMonitor, DbError> {
    sqlx::query_as::<_, WatchMonitor>(
        r#"
        INSERT INTO watch_monitors (user_id, symbol, timeframe, skill_id, cooldown_minutes)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (user_id, symbol, timeframe)
        DO UPDATE SET skill_id = EXCLUDED.skill_id,
                      cooldown_minutes = EXCLUDED.cooldown_minutes,
                      enabled = TRUE,
                      updated_at = now()
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(symbol)
    .bind(timeframe)
    .bind(skill_id)
    .bind(cooldown_minutes)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

/// The user's monitor for one scope, when one exists.
///
/// The create path reads this before applying a cap: toggling an existing
/// monitor is an update, not a new watcher, and must not be refused because the
/// user is already at their limit.
pub async fn find_watch_monitor(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    timeframe: &str,
) -> Result<Option<WatchMonitor>, DbError> {
    sqlx::query_as::<_, WatchMonitor>(
        "SELECT * FROM watch_monitors WHERE user_id = $1 AND symbol = $2 AND timeframe = $3",
    )
    .bind(user_id)
    .bind(symbol)
    .bind(timeframe)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// The user's own monitors, newest first -- the panel's list.
pub async fn list_watch_monitors(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<WatchMonitor>, DbError> {
    sqlx::query_as::<_, WatchMonitor>(
        "SELECT * FROM watch_monitors WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Every enabled monitor, every user -- the watcher loop's read, each tick.
pub async fn list_enabled_watch_monitors(pool: &PgPool) -> Result<Vec<WatchMonitor>, DbError> {
    sqlx::query_as::<_, WatchMonitor>(
        "SELECT * FROM watch_monitors WHERE enabled ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Enable or disable one of the user's own monitors. Returns the updated row,
/// or `None` when the id is not this user's -- a fact, not an error leak.
pub async fn set_watch_monitor_enabled(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    enabled: bool,
) -> Result<Option<WatchMonitor>, DbError> {
    sqlx::query_as::<_, WatchMonitor>(
        r#"
        UPDATE watch_monitors SET enabled = $3, updated_at = now()
        WHERE id = $1 AND user_id = $2
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(enabled)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// Delete one of the user's own monitors; its events cascade with it.
pub async fn delete_watch_monitor(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<bool, DbError> {
    let result = sqlx::query("DELETE FROM watch_monitors WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Record a firing. Written before the run starts, so a crashed run still
/// leaves the touch on record with status `failed` to follow.
pub async fn create_watch_event(
    pool: &PgPool,
    monitor: &WatchMonitor,
    drawing_id: Option<Uuid>,
    reason: &str,
) -> Result<WatchEvent, DbError> {
    sqlx::query_as::<_, WatchEvent>(
        r#"
        INSERT INTO watch_events (monitor_id, user_id, symbol, timeframe, drawing_id, reason, status)
        VALUES ($1, $2, $3, $4, $5, $6, 'failed')
        RETURNING *
        "#,
    )
    .bind(monitor.id)
    .bind(monitor.user_id)
    .bind(monitor.symbol.as_str())
    .bind(monitor.timeframe.as_str())
    .bind(drawing_id)
    .bind(reason)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

/// Close out an event with the run's outcome: `done` with the thesis
/// narrative (bounded), or `failed` with the error's text.
pub async fn finish_watch_event(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    summary: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query("UPDATE watch_events SET status = $2, summary = $3 WHERE id = $1")
        .bind(id)
        .bind(status)
        .bind(summary)
        .execute(pool)
        .await?;
    Ok(())
}

/// The zone's latest firing, for the cooldown check.
pub async fn latest_watch_event_for_drawing(
    pool: &PgPool,
    monitor_id: Uuid,
    drawing_id: Uuid,
) -> Result<Option<DateTime<Utc>>, DbError> {
    sqlx::query_scalar::<_, DateTime<Utc>>(
        "SELECT created_at FROM watch_events WHERE monitor_id = $1 AND drawing_id = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(monitor_id)
    .bind(drawing_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// The monitor's latest firing of any zone: the floor that keeps a burst of
/// touches from becoming a burst of paid runs.
pub async fn latest_watch_event_for_monitor(
    pool: &PgPool,
    monitor_id: Uuid,
) -> Result<Option<DateTime<Utc>>, DbError> {
    sqlx::query_scalar::<_, DateTime<Utc>>(
        "SELECT created_at FROM watch_events WHERE monitor_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(monitor_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// The user's recent events, newest first -- the panel's list.
pub async fn list_watch_events(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<WatchEvent>, DbError> {
    sqlx::query_as::<_, WatchEvent>(
        "SELECT * FROM watch_events WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}
