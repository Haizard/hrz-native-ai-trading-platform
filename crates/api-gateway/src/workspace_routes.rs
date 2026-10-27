//! `/workspace`: the shell's durable workspace (`docs/54`).
//!
//! The server stores one opaque JSON object and enforces three things: it is
//! an object, it fits the size bound, and it belongs to the caller. The shape
//! inside is the shell's, because the shell is the only writer and the only
//! reader -- a second interpretation here would drift from the page.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// The largest snapshot stored, in bytes.
const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
/// The largest number of panes a snapshot may describe.
const MAX_PANES: usize = 100;
/// The only snapshot version this release writes.
const STATE_VERSION: i64 = 1;

/// `GET /workspace`
#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    /// The snapshot, or null when nothing has been saved.
    pub state: Option<serde_json::Value>,
}

/// `PUT /workspace`'s body.
#[derive(Debug, Deserialize)]
pub struct SaveWorkspaceBody {
    /// The snapshot.
    pub state: serde_json::Value,
}

/// `PUT /workspace`'s response.
#[derive(Debug, Serialize)]
pub struct SavedResponse {
    /// Always true; a failed save is an error response.
    pub saved: bool,
    /// Unix milliseconds.
    pub updated_at: i64,
}

fn database(state: &AppState) -> Result<Arc<db::Database>, ApiError> {
    state.db.clone().ok_or_else(|| {
        ApiError::unavailable("the workspace store needs the database; it is not configured")
    })
}

fn validate(state: &serde_json::Value) -> Result<(), ApiError> {
    let Some(object) = state.as_object() else {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            "the workspace state is a JSON object",
        ));
    };
    if object.get("version").and_then(|v| v.as_i64()) != Some(STATE_VERSION) {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("the workspace state is version {STATE_VERSION}"),
        ));
    }
    let panes = object.get("panes").and_then(|v| v.as_array()).ok_or_else(|| {
        ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            "the workspace state carries a `panes` list",
        )
    })?;
    if panes.is_empty() || panes.len() > MAX_PANES {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("a workspace has 1-{MAX_PANES} panes"),
        ));
    }
    let bytes = serde_json::to_vec(state)
        .map_err(|error| ApiError::bad_request("WORKSPACE_STATE_INVALID", error.to_string()))?
        .len();
    if bytes > MAX_STATE_BYTES {
        return Err(ApiError::bad_request(
            "WORKSPACE_STATE_INVALID",
            format!("the workspace state is {bytes} bytes; the limit is {MAX_STATE_BYTES}"),
        ));
    }
    Ok(())
}

/// `GET /workspace`
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let database = database(&state)?;
    let saved = db::get_workspace_state(database.pool(), user.user_id).await?;
    Ok(Json(WorkspaceResponse { state: saved }))
}

/// `PUT /workspace`
pub async fn put(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<SaveWorkspaceBody>,
) -> Result<Json<SavedResponse>, ApiError> {
    let database = database(&state)?;
    validate(&body.state)?;
    let updated_at = db::put_workspace_state(database.pool(), user.user_id, &body.state).await?;
    Ok(Json(SavedResponse {
        saved: true,
        updated_at: updated_at / 1_000_000,
    }))
}
