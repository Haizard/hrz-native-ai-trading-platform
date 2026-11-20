//! Immutable Markov evidence records with one-shot settlement (`docs/62`).
//!
//! A row is written when `analyze_markov` runs. It is never overwritten: the
//! only later write is [`settle`], which fills the realized outcome once the
//! forecast horizon has matured. That separation is what makes out-of-sample
//! reliability measurable and reproducible.

use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// One evidence record about to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMarkovEvidence {
    /// Instrument the evidence was computed for.
    pub symbol: String,
    /// Resolution the evidence was computed on.
    pub timeframe: String,
    /// Full serialized `MarkovResult`.
    pub evidence: Value,
    /// Engine version, for replay and reliability grouping.
    pub model_version: String,
    /// Forecast horizon in bars, when the caller supplied one.
    pub horizon_bars: Option<f64>,
    /// When the evidence was observed, unix nanoseconds.
    pub observed_at: i64,
}

/// A stored evidence record.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkovEvidenceRow {
    /// Row id.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Full serialized `MarkovResult`.
    pub evidence: Value,
    /// Engine version.
    pub model_version: String,
    /// Forecast horizon in bars.
    pub horizon_bars: Option<f64>,
    /// Observation time, unix nanoseconds.
    pub observed_at: i64,
    /// Settlement time, unix nanoseconds, once realized.
    pub settled_at: Option<i64>,
    /// Realized outcome payload, once settled.
    pub outcome: Option<Value>,
}

/// A realized outcome, stored as opaque JSON so the scoring formula can change.
#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    /// Realized outcome as opaque JSON.
    pub outcome: Value,
    /// Settlement time, unix nanoseconds.
    pub settled_at: i64,
}

const SELECT: &str = "SELECT id, user_id, symbol, timeframe, evidence, model_version, \
                      horizon_bars, observed_at, settled_at, outcome FROM markov_evidence";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<MarkovEvidenceRow, DbError> {
    Ok(MarkovEvidenceRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        evidence: row.try_get("evidence")?,
        model_version: row.try_get("model_version")?,
        horizon_bars: row.try_get("horizon_bars")?,
        observed_at: dt_to_ns(row.try_get("observed_at")?),
        settled_at: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("settled_at")?
            .map(dt_to_ns),
        outcome: row.try_get("outcome")?,
    })
}

/// Store one evidence record for a user.
pub async fn insert_evidence(
    pool: &PgPool,
    user_id: Uuid,
    new: &NewMarkovEvidence,
) -> Result<MarkovEvidenceRow, DbError> {
    let row = sqlx::query(
        "INSERT INTO markov_evidence \
         (user_id, symbol, timeframe, evidence, model_version, horizon_bars, observed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING \
         id, user_id, symbol, timeframe, evidence, model_version, horizon_bars, observed_at, settled_at, outcome",
    )
    .bind(user_id)
    .bind(new.symbol.to_uppercase())
    .bind(&new.timeframe)
    .bind(&new.evidence)
    .bind(&new.model_version)
    .bind(new.horizon_bars)
    .bind(ns_to_dt(new.observed_at))
    .fetch_one(pool)
    .await?;
    from_row(&row)
}

/// Read one record, scoped to its owner.
pub async fn get_evidence(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<MarkovEvidenceRow>, DbError> {
    let row = sqlx::query(&format!("{SELECT} WHERE id = $1 AND user_id = $2"))
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(from_row).transpose()
}

/// List a user's newest records for a symbol.
pub async fn list_evidence(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    limit: i64,
) -> Result<Vec<MarkovEvidenceRow>, DbError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE user_id = $1 AND symbol = $2 ORDER BY observed_at DESC LIMIT $3"
    ))
    .bind(user_id)
    .bind(symbol.to_uppercase())
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

/// List unsettled records oldest-first for settlement work.
pub async fn list_unsettled(
    pool: &PgPool,
    symbol: &str,
    limit: i64,
) -> Result<Vec<MarkovEvidenceRow>, DbError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE symbol = $1 AND settled_at IS NULL ORDER BY observed_at ASC LIMIT $2"
    ))
    .bind(symbol.to_uppercase())
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

/// Settle one record exactly once. Returns `false` when it is already settled
/// or does not belong to the caller, so a retry cannot overwrite an outcome.
pub async fn settle(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    settlement: &Settlement,
) -> Result<bool, DbError> {
    let mut transaction: Transaction<'_, Postgres> = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE markov_evidence SET settled_at = $1, outcome = $2 \
         WHERE id = $3 AND user_id = $4 AND settled_at IS NULL",
    )
    .bind(ns_to_dt(settlement.settled_at))
    .bind(&settlement.outcome)
    .bind(id)
    .bind(user_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok(updated == 1)
}
/// Settle one record from stored candles after its forecast horizon.
///
/// Returns `None` when the row is absent, already settled, or the window has
/// no usable candles. The outcome stays opaque JSON so the scoring formula can
/// change without a migration.
pub async fn settle_with_market(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<Value>, DbError> {
    let Some(row) = get_evidence(pool, user_id, id).await? else {
        return Ok(None);
    };
    if row.settled_at.is_some() {
        return Ok(None);
    }
    let instrument = row
        .evidence
        .get("instrument")
        .and_then(Value::as_str)
        .unwrap_or(&row.symbol)
        .to_string();
    let timeframe_name = row
        .evidence
        .get("timeframe")
        .and_then(Value::as_str)
        .unwrap_or(&row.timeframe)
        .to_string();
    let Ok(timeframe) = timeframe_name.parse::<analytics_core::Timeframe>() else {
        return Ok(None);
    };
    let horizon = row.horizon_bars.unwrap_or(4.0).max(1.0);
    let to_ns = row.observed_at + (horizon * timeframe.nanos() as f64) as i64;
    let timeframes = std::collections::BTreeMap::from([("sec".to_string(), timeframe)]);
    let series = crate::loading::load_timeframe_series(
        pool,
        &instrument,
        &timeframes,
        row.observed_at,
        to_ns,
        timeframe,
    )
    .await?;
    let candles = series.get("sec").map_or(&[][..], Vec::as_slice);
    let first = candles.iter().find(|candle| candle.close.is_finite());
    let last = candles.iter().rev().find(|candle| candle.close.is_finite());
    let (Some(first), Some(last)) = (first, last) else {
        return Ok(None);
    };
    let realized_return = if first.close > 0.0 {
        last.close / first.close - 1.0
    } else {
        0.0
    };
    let regime = row
        .evidence
        .get("current_regime")
        .and_then(Value::as_str)
        .unwrap_or("sideways");
    let direction_hit = match regime {
        "bull_trend" => realized_return > 0.0,
        "bear_trend" => realized_return < 0.0,
        _ => realized_return.abs() < 0.002,
    };
    let outcome = serde_json::json!({
        "realized_return": realized_return,
        "direction_hit": direction_hit,
        "bars_observed": candles.len(),
        "settlement_kind": "close_to_close",
    });
    let settlement = Settlement {
        outcome: outcome.clone(),
        settled_at: last.open_time.max(row.observed_at),
    };
    if settle(pool, user_id, id, &settlement).await? {
        Ok(Some(outcome))
    } else {
        Ok(None)
    }
}

