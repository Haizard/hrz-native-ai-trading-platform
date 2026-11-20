//! `/markov/evidence/{id}/settle` — score a recorded regime call after its
//! forecast horizon (`docs/62`).
//!
//! Settlement is deliberately separate from recording: a prediction and its
//! outcome must never be written in the same request, or reliability becomes
//! unfalsifiable. The window is computed from stored candles by the db crate,
//! so the same number is available to a scheduled job and to this route.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::AppState;

/// Settle one of the caller's evidence records from stored market data.
pub async fn settle_evidence(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("this deployment has no evidence store"))?;
    let outcome = db::markov_evidence::settle_with_market(database.pool(), user.user_id, id)
        .await
        .map_err(|error| ApiError::internal(format!("could not settle the evidence: {error}")))?
        .ok_or_else(|| {
            ApiError::coded(
                axum::http::StatusCode::CONFLICT,
                "EVIDENCE_NOT_SETTLEABLE",
                "the record is unknown, already settled, or its horizon has no candles",
            )
        })?;
    Ok(Json(json!({ "id": id, "outcome": outcome })))
}
