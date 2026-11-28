//! Durable Core Analytics predictions and reliability reporting.
//!
//! The Rust core owns the arithmetic; these routes only persist decisions,
//! settle them against realized market data, and report what the core computed.
//! A prediction row is never rewritten after settlement.

use analytics_core::hierarchy::Direction;
use analytics_core::validation::ForwardOutcome;
use analytics_core::{
    detect_drift, evaluate_promotion, DriftConfig, OutcomeLog, PredictionRecord, Timeframe,
};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::market_data::WindowMarketData;
use crate::AppState;

use ai_agent::MarketDataSource;

const MAX_PREDICTIONS: i64 = 1000;

/// Body of `POST /analytics/opportunities`.
#[derive(Debug, Deserialize)]
pub struct RecordOpportunityBody {
    /// Symbol.
    pub symbol: String,
    /// Resolution of the representation.
    pub timeframe: String,
    /// Direction at decision time.
    pub direction: String,
    /// Packed representation signature, 0..=255.
    pub signature: i16,
    /// Evidence strength in `[0, 1]`.
    pub evidence_strength: f64,
    /// Decision instant, milliseconds since the epoch.
    pub observed_at_ms: i64,
}

/// Body of `POST /analytics/opportunities/settle`.
#[derive(Debug, Deserialize)]
pub struct SettleOpportunityBody {
    /// Row id.
    pub id: Uuid,
    /// Realized forward return.
    pub realized_return: f64,
    /// Whether the directional call was correct.
    pub hit: bool,
    /// Settlement instant, milliseconds since the epoch.
    pub settled_at_ms: i64,
}

/// Query of `GET /analytics/opportunities`.
#[derive(Debug, Deserialize)]
pub struct OpportunitiesQuery {
    /// Filter by symbol.
    pub symbol: Option<String>,
    /// Filter by settlement state.
    pub settled: Option<bool>,
    /// Row limit.
    pub limit: Option<i64>,
}

/// Body of `POST /analytics/predictions`.
#[derive(Debug, Deserialize)]
pub struct RecordPredictionBody {
    /// Symbol the call was made on.
    pub symbol: String,
    /// Resolution the call describes.
    pub timeframe: String,
    /// Discipline that made the call.
    pub discipline: String,
    /// Direction spelling from the standard result types.
    pub direction: String,
    /// Evidence strength in `[0, 1]`; never a probability.
    pub evidence_strength: f64,
    /// Model or configuration version.
    pub model_version: String,
    /// Decision instant, milliseconds since the epoch.
    pub decision_timestamp_ms: i64,
}

/// Body of `POST /analytics/predictions/settle`.
#[derive(Debug, Deserialize)]
pub struct SettleBody {
    /// Limit settlement to one symbol, when supplied.
    pub symbol: Option<String>,
    /// Forward horizon in milliseconds.
    pub horizon_ms: i64,
    /// Absolute return below which an outcome is neutral.
    pub neutral_threshold: Option<f64>,
}

/// Query of `GET /analytics/predictions`.
#[derive(Debug, Deserialize)]
pub struct PredictionsQuery {
    /// Row limit, capped by the handler.
    pub limit: Option<i64>,
    /// Only rows awaiting an outcome.
    pub unsettled: Option<bool>,
}

/// Query of `GET /analytics/drift`.
#[derive(Debug, Deserialize)]
pub struct DriftQuery {
    /// Operator consent for a promotion recommendation.
    pub operator_authorized: Option<bool>,
}

fn database<'a>(state: &'a AppState) -> Result<&'a std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("the database is not configured"))
}

fn direction_of(raw: &str) -> Result<Direction, ApiError> {
    match raw {
        "Bullish" => Ok(Direction::Bullish),
        "Bearish" => Ok(Direction::Bearish),
        "Neutral" => Ok(Direction::Neutral),
        other => Err(ApiError::bad_request(
            "unknown_direction",
            format!("`{other}` is not a Core Analytics direction"),
        )),
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Bullish => "Bullish",
        Direction::Bearish => "Bearish",
        Direction::Neutral => "Neutral",
    }
}

fn timeframe_of(raw: &str) -> Result<Timeframe, ApiError> {
    raw.parse::<Timeframe>().map_err(|_| {
        ApiError::bad_request(
            "unknown_timeframe",
            format!("`{raw}` is not a timeframe this platform supports"),
        )
    })
}

/// Record a prediction at decision time, before the outcome is known.
pub async fn record_prediction(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    ApiJson(body): ApiJson<RecordPredictionBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let timeframe = timeframe_of(&body.timeframe)?;
    direction_of(&body.direction)?;
    if !body.evidence_strength.is_finite() || !(0.0..=1.0).contains(&body.evidence_strength) {
        return Err(ApiError::bad_request(
            "invalid_evidence_strength",
            "evidence strength must be finite and within [0, 1]",
        ));
    }
    let row = db::insert_core_prediction(
        database(&state)?.pool(),
        user.user_id,
        &db::NewCorePrediction {
            symbol: body.symbol,
            timeframe: timeframe.as_str().to_string(),
            discipline: body.discipline,
            direction: body.direction,
            evidence_strength: body.evidence_strength,
            model_version: body.model_version,
            decision_at: body.decision_timestamp_ms.saturating_mul(1_000_000),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "prediction": row }))))
}

/// List a user's recorded predictions.
pub async fn list_predictions(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    Query(query): Query<PredictionsQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = db::list_core_predictions(
        database(&state)?.pool(),
        user.user_id,
        query.limit.unwrap_or(100).clamp(1, MAX_PREDICTIONS),
        query.unsettled.unwrap_or(false),
    )
    .await?;
    Ok(Json(json!({ "predictions": rows })))
}

/// Settle matured predictions from market data, once each.
pub async fn settle_predictions(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    ApiJson(body): ApiJson<SettleBody>,
) -> Result<Json<Value>, ApiError> {
    if body.horizon_ms <= 0 {
        return Err(ApiError::bad_request(
            "invalid_horizon",
            "the settlement horizon must be positive",
        ));
    }
    let threshold = body.neutral_threshold.unwrap_or(0.002);
    if !threshold.is_finite() || threshold <= 0.0 {
        return Err(ApiError::bad_request(
            "invalid_neutral_threshold",
            "the neutral threshold must be a positive finite number",
        ));
    }
    let pool = database(&state)?.pool();
    let rows = db::list_core_predictions(pool, user.user_id, MAX_PREDICTIONS, true).await?;
    let data = WindowMarketData::new(state.windows.clone());
    let horizon_ns = body.horizon_ms.saturating_mul(1_000_000);
    let mut settled = 0usize;
    let mut skipped = 0usize;

    for row in rows {
        if let Some(symbol) = &body.symbol {
            if !symbol.trim().is_empty() && row.symbol != symbol.trim().to_uppercase() {
                continue;
            }
        }
        let Ok(timeframe) = row.timeframe.parse::<Timeframe>() else {
            skipped += 1;
            continue;
        };
        let from = row
            .decision_at
            .saturating_sub(timeframe.nanos().saturating_mul(2));
        let to = row
            .decision_at
            .saturating_add(horizon_ns)
            .saturating_add(timeframe.nanos());
        let candles = match data.candles(&row.symbol, timeframe, from, to).await {
            Ok(candles) => candles,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(outcome) = ForwardOutcome::label(&candles, row.decision_at, horizon_ns, threshold)
        else {
            skipped += 1;
            continue;
        };
        let direction = direction_of(&row.direction)?;
        let hit = direction != Direction::Neutral && direction == outcome.direction;
        let result = db::settle_core_prediction(
            pool,
            user.user_id,
            row.id,
            outcome.return_fraction,
            direction_name(outcome.direction),
            hit,
            row.decision_at.saturating_add(horizon_ns),
        )
        .await?;
        if result.is_some() {
            settled += 1;
        } else {
            skipped += 1;
        }
    }

    Ok(Json(json!({ "settled": settled, "skipped": skipped })))
}

fn rebuild_log(rows: &[db::CorePredictionRow]) -> Result<OutcomeLog, ApiError> {
    let mut log = OutcomeLog::new();
    for row in rows {
        let timeframe = timeframe_of(&row.timeframe)?;
        let prediction = PredictionRecord {
            instrument_id: row.symbol.clone(),
            decision_timestamp: row.decision_at,
            timeframe,
            direction: direction_of(&row.direction)?,
            evidence_strength: row.evidence_strength,
            model_version: row.model_version.clone(),
        };
        log.record(prediction).map_err(ApiError::internal)?;
        if let (Some(realized_return), Some(realized_direction)) =
            (row.realized_return, &row.realized_direction)
        {
            let outcome = ForwardOutcome {
                decision_time: row.decision_at,
                return_fraction: realized_return,
                direction: direction_of(realized_direction)?,
            };
            let key = (row.symbol.clone(), row.decision_at, timeframe);
            log.settle_with_outcome(key, &outcome)
                .map_err(ApiError::internal)?;
        }
    }
    Ok(log)
}

/// Store an opportunity representation for later recall.
pub async fn record_opportunity(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    ApiJson(body): ApiJson<RecordOpportunityBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    timeframe_of(&body.timeframe)?;
    direction_of(&body.direction)?;
    if !(0..=255).contains(&body.signature) {
        return Err(ApiError::bad_request(
            "invalid_signature",
            "a representation signature must be within [0, 255]",
        ));
    }
    if !body.evidence_strength.is_finite() || !(0.0..=1.0).contains(&body.evidence_strength) {
        return Err(ApiError::bad_request(
            "invalid_evidence_strength",
            "evidence strength must be finite and within [0, 1]",
        ));
    }
    let row = db::insert_memory_opportunity(
        database(&state)?.pool(),
        user.user_id,
        &db::NewOpportunity {
            symbol: body.symbol,
            timeframe: body.timeframe,
            direction: body.direction,
            signature: body.signature,
            evidence_strength: body.evidence_strength,
            observed_at: body.observed_at_ms.saturating_mul(1_000_000),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "opportunity": row }))))
}

/// List stored opportunities.
pub async fn list_opportunities(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    Query(query): Query<OpportunitiesQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = db::list_memory_opportunities(
        database(&state)?.pool(),
        user.user_id,
        query.symbol.as_deref(),
        query.settled,
        query.limit.unwrap_or(100).clamp(1, MAX_PREDICTIONS),
    )
    .await?;
    Ok(Json(json!({ "opportunities": rows })))
}

/// Settle an opportunity once from realized data.
pub async fn settle_opportunity(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    ApiJson(body): ApiJson<SettleOpportunityBody>,
) -> Result<Json<Value>, ApiError> {
    if !body.realized_return.is_finite() {
        return Err(ApiError::bad_request(
            "invalid_realized_return",
            "the realized return must be finite",
        ));
    }
    let row = db::settle_memory_opportunity(
        database(&state)?.pool(),
        user.user_id,
        body.id,
        body.realized_return,
        body.hit,
        body.settled_at_ms.saturating_mul(1_000_000),
    )
    .await?;
    Ok(Json(
        json!({ "opportunity": row, "settled": row.is_some() }),
    ))
}

/// Report drift and a promotion recommendation from settled outcomes.
pub async fn drift_report(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    Query(query): Query<DriftQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = db::list_core_predictions(
        database(&state)?.pool(),
        user.user_id,
        MAX_PREDICTIONS,
        false,
    )
    .await?;
    let log = rebuild_log(&rows)?;
    let drift = detect_drift(&log, &DriftConfig::default());
    let promotion = evaluate_promotion(
        &log,
        &DriftConfig::default(),
        query.operator_authorized.unwrap_or(false),
    );
    Ok(Json(json!({
        "drift": drift,
        "promotion": promotion,
        "settled": log.settled().len(),
        "note": "a promotion is a recommendation only: an untouched holdout and risk review remain mandatory"
    })))
}
