//! The pattern library: detected patterns a user chose to keep (`docs/45`).
//!
//! ## Explicit saves, never automatic
//!
//! `detect_pattern` is deterministic and cheap, so a match is not inherently
//! valuable -- the engine finds a double top every other afternoon. The library
//! holds the ones somebody *recorded*: the anchors, levels, and confidence at
//! detection time, frozen as a row. Auto-saving every detection would make the
//! library a log of everything the engine ever saw, and a library you cannot
//! find anything in is not a library.
//!
//! ## Why not a drawing
//!
//! A pattern save could be a labelled drawing. It is not, because the questions
//! differ: drawings answer "what is on this chart", the library answers "which
//! head-and-shoulders have I kept, and how did they read" -- a query by kind,
//! direction and confidence that the drawings table's shape does not carry.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A stored pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternRow {
    /// The row id.
    pub id: Uuid,
    /// The instrument, uppercase.
    pub symbol: String,
    /// The timeframe it was detected on.
    pub timeframe: String,
    /// The detector's vocabulary (head_and_shoulders, double_top, ...).
    pub kind: String,
    /// bullish | bearish | neutral.
    pub direction: String,
    /// The detector's confidence at detection, 0..1.
    pub confidence: f64,
    /// The swings that defined it, frozen.
    pub anchors: serde_json::Value,
    /// The neckline / breakout level.
    pub entry_level: f64,
    /// The measured-move objective.
    pub target: f64,
    /// The level that kills the read.
    pub invalidation: f64,
    /// One line, in the detector's words.
    pub summary: String,
    /// The saver's note, if any.
    pub note: Option<String>,
    /// `user` or `ai`.
    pub created_by: String,
    /// Unix nanoseconds.
    pub created_at: i64,
}

/// A pattern about to be saved.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPattern {
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// The kind.
    pub kind: String,
    /// The direction.
    pub direction: String,
    /// The confidence, 0..1.
    pub confidence: f64,
    /// The anchors.
    pub anchors: serde_json::Value,
    /// The entry level.
    pub entry_level: f64,
    /// The measured target.
    pub target: f64,
    /// The invalidation level.
    pub invalidation: f64,
    /// The detector's summary line.
    pub summary: String,
    /// The saver's note.
    pub note: Option<String>,
    /// `user` or `ai`.
    pub created_by: String,
}

const SELECT: &str = "SELECT id, symbol, timeframe, kind, direction, confidence, anchors, entry_level, target, invalidation, summary, note, created_by, created_at FROM pattern_library";

fn row_to_pattern(row: &sqlx::postgres::PgRow) -> Result<PatternRow, DbError> {
    Ok(PatternRow {
        id: row.try_get("id")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        kind: row.try_get("kind")?,
        direction: row.try_get("direction")?,
        confidence: row.try_get("confidence")?,
        anchors: row.try_get("anchors")?,
        entry_level: row.try_get("entry_level")?,
        target: row.try_get("target")?,
        invalidation: row.try_get("invalidation")?,
        summary: row.try_get("summary")?,
        note: row.try_get("note")?,
        created_by: row.try_get("created_by")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Save a pattern.
pub async fn insert_pattern(
    pool: &PgPool,
    user_id: Uuid,
    pattern: &NewPattern,
) -> Result<PatternRow, DbError> {
    let row = sqlx::query(&format!(
        "INSERT INTO pattern_library \
             (user_id, symbol, timeframe, kind, direction, confidence, anchors, \
              entry_level, target, invalidation, summary, note, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         RETURNING id, symbol, timeframe, kind, direction, confidence, anchors, \
                   entry_level, target, invalidation, summary, note, created_by, created_at"
    ))
    .bind(user_id)
    .bind(pattern.symbol.to_uppercase())
    .bind(&pattern.timeframe)
    .bind(&pattern.kind)
    .bind(&pattern.direction)
    .bind(pattern.confidence)
    .bind(&pattern.anchors)
    .bind(pattern.entry_level)
    .bind(pattern.target)
    .bind(pattern.invalidation)
    .bind(&pattern.summary)
    .bind(&pattern.note)
    .bind(&pattern.created_by)
    .fetch_one(pool)
    .await?;
    row_to_pattern(&row)
}

/// This user's saved patterns, newest first, optionally one symbol, capped.
pub async fn list_patterns(
    pool: &PgPool,
    user_id: Uuid,
    symbol: Option<&str>,
    limit: i64,
) -> Result<Vec<PatternRow>, DbError> {
    let rows = match symbol {
        Some(symbol) => sqlx::query(&format!(
            "{SELECT} WHERE user_id = $1 AND symbol = $2 ORDER BY created_at DESC LIMIT $3"
        ))
        .bind(user_id)
        .bind(symbol.to_uppercase())
        .bind(limit)
        .fetch_all(pool)
        .await?,
        None => sqlx::query(&format!(
            "{SELECT} WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(pool)
        .await?,
    };
    rows.iter().map(row_to_pattern).collect()
}

/// Delete a saved pattern.
pub async fn delete_pattern(
    pool: &PgPool,
    user_id: Uuid,
    pattern_id: Uuid,
) -> Result<bool, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM pattern_library WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(pattern_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(id.is_some())
}
