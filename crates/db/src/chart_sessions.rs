//! Chart sessions: the multi-panel grid, named and kept (`docs/45`).
//!
//! ## One session, many panels
//!
//! A session is the workspace the user arranged -- "morning scan", "BTC swing"
//! -- and its panels are the charts in it. Panels are rows rather than JSONB
//! because "which sessions watch SOL on the 5m" is a real question, and a
//! question a JSONB blob answers only by reading every row it is asked about.
//!
//! ## Templates are sessions, not a second table
//!
//! `is_template` marks a session as a starting point rather than a workspace.
//! The distinction is one bit of user intent, and one bit does not earn a
//! second table with its own read/write paths. Restoring a template creates a
//! new non-template session from its panels; templates themselves are not
//! edited by ordinary saves.
//!
//! ## Replace-the-panels writes
//!
//! Saving a session rewrites its panels wholesale: delete the old rows, insert
//! the new. A grid save is the user saying "the workspace looks like *this*
//! now", and diffing the old against the new would preserve a history nobody
//! asked for -- the same reasoning as [`crate::drawings`], one level up.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A stored chart session, without its panels.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSessionRow {
    /// The row id.
    pub id: Uuid,
    /// What the user called it.
    pub name: String,
    /// Whether it is a starting point rather than a workspace.
    pub is_template: bool,
    /// Unix nanoseconds.
    pub created_at: i64,
    /// Unix nanoseconds; bumped on every save.
    pub updated_at: i64,
}

/// One panel of a session: the chart in one grid slot.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartPanelRow {
    /// The row id.
    pub id: Uuid,
    /// Grid slot, 0-based.
    pub position: i32,
    /// The instrument, uppercase.
    pub symbol: String,
    /// The engine's timeframe vocabulary (`1m` .. `1d`).
    pub timeframe: String,
    /// Candlesticks, heikin-ashi, line, ... -- the engine validates.
    pub chart_type: Option<String>,
    /// Configured indicator instances, opaque `[{name, params, ...}]`.
    pub indicators: serde_json::Value,
}

/// A session as a save request carries it: name, template flag, panels.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChartSession {
    /// What the user called it.
    pub name: String,
    /// Whether it is a starting point.
    pub is_template: bool,
    /// The panels, in slot order; `position` is assigned from the index.
    pub panels: Vec<NewChartPanel>,
}

/// A panel in a save request.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChartPanel {
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// The chart style, when set.
    pub chart_type: Option<String>,
    /// Configured indicators.
    pub indicators: serde_json::Value,
}

fn row_to_session(row: &sqlx::postgres::PgRow) -> Result<ChartSessionRow, DbError> {
    Ok(ChartSessionRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        is_template: row.try_get("is_template")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

fn row_to_panel(row: &sqlx::postgres::PgRow) -> Result<ChartPanelRow, DbError> {
    Ok(ChartPanelRow {
        id: row.try_get("id")?,
        position: row.try_get("position")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        chart_type: row.try_get("chart_type")?,
        indicators: row.try_get("indicators")?,
    })
}

/// This user's sessions, most recently saved first.
pub async fn list_chart_sessions(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ChartSessionRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, name, is_template, created_at, updated_at \
           FROM chart_sessions \
          WHERE user_id = $1 \
          ORDER BY updated_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_session).collect()
}

/// The panels of one session, in slot order.
pub async fn list_chart_panels(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Vec<ChartPanelRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, position, symbol, timeframe, chart_type, indicators \
           FROM chart_panels \
          WHERE session_id = $1 \
          ORDER BY position",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_panel).collect()
}

/// A session plus its panels, when it belongs to this user.
pub async fn get_chart_session(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<Option<(ChartSessionRow, Vec<ChartPanelRow>)>, DbError> {
    let row = sqlx::query(
        "SELECT id, name, is_template, created_at, updated_at \
           FROM chart_sessions \
          WHERE id = $1 AND user_id = $2",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let panels = list_chart_panels(pool, session_id).await?;
            Ok(Some((row_to_session(&row)?, panels)))
        }
    }
}

/// Create a session and its panels.
///
/// One transaction: a session without its panels is a save that half-landed,
/// and the caller should get an error, not a partial grid.
pub async fn create_chart_session(
    pool: &PgPool,
    user_id: Uuid,
    session: &NewChartSession,
) -> Result<ChartSessionRow, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "INSERT INTO chart_sessions (user_id, name, is_template) \
         VALUES ($1, $2, $3) \
         RETURNING id, name, is_template, created_at, updated_at",
    )
    .bind(user_id)
    .bind(&session.name)
    .bind(session.is_template)
    .fetch_one(&mut *tx)
    .await?;
    let out = row_to_session(&row)?;
    insert_panels(&mut tx, out.id, &session.panels).await?;
    tx.commit().await?;
    Ok(out)
}

/// Save over a session: new name/flag, and the panels replaced wholesale.
///
/// `None` when the session is not this user's -- the same answer as "no such
/// session", because a save must not reveal that another user's workspace
/// exists.
pub async fn update_chart_session(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Uuid,
    session: &NewChartSession,
) -> Result<Option<ChartSessionRow>, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "UPDATE chart_sessions \
            SET name = $3, is_template = $4, updated_at = now() \
          WHERE id = $1 AND user_id = $2 \
          RETURNING id, name, is_template, created_at, updated_at",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(&session.name)
    .bind(session.is_template)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(None);
    };
    sqlx::query("DELETE FROM chart_panels WHERE session_id = $1")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    insert_panels(&mut tx, session_id, &session.panels).await?;
    let out = row_to_session(&row)?;
    tx.commit().await?;
    Ok(Some(out))
}

/// Delete a session; the panels go with it (`ON DELETE CASCADE`).
pub async fn delete_chart_session(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<bool, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM chart_sessions WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(id.is_some())
}

async fn insert_panels(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
    panels: &[NewChartPanel],
) -> Result<(), DbError> {
    for (position, panel) in panels.iter().enumerate() {
        sqlx::query(
            "INSERT INTO chart_panels \
                 (session_id, position, symbol, timeframe, chart_type, indicators) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(session_id)
        .bind(position as i32)
        .bind(panel.symbol.to_uppercase())
        .bind(&panel.timeframe)
        .bind(&panel.chart_type)
        .bind(&panel.indicators)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
