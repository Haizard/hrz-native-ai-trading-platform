//! Chart snapshots: the point-in-time record of what a chart showed (`docs/45`).
//!
//! ## A frozen copy, deliberately
//!
//! A snapshot holds the last close, the drawings on the symbol, and a structure
//! digest **as JSONB** -- copies of state whose homes are elsewhere (the
//! candles table, [`crate::drawings`], the analytics engines). Columns would
//! claim the snapshot tracks its sources; JSONB says what is true: this is what
//! the chart showed at capture, and the live state has since moved on. The
//! diff between two rows of this table is what the agent's
//! `compare_snapshots` tool answers with.
//!
//! ## Who captures
//!
//! Two writers: the user (a button) and the agent (the `take_snapshot` tool),
//! distinguished by `created_by`, which is a CHECK-constrained vocabulary
//! rather than convention -- see `0012`'s header for why.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A stored snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSnapshotRow {
    /// The row id.
    pub id: Uuid,
    /// The instrument, uppercase.
    pub symbol: String,
    /// The timeframe the chart was showing.
    pub timeframe: String,
    /// Last close at capture.
    pub price: f64,
    /// The user's drawings on the symbol, frozen.
    pub drawings: serde_json::Value,
    /// The structure digest at capture.
    pub structure: serde_json::Value,
    /// What the capturer said about it, if anything.
    pub note: Option<String>,
    /// Retrieval tags.
    pub tags: Vec<String>,
    /// `user` or `ai` (CHECK-constrained).
    pub created_by: String,
    /// Unix nanoseconds.
    pub created_at: i64,
}

/// A snapshot about to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChartSnapshot {
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// Last close at capture.
    pub price: f64,
    /// Frozen drawings.
    pub drawings: serde_json::Value,
    /// Frozen structure digest.
    pub structure: serde_json::Value,
    /// The capturer's note.
    pub note: Option<String>,
    /// Retrieval tags.
    pub tags: Vec<String>,
    /// `user` or `ai`.
    pub created_by: String,
}

fn row_to_snapshot(row: &sqlx::postgres::PgRow) -> Result<ChartSnapshotRow, DbError> {
    Ok(ChartSnapshotRow {
        id: row.try_get("id")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        price: row.try_get("price")?,
        drawings: row.try_get("drawings")?,
        structure: row.try_get("structure")?,
        note: row.try_get("note")?,
        tags: row.try_get("tags")?,
        created_by: row.try_get("created_by")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

const SELECT: &str = "SELECT id, symbol, timeframe, price, drawings, structure, note, tags, created_by, created_at FROM chart_snapshots";

/// Write a snapshot.
pub async fn insert_chart_snapshot(
    pool: &PgPool,
    user_id: Uuid,
    snapshot: &NewChartSnapshot,
) -> Result<ChartSnapshotRow, DbError> {
    let row = sqlx::query(&format!(
        "INSERT INTO chart_snapshots \
             (user_id, symbol, timeframe, price, drawings, structure, note, tags, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id, symbol, timeframe, price, drawings, structure, note, tags, created_by, created_at"
    ))
    .bind(user_id)
    .bind(snapshot.symbol.to_uppercase())
    .bind(&snapshot.timeframe)
    .bind(snapshot.price)
    .bind(&snapshot.drawings)
    .bind(&snapshot.structure)
    .bind(&snapshot.note)
    .bind(&snapshot.tags)
    .bind(&snapshot.created_by)
    .fetch_one(pool)
    .await?;
    row_to_snapshot(&row)
}

/// This user's snapshots of a symbol, newest first, capped.
///
/// The cap is a parameter, not a constant, because the agent's compare flow
/// wants a handful and a history picker wants a page; the clamp lives in the
/// caller that has the product decision to make.
pub async fn list_chart_snapshots(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    limit: i64,
) -> Result<Vec<ChartSnapshotRow>, DbError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE user_id = $1 AND symbol = $2 ORDER BY created_at DESC LIMIT $3"
    ))
    .bind(user_id)
    .bind(symbol.to_uppercase())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_snapshot).collect()
}

/// One snapshot, when it belongs to this user.
pub async fn get_chart_snapshot(
    pool: &PgPool,
    user_id: Uuid,
    snapshot_id: Uuid,
) -> Result<Option<ChartSnapshotRow>, DbError> {
    let row = sqlx::query(&format!("{SELECT} WHERE id = $1 AND user_id = $2"))
        .bind(snapshot_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    row.map(|r| row_to_snapshot(&r)).transpose()
}
