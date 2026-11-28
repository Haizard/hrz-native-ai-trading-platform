//! Durable Market Opportunity Memory with one-shot settlement.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// An opportunity to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewOpportunity {
    /// Symbol the representation describes.
    pub symbol: String,
    /// Resolution of the representation.
    pub timeframe: String,
    /// Direction at decision time.
    pub direction: String,
    /// Packed representation signature, 0..=255.
    pub signature: i16,
    /// Evidence strength, not a probability.
    pub evidence_strength: f64,
    /// Decision instant, unix nanoseconds.
    pub observed_at: i64,
}

/// A stored opportunity.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OpportunityRow {
    /// Row id.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Symbol.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Direction at decision time.
    pub direction: String,
    /// Representation signature.
    pub signature: i16,
    /// Evidence strength.
    pub evidence_strength: f64,
    /// Decision instant, unix nanoseconds.
    pub observed_at: i64,
    /// Settlement instant, unix nanoseconds.
    pub settled_at: Option<i64>,
    /// Realized forward return.
    pub realized_return: Option<f64>,
    /// Whether the directional call was correct.
    pub hit: Option<bool>,
}

const SELECT: &str = "SELECT id, user_id, symbol, timeframe, direction, signature, \
                      evidence_strength, observed_at, settled_at, realized_return, hit \
                      FROM opportunity_memory";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<OpportunityRow, DbError> {
    Ok(OpportunityRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        direction: row.try_get("direction")?,
        signature: row.try_get("signature")?,
        evidence_strength: row.try_get("evidence_strength")?,
        observed_at: dt_to_ns(row.try_get("observed_at")?),
        settled_at: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("settled_at")?
            .map(dt_to_ns),
        realized_return: row.try_get("realized_return")?,
        hit: row.try_get("hit")?,
    })
}

/// Store one opportunity for a user.
pub async fn insert_opportunity(
    pool: &PgPool,
    user_id: Uuid,
    new: &NewOpportunity,
) -> Result<OpportunityRow, DbError> {
    let row = sqlx::query(
        "INSERT INTO opportunity_memory \
         (user_id, symbol, timeframe, direction, signature, evidence_strength, observed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING \
         id, user_id, symbol, timeframe, direction, signature, evidence_strength, \
         observed_at, settled_at, realized_return, hit",
    )
    .bind(user_id)
    .bind(new.symbol.to_uppercase())
    .bind(&new.timeframe)
    .bind(&new.direction)
    .bind(new.signature)
    .bind(new.evidence_strength)
    .bind(ns_to_dt(new.observed_at))
    .fetch_one(pool)
    .await?;
    from_row(&row)
}

/// List a user's opportunities, newest first.
pub async fn list_opportunities(
    pool: &PgPool,
    user_id: Uuid,
    symbol: Option<&str>,
    settled: Option<bool>,
    limit: i64,
) -> Result<Vec<OpportunityRow>, DbError> {
    let mut sql = format!("{SELECT} WHERE user_id = $1");
    if symbol.is_some() {
        sql.push_str(" AND symbol = $2");
    }
    match settled {
        Some(true) => sql.push_str(" AND settled_at IS NOT NULL"),
        Some(false) => sql.push_str(" AND settled_at IS NULL"),
        None => {}
    }
    sql.push_str(" ORDER BY observed_at DESC LIMIT $3");
    let query = sqlx::query(&sql).bind(user_id);
    let query = if let Some(symbol) = symbol {
        query.bind(symbol.to_uppercase())
    } else {
        query
    };
    // When no symbol is bound, Postgres cannot infer $2's type for an unused
    // placeholder. Rebuild without it instead.
    let rows = if symbol.is_none() {
        let mut fallback = format!("{SELECT} WHERE user_id = $1");
        match settled {
            Some(true) => fallback.push_str(" AND settled_at IS NOT NULL"),
            Some(false) => fallback.push_str(" AND settled_at IS NULL"),
            None => {}
        }
        fallback.push_str(" ORDER BY observed_at DESC LIMIT $2");
        sqlx::query(&fallback)
            .bind(user_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
    } else {
        query.bind(limit).fetch_all(pool).await?
    };
    rows.iter().map(from_row).collect()
}

/// Settle one opportunity exactly once.
pub async fn settle_opportunity(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    realized_return: f64,
    hit: bool,
    settled_at: i64,
) -> Result<Option<OpportunityRow>, DbError> {
    let row = sqlx::query(
        "UPDATE opportunity_memory \
         SET settled_at = $3, realized_return = $4, hit = $5 \
         WHERE id = $1 AND user_id = $2 AND settled_at IS NULL RETURNING \
         id, user_id, symbol, timeframe, direction, signature, evidence_strength, \
         observed_at, settled_at, realized_return, hit",
    )
    .bind(id)
    .bind(user_id)
    .bind(ns_to_dt(settled_at))
    .bind(realized_return)
    .bind(hit)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(from_row).transpose()
}
