//! Authenticated persistence endpoints for AI-generated indicator workspaces.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

const DEFAULT_LIMIT: i64 = 50;

/// Body of `POST /indicator-workspaces`.
#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceBody {
    /// Human-readable workspace name.
    pub name: String,
    /// Chart market, e.g. BTCUSDT.
    pub symbol: String,
    /// Primary chart timeframe.
    pub timeframe: String,
}

/// A revision produced by the workspace AI after it has generated and previewed
/// source. The client may display it but cannot mark an invalid preview active.
#[derive(Debug, Deserialize)]
pub struct CreateRevisionBody {
    pub parent_revision_id: Option<Uuid>,
    pub source: String,
    pub summary: String,
    pub change_summary: String,
    pub preview: chart_engine::IndicatorOutput,
}

/// An owned workspace as a client sees it.
#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub id: String,
    pub name: String,
    pub symbol: String,
    pub timeframe: String,
    pub memory: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_revision_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<db::IndicatorWorkspaceRow> for WorkspaceResponse {
    fn from(row: db::IndicatorWorkspaceRow) -> Self {
        Self {
            id: row.id.to_string(), name: row.name, symbol: row.symbol, timeframe: row.timeframe,
            memory: row.memory, active_revision_id: row.active_revision_id.map(|id| id.to_string()),
            created_at: row.created_at, updated_at: row.updated_at,
        }
    }
}

/// A source revision. Source remains read-only because this route only reads it.
#[derive(Debug, Serialize)]
pub struct RevisionResponse {
    pub id: String,
    pub revision_number: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_revision_id: Option<String>,
    pub source: String,
    pub summary: String,
    pub change_summary: String,
    pub validation: serde_json::Value,
    pub preview: serde_json::Value,
    pub status: String,
    pub created_at: i64,
}

impl From<db::IndicatorRevisionRow> for RevisionResponse {
    fn from(row: db::IndicatorRevisionRow) -> Self {
        Self { id: row.id.to_string(), revision_number: row.revision_number,
            parent_revision_id: row.parent_revision_id.map(|id| id.to_string()), source: row.source,
            summary: row.summary, change_summary: row.change_summary, validation: row.validation,
            preview: row.preview, status: row.status, created_at: row.created_at }
    }
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state.db.as_ref().ok_or_else(|| ApiError::unavailable(
        "no database configured; indicator workspaces cannot be stored",
    ))
}

/// `GET /indicator-workspaces`.
pub async fn list(State(state): State<AppState>, user: UserContext) -> Result<Json<Vec<WorkspaceResponse>>, ApiError> {
    let rows = db::list_indicator_workspaces(database(&state)?.pool(), user.user_id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces`.
pub async fn create(State(state): State<AppState>, user: UserContext, ApiJson(body): ApiJson<CreateWorkspaceBody>) -> Result<Json<WorkspaceResponse>, ApiError> {
    for (field, value) in [("name", &body.name), ("symbol", &body.symbol), ("timeframe", &body.timeframe)] {
        if value.trim().is_empty() { return Err(ApiError::bad_request("WORKSPACE_FIELD_REQUIRED", format!("{field} must not be empty"))); }
    }
    let database = database(&state)?;
    let id = db::create_indicator_workspace(database.pool(), user.user_id, &body.name, &body.symbol.to_uppercase(), &body.timeframe, &serde_json::json!({})).await?;
    let row = db::get_indicator_workspace(database.pool(), user.user_id, id).await?
        .ok_or_else(|| ApiError::internal("new indicator workspace was not readable"))?;
    Ok(Json(row.into()))
}

/// `GET /indicator-workspaces/{id}/revisions`.
pub async fn revisions(State(state): State<AppState>, user: UserContext, Path(id): Path<Uuid>) -> Result<Json<Vec<RevisionResponse>>, ApiError> {
    let database = database(&state)?;
    if db::get_indicator_workspace(database.pool(), user.user_id, id).await?.is_none() {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    let rows = db::list_indicator_revisions(database.pool(), user.user_id, id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces/{id}/revisions`.
///
/// This is the gate between AI generation and chart attachment: output is
/// validated server-side and a refused result is persisted for explanation but
/// cannot replace the workspace's attached revision.
pub async fn create_revision(
    State(state): State<AppState>, user: UserContext, Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateRevisionBody>,
) -> Result<Json<RevisionResponse>, ApiError> {
    if body.source.trim().is_empty() || body.summary.trim().is_empty() || body.change_summary.trim().is_empty() {
        return Err(ApiError::bad_request("INDICATOR_REVISION_FIELD_REQUIRED", "source, summary and change_summary must not be empty"));
    }
    let database = database(&state)?;
    let valid = body.preview.validate();
    let (status, validation) = match valid {
        Ok(()) => ("validated", serde_json::json!({"valid": true, "engine": "indicator-output-v1"})),
        Err(reason) => ("rejected", serde_json::json!({"valid": false, "reason": reason, "engine": "indicator-output-v1"})),
    };
    let preview = serde_json::to_value(&body.preview)
        .map_err(|e| ApiError::internal(format!("could not store indicator preview: {e}")))?;
    let row = db::create_indicator_revision(
        database.pool(), user.user_id, id, body.parent_revision_id, &body.source,
        &body.summary, &body.change_summary, &validation, &preview, status,
    ).await?.ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    Ok(Json(row.into()))
}
