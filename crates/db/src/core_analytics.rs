//! Durable Core Analytics predictions with one-shot settlement (docs/64).
//!
//! A row is written when a discipline makes a call and is later settled once
//! from realized market data. The decision-time fields are never updated, so a
//! reliability report cannot be polluted by hindsight.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// A prediction to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCorePrediction {
    /// Symbol the call was made on.
    pub symbol: String,
    /// Resolution the call describes.
    pub timeframe: String,
    /// Discipline that made the call.
    pub discipline: String,
    /// Directional conclusion, using the standard result's spelling.
    pub direction: String,
    /// Evidence strength in `[0, 1]`, never a probability.
    pub evidence_strength: f64,
    /// Model or configuration version.
    pub model_version: String,
    /// Decision instant, unix nanoseconds.
    pub decision_at: i64,
}

/// A stored prediction, settled or not.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CorePredictionRow {
    /// Row id.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Symbol.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Discipline.
    pub discipline: String,
    /// Direction at decision time.
    pub direction: String,
    /// Evidence strength at decision time.
    pub evidence_strength: f64,
    /// Model version.
    pub model_version: String,
    /// Decision instant, unix nanoseconds.
    pub decision_at: i64,
    /// Settlement instant, unix nanoseconds, once realized.
    pub settled_at: Option<i64>,
    /// Realized forward return.
    pub realized_return: Option<f64>,
    /// Realized direction.
    pub realized_direction: Option<String>,
    /// Whether the directional call was correct.
    pub hit: Option<bool>,
}

const SELECT: &str = "SELECT id, user_id, symbol, timeframe, discipline, direction, \
                      evidence_strength, model_version, decision_at, settled_at, \
                      realized_return, realized_direction, hit FROM core_analytics_predictions";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<CorePredictionRow, DbError> {
    Ok(CorePredictionRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        discipline: row.try_get("discipline")?,
        direction: row.try_get("direction")?,
        evidence_strength: row.try_get("evidence_strength")?,
        model_version: row.try_get("model_version")?,
        decision_at: dt_to_ns(row.try_get("decision_at")?),
        settled_at: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("settled_at")?
            .map(dt_to_ns),
        realized_return: row.try_get("realized_return")?,
        realized_direction: row.try_get("realized_direction")?,
        hit: row.try_get("hit")?,
    })
}

/// Store one prediction for a user.
pub async fn insert_prediction(
    pool: &PgPool,
    user_id: Uuid,
    new: &NewCorePrediction,
) -> Result<CorePredictionRow, DbError> {
    let row = sqlx::query(
        "INSERT INTO core_analytics_predictions \
         (user_id, symbol, timeframe, discipline, direction, evidence_strength, model_version, decision_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING \
         id, user_id, symbol, timeframe, discipline, direction, evidence_strength, model_version, \
         decision_at, settled_at, realized_return, realized_direction, hit",
    )
    .bind(user_id)
    .bind(new.symbol.to_uppercase())
    .bind(&new.timeframe)
    .bind(&new.discipline)
    .bind(&new.direction)
    .bind(new.evidence_strength)
    .bind(&new.model_version)
    .bind(ns_to_dt(new.decision_at))
    .fetch_one(pool)
    .await?;
    from_row(&row)
}

/// List a user's predictions, newest decision first.
pub async fn list_predictions(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
    unsettled_only: bool,
) -> Result<Vec<CorePredictionRow>, DbError> {
    let sql = if unsettled_only {
        format!(
            "{SELECT} WHERE user_id = $1 AND settled_at IS NULL ORDER BY decision_at DESC LIMIT $2"
        )
    } else {
        format!("{SELECT} WHERE user_id = $1 ORDER BY decision_at DESC LIMIT $2")
    };
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.iter().map(from_row).collect()
}

/// Settle an unsettled prediction exactly once.
///
/// Returns `None` when the row does not belong to the user or was already
/// settled; the caller must not treat that as a failed settlement it can retry.
pub async fn settle_prediction(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    realized_return: f64,
    realized_direction: &str,
    hit: bool,
    settled_at: i64,
) -> Result<Option<CorePredictionRow>, DbError> {
    let row = sqlx::query(
        "UPDATE core_analytics_predictions \
         SET settled_at = $3, realized_return = $4, realized_direction = $5, hit = $6 \
         WHERE id = $1 AND user_id = $2 AND settled_at IS NULL RETURNING \
         id, user_id, symbol, timeframe, discipline, direction, evidence_strength, model_version, \
         decision_at, settled_at, realized_return, realized_direction, hit",
    )
    .bind(id)
    .bind(user_id)
    .bind(ns_to_dt(settled_at))
    .bind(realized_return)
    .bind(realized_direction)
    .bind(hit)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).transpose()
}
