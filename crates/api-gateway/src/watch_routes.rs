//! `/watch-monitors` and `/watch-events` (docs/48): the zone watcher's
//! standing instructions and their firings.
//!
//! A monitor is an upserted toggle -- one per user+symbol+timeframe -- and
//! events are read-only: the watcher writes them, the user only reads.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::auth::UserContext;
use crate::billing::{enforce_capacity, Resource};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// Body of `POST /watch-monitors`.
#[derive(Debug, Deserialize)]
pub struct CreateMonitorBody {
    /// The market scope's symbol.
    pub symbol: String,
    /// The market scope's resolution.
    pub timeframe: String,
    /// The skill pinned on the triggered run, when one is.
    pub skill_id: Option<String>,
    /// Minutes before the same zone may fire again; the table bounds it.
    pub cooldown_minutes: Option<i32>,
}

/// `GET /watch-monitors`: the user's own monitors, newest first.
pub async fn list_monitors(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let monitors = db::watch::list_watch_monitors(database.pool(), user.user_id).await?;
    Ok(Json(serde_json::json!({ "monitors": monitors })))
}

/// `POST /watch-monitors`: create or refresh the scope's monitor.
pub async fn create_monitor(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<CreateMonitorBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let database = database(&state)?;
    let symbol = body.symbol.trim().to_uppercase();
    let timeframe = body.timeframe.trim().to_string();
    if symbol.is_empty() || timeframe.is_empty() {
        return Err(ApiError::bad_request(
            "SCOPE_INCOMPLETE",
            "a monitor needs a symbol and a timeframe -- a watch with no scope watches nothing",
        ));
    }
    // The resolution must be one the engine speaks, or the watcher fails this
    // monitor every tick and the user learns it nowhere.
    timeframe
        .parse::<analytics_core::Timeframe>()
        .map_err(|_| {
            ApiError::bad_request(
                "BAD_TIMEFRAME",
                &format!("`{timeframe}` is not a resolution the engine accepts"),
            )
        })?;
    let existing =
        db::watch::find_watch_monitor(database.pool(), user.user_id, &symbol, &timeframe).await?;
    match &existing {
        // A brand-new scope is a new watcher, and the saved total is capped.
        None => {
            let saved = db::count_watch_monitors(database.pool(), user.user_id).await?;
            enforce_capacity(&state, user.user_id, Resource::Watchers, saved).await?;
        }
        // Refreshing a live monitor changes its skill, not the watcher count.
        Some(monitor) if monitor.enabled => {}
        // A disabled monitor being switched back on is a watcher waking up,
        // and the enabled total is capped.
        Some(_) => {
            let enabled = db::count_enabled_watch_monitors(database.pool(), user.user_id).await?;
            enforce_capacity(&state, user.user_id, Resource::Watchers, enabled).await?;
        }
    }
    let monitor = db::watch::upsert_watch_monitor(
        database.pool(),
        user.user_id,
        &symbol,
        &timeframe,
        body.skill_id.as_deref(),
        body.cooldown_minutes.unwrap_or(60),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "monitor": monitor })),
    ))
}

/// Body of `PATCH /watch-monitors/{id}`.
#[derive(Debug, Deserialize)]
pub struct PatchMonitorBody {
    /// The new state.
    pub enabled: bool,
}

/// `PATCH /watch-monitors/{id}`: pause or resume a monitor.
pub async fn patch_monitor(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<uuid::Uuid>,
    ApiJson(body): ApiJson<PatchMonitorBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    if body.enabled {
        if let Some(existing) = db::watch::list_watch_monitors(database.pool(), user.user_id)
            .await?
            .into_iter()
            .find(|monitor| monitor.id == id)
        {
            if !existing.enabled {
                let enabled =
                    db::count_enabled_watch_monitors(database.pool(), user.user_id).await?;
                enforce_capacity(&state, user.user_id, Resource::Watchers, enabled).await?;
            }
        }
    }
    let monitor =
        db::watch::set_watch_monitor_enabled(database.pool(), user.user_id, id, body.enabled)
            .await?
            .ok_or_else(|| ApiError::not_found("watch monitor not found"))?;
    Ok(Json(serde_json::json!({ "monitor": monitor })))
}

/// `DELETE /watch-monitors/{id}`: stop watching, and the events go with it.
pub async fn delete_monitor(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, ApiError> {
    let database = database(&state)?;
    if !db::watch::delete_watch_monitor(database.pool(), user.user_id, id).await? {
        return Err(ApiError::not_found("watch monitor not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The events panel's query.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// The most events to return; bounded either way.
    pub limit: Option<i64>,
}

/// `GET /watch-events`: the user's recent firings, newest first.
pub async fn list_events(
    State(state): State<AppState>,
    user: UserContext,
    Query(query): Query<EventsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let events = db::watch::list_watch_events(
        database.pool(),
        user.user_id,
        query.limit.unwrap_or(30).clamp(1, 100),
    )
    .await?;
    Ok(Json(serde_json::json!({ "events": events })))
}

/// The database, or the honest 503 every other route makes when it is absent.
fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("the database is not configured"))
}
