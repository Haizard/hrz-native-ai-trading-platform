//! Authenticated persistence endpoints for AI-generated indicator workspaces.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use trading_engine::Decisions;
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

/// Body of `POST /indicator-workspaces/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct CreateMessageBody {
    /// The client instruction for the next indicator revision.
    pub content: String,
    /// Optional skill to pin while Bedrock generates the underlying DSL.
    pub skill_id: Option<String>,
    /// Optional screenshots to read the indicator off -- the "start anywhere"
    /// path. Bounded; see `MAX_MESSAGE_IMAGES`.
    #[serde(default)]
    pub images: Vec<MessageImage>,
}

/// The most images one workspace message may carry.
///
/// A chart draft is one image; two is generous. More than four is somebody
/// stuffing the request, and each image is real base64 in the LLM context.
const MAX_MESSAGE_IMAGES: usize = 4;

/// The media types the provider accepts -- the agent's own list, by reference
/// rather than by copy, so the two cannot drift.
const MESSAGE_IMAGE_TYPES: &[&str] = &ai_agent::chart_context::SCREENSHOT_MEDIA_TYPES;

/// A durable workspace message.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub role: String,
    pub kind: String,
    pub content: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

/// Result of one Bedrock-backed workspace turn.
#[derive(Debug, Serialize)]
pub struct WorkspaceTurnResponse {
    pub user_message: MessageResponse,
    pub assistant_message: MessageResponse,
    pub revision: RevisionResponse,
    pub strategy_id: String,
}

impl From<db::IndicatorWorkspaceMessageRow> for MessageResponse {
    fn from(row: db::IndicatorWorkspaceMessageRow) -> Self {
        Self {
            id: row.id.to_string(),
            role: row.role,
            kind: row.kind,
            content: row.content,
            payload: row.payload,
            created_at: row.created_at,
        }
    }
}

/// A client-facing alert preference response.
#[derive(Debug, Serialize)]
pub struct AlertPreferenceResponse {
    pub event_name: String,
    pub enabled: bool,
    pub channels: serde_json::Value,
}

impl From<db::IndicatorAlertPreference> for AlertPreferenceResponse {
    fn from(row: db::IndicatorAlertPreference) -> Self {
        Self {
            event_name: row.event_name,
            enabled: row.enabled,
            channels: row.channels,
        }
    }
}

/// An image attached to a workspace message.
///
/// Base64 data only: the shell reads the file itself and sends bytes, the
/// same way `agent_routes` takes screenshots from the chart capture.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageImage {
    /// Media type. Only what the provider accepts travels further.
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

/// Body of the alert preference endpoint.
#[derive(Debug, Deserialize)]
pub struct SetAlertBody {
    pub revision_id: Uuid,
    pub event_name: String,
    pub enabled: bool,
    #[serde(default = "default_channels")]
    pub channels: serde_json::Value,
}

fn default_channels() -> serde_json::Value {
    serde_json::json!([])
}

/// Body of the revision-pinned bot draft endpoint.
#[derive(Debug, Deserialize)]
pub struct CreateBotDraftBody {
    pub revision_id: Uuid,
    pub strategy_id: Uuid,
    pub backtest_id: Option<Uuid>,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub venue: Option<String>,
    #[serde(default = "default_risk")]
    pub risk: serde_json::Value,
}

fn default_mode() -> String {
    "paper".to_string()
}
fn default_risk() -> serde_json::Value {
    serde_json::json!({})
}

/// A revision-pinned bot draft.
#[derive(Debug, Serialize)]
pub struct BotDraftResponse {
    pub id: String,
    pub revision_id: String,
    pub strategy_id: String,
    pub backtest_id: Option<String>,
    pub mode: String,
    pub venue: Option<String>,
    pub risk: serde_json::Value,
    pub status: String,
    pub created_at: i64,
}

impl From<db::IndicatorBotDraftRow> for BotDraftResponse {
    fn from(row: db::IndicatorBotDraftRow) -> Self {
        Self {
            id: row.id.to_string(),
            revision_id: row.revision_id.to_string(),
            strategy_id: row.strategy_id.to_string(),
            backtest_id: row.backtest_id.map(|id| id.to_string()),
            mode: row.mode,
            venue: row.venue,
            risk: row.risk,
            status: row.status,
            created_at: row.created_at,
        }
    }
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
            id: row.id.to_string(),
            name: row.name,
            symbol: row.symbol,
            timeframe: row.timeframe,
            memory: row.memory,
            active_revision_id: row.active_revision_id.map(|id| id.to_string()),
            created_at: row.created_at,
            updated_at: row.updated_at,
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
        Self {
            id: row.id.to_string(),
            revision_number: row.revision_number,
            parent_revision_id: row.parent_revision_id.map(|id| id.to_string()),
            source: row.source,
            summary: row.summary,
            change_summary: row.change_summary,
            validation: row.validation,
            preview: row.preview,
            status: row.status,
            created_at: row.created_at,
        }
    }
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable("no database configured; indicator workspaces cannot be stored")
    })
}

/// `GET /indicator-workspaces`.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<WorkspaceResponse>>, ApiError> {
    let rows = db::list_indicator_workspaces(database(&state)?.pool(), user.user_id, DEFAULT_LIMIT)
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces`.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<CreateWorkspaceBody>,
) -> Result<(StatusCode, Json<WorkspaceResponse>), ApiError> {
    for (field, value) in [
        ("name", &body.name),
        ("symbol", &body.symbol),
        ("timeframe", &body.timeframe),
    ] {
        if value.trim().is_empty() {
            return Err(ApiError::bad_request(
                "WORKSPACE_FIELD_REQUIRED",
                format!("{field} must not be empty"),
            ));
        }
    }
    let database = database(&state)?;
    let id = db::create_indicator_workspace(
        database.pool(),
        user.user_id,
        &body.name,
        &body.symbol.to_uppercase(),
        &body.timeframe,
        &serde_json::json!({}),
    )
    .await?;
    let row = db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::internal("new indicator workspace was not readable"))?;
    Ok((StatusCode::CREATED, Json(row.into())))
}

/// `GET /indicator-workspaces/{id}/revisions`.
pub async fn revisions(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RevisionResponse>>, ApiError> {
    let database = database(&state)?;
    if db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    let rows =
        db::list_indicator_revisions(database.pool(), user.user_id, id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces/{id}/revisions`.
///
/// This is the gate between AI generation and chart attachment: output is
/// validated server-side and a refused result is persisted for explanation but
/// cannot replace the workspace's attached revision.
pub async fn create_revision(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateRevisionBody>,
) -> Result<(StatusCode, Json<RevisionResponse>), ApiError> {
    if body.source.trim().is_empty()
        || body.summary.trim().is_empty()
        || body.change_summary.trim().is_empty()
    {
        return Err(ApiError::bad_request(
            "INDICATOR_REVISION_FIELD_REQUIRED",
            "source, summary and change_summary must not be empty",
        ));
    }
    // The source itself must parse as a strategy document: a revision is a
    // runnable unit, and storing YAML the engine will refuse would turn the
    // code panel into a place where broken documents look saved. The failure
    // is a 422 naming the validator's first issue, which the editor shows.
    let source_valid = strategy_dsl::parse_and_validate(&body.source);
    let database = database(&state)?;
    let valid = match (&source_valid, body.preview.validate()) {
        (Err(err), _) => Err(err.to_string()),
        (Ok(_), preview) => preview,
    };
    let (status, validation) = match valid {
        Ok(()) => (
            "validated",
            serde_json::json!({"valid": true, "engine": "indicator-output-v1"}),
        ),
        Err(reason) => (
            "rejected",
            serde_json::json!({"valid": false, "reason": reason, "engine": "indicator-output-v1"}),
        ),
    };
    let preview = serde_json::to_value(&body.preview)
        .map_err(|e| ApiError::internal(format!("could not store indicator preview: {e}")))?;
    let row = db::create_indicator_revision(
        database.pool(),
        user.user_id,
        id,
        body.parent_revision_id,
        &body.source,
        &body.summary,
        &body.change_summary,
        &validation,
        &preview,
        status,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    Ok((StatusCode::CREATED, Json(row.into())))
}

/// `GET /indicator-workspaces/{id}/messages`.
pub async fn messages(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<MessageResponse>>, ApiError> {
    let rows = db::list_indicator_workspace_messages(
        database(&state)?.pool(),
        user.user_id,
        id,
        DEFAULT_LIMIT,
    )
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces/{id}/messages`.
///
/// Stores the user's instruction before asking Bedrock, so a provider failure is
/// visible in the durable transcript rather than silently disappearing.
pub async fn create_message(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateMessageBody>,
) -> Result<(StatusCode, Json<WorkspaceTurnResponse>), ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError::bad_request(
            "WORKSPACE_MESSAGE_REQUIRED",
            "message content must not be empty",
        ));
    }
    let database = database(&state)?;
    let workspace = db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    crate::agent_routes::check_agent_limit(&state, &user)?;
    let user_message = db::create_indicator_workspace_message(
        database.pool(),
        user.user_id,
        id,
        "user",
        "message",
        body.content.trim(),
        // The image count, not the bytes: base64 payloads do not belong in a
        // transcript, but "this request had a screenshot" belongs in history.
        &serde_json::json!({"skill_id": body.skill_id, "image_count": body.images.len()}),
    )
    .await?
    .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let agent = state.agent.as_ref().ok_or_else(|| ApiError::unavailable("the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID and AWS credentials"))?;
    // Screenshots: bounded and type-checked before anything expensive runs.
    // A rejected image is a 400 the shell can show, not a silent drop -- a
    // user who pasted a chart and got a text-only reading would not know why.
    if body.images.len() > MAX_MESSAGE_IMAGES {
        return Err(ApiError::bad_request(
            "TOO_MANY_IMAGES",
            &format!("a message may carry at most {MAX_MESSAGE_IMAGES} images"),
        ));
    }
    let mut screenshots = Vec::new();
    for image in &body.images {
        if !MESSAGE_IMAGE_TYPES.contains(&image.media_type.as_str()) {
            return Err(ApiError::bad_request(
                "UNSUPPORTED_IMAGE_TYPE",
                &format!(
                    "{} is not accepted; use one of {}",
                    image.media_type,
                    MESSAGE_IMAGE_TYPES.join(", ")
                ),
            ));
        }
        screenshots.push(ai_agent::chart_context::ChartScreenshot {
            media_type: image.media_type.clone(),
            data: image.data.clone(),
            label: Some("user-attached chart screenshot".to_string()),
        });
    }
    let memory = workspace.memory.to_string();
    let description = format!(
        "Workspace: {}. Existing compact memory: {memory}. Client request: {}. IMPORTANT: this workspace generates `kind: indicator` detector documents only — the user asked for an indicator, so the document MUST declare `kind: indicator` with `concepts` (the patterns to detect) and MUST NOT have entry/risk blocks; a `kind: strategy` here is a wrong answer even if it validates. IMPORTANT: timeframes.entry MUST be exactly `{}` — no other value is acceptable.",
        workspace.name,
        body.content.trim(),
        workspace.timeframe
    );
    let mut request =
        ai_agent::StrategyRequest::new(description, &workspace.symbol, &workspace.timeframe);
    request.skill_id = body.skill_id.clone();
    for shot in screenshots {
        request = request.with_image(shot);
    }
    // Iterative editing: a workspace with an active revision revises THAT
    // document instead of starting over -- the model sees the current source
    // and the edit-mode prompt, so "make the bands tighter" tightens the bands
    // and does not lose the user's other concepts.
    if let Some(active_id) = workspace.active_revision_id {
        if let Some(active) =
            db::get_indicator_revision(database.pool(), user.user_id, id, active_id).await?
        {
            request = request.with_base_document(active.source);
        }
    }
    // Give the model extra retry attempts — it often ignores the entry
    // timeframe on the first pass and needs the feedback loop to correct.
    request.max_attempts = Some(5);
    let generated = agent
        .generate_strategy(&request)
        .await
        .map_err(ApiError::from)?;
    let document = generated.document().clone();
    let yaml = generated.yaml.clone();
    let attempts = generated.attempts;
    let repaired_errors = generated.repaired_errors.clone();
    let validated = generated.into_validated();
    // Only sandbox-check tradable documents. `kind: indicator` documents
    // have no entry/risk blocks by design and the sandbox rightfully refuses
    // them — skipping is correct, not a safety gap.
    if document.kind != strategy_dsl::DocumentKind::Indicator {
        Decisions::sandboxed(state.sandbox.as_ref(), &validated).map_err(|err| {
            ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INDICATOR_SANDBOX_REFUSED",
                err.to_string(),
            )
        })?;
    }
    let strategy = serde_json::to_value(&document)
        .map_err(|err| ApiError::internal(format!("could not store generated strategy: {err}")))?;
    let strategy_id = db::create_strategy(
        database.pool(),
        user.user_id,
        &document.name,
        &document.version,
        &strategy,
        "ai_agent",
    )
    .await?;

    // Replay the document and turn the signals it actually emitted into the
    // chart's evidence chain. A replay that cannot run (no stored candles, a
    // declared timeframe with no data) is not a reason to discard a validated
    // revision, so the failure is recorded and an honest empty preview is kept
    // rather than a chart that quietly claims evidence it does not have.
    let revision_tag = strategy_id.to_string();
    let (preview, preview_note, preview_stats) = match crate::indicator_preview::replay_preview(
        &state,
        database,
        &workspace.symbol,
        &workspace.timeframe,
        &document,
        &validated,
    )
    .await
    {
        Ok((mut replayed, stats)) => {
            replayed.revision_id = revision_tag.clone();
            (replayed, serde_json::Value::Null, stats)
        }
        Err(reason) => (
            chart_engine::IndicatorOutput {
                revision_id: revision_tag.clone(),
                // Even a replay that could not run stores the document's
                // concepts: the definition is what lets a chart re-detect the
                // layer live later, on data the generator never saw.
                name: Some(document.name.clone()),
                concepts: document.concepts.clone(),
                evidence: Vec::new(),
                zones: Vec::new(),
                markers: Vec::new(),
                links: Vec::new(),
                trendlines: Vec::new(),
            },
            serde_json::Value::String(reason),
            None,
        ),
    };
    let preview_json = serde_json::to_value(&preview)
        .map_err(|err| ApiError::internal(format!("could not store indicator preview: {err}")))?;
    let validation = serde_json::json!({"valid": true, "engine": "strategy-dsl-wasm-v1", "strategy_id": strategy_id, "attempts": attempts, "repaired_errors": repaired_errors, "preview_note": preview_note, "evidence_nodes": preview.evidence.len()});
    let revision = db::create_indicator_revision(
        database.pool(),
        user.user_id,
        id,
        workspace.active_revision_id,
        &yaml,
        &format!("Generated strategy {}", document.name),
        "Generated from workspace message",
        &validation,
        &preview_json,
        "validated",
    )
    .await?
    .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let memory = serde_json::json!({"last_request": body.content.trim(), "active_strategy_id": strategy_id, "revision": revision.revision_number});
    db::update_indicator_workspace_memory(database.pool(), user.user_id, id, &memory).await?;
    let assistant_text = if document.kind == strategy_dsl::DocumentKind::Indicator {
        let zones = preview.zones.len();
        let markers = preview.markers.len();
        let lines = preview.trendlines.len();
        // A trendline-shaped concept never produces bands -- its evidence is
        // the fitted line -- so an all-trendline document is expected to show
        // zero zones and must not be reported as a failure to detect.
        let wanted_lines = document
            .concepts
            .iter()
            .filter(|c| c.shape == analytics_core::concepts::ConceptShape::Trendline)
            .count();
        let note = if preview.evidence.is_empty() && lines == 0 {
            if wanted_lines > 0 {
                "the trendline concept(s) found no confirmed swing pivots on the backfilled window, so no line was fitted -- either the window is too short for swings to confirm or the pivots are not there yet; a longer chart window will usually draw it.".to_string()
            } else {
                "no detection bands fired in the backfilled preview window -- on a real but tiny stored window that can mean missing data or a concept that did not match yet, not necessarily a wrong idea.".to_string()
            }
        } else {
            format!(
                "the detector layer found {zones} zone(s), {markers} marker(s) and {lines} fitted trendline(s) on the chart over the backfilled window; because this is a kind: indicator with no entry/risk blocks it fires 0 trading setups by design -- a kind: strategy is what produces setups.",
                zones = zones,
                markers = markers,
                lines = lines,
            )
        };
        format!(
            "Generated and validated revision {} (kind: indicator). This is a detector layer: it marks where the declared concepts match on the chart but has no entry/risk logic, so it produces 0 trading setups by design. {}",
            revision.revision_number,
            note,
        )
    } else if preview.evidence.is_empty() {
        format!("Generated and validated revision {} (kind: strategy). The replay over the past week fired no setups -- the conditions never coincided in that window. The source is shown below and the revision is attached; create a bot draft only after you have stored a historical backtest.", revision.revision_number)
    } else {
        format!("Generated and validated revision {} (kind: strategy). Attached to the chart with {} evidence node(s) from the replayed signals; source below. Create a bot draft only after you have stored a historical backtest.", revision.revision_number, preview.evidence.len())
    };
    let assistant_payload = serde_json::json!({
        "revision_id": revision.id,
        "strategy_id": strategy_id,
        "revision_number": revision.revision_number,
        "kind": document.kind.to_string(),
        "source": yaml,
        "evidence_nodes": preview.evidence.len(),
        "zones": preview.zones.len(),
        "markers": preview.markers.len(),
        // The auto-backtest card: the preview replay's own numbers, rendered
        // by the shell as a compact stats row under the message. Absent when
        // the replay could not run, so the card is honest about its absence.
        "preview_stats": preview_stats,
    });
    let assistant_message = db::create_indicator_workspace_message(
        database.pool(),
        user.user_id,
        id,
        "assistant",
        "revision",
        &assistant_text,
        &assistant_payload,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceTurnResponse {
            user_message: user_message.into(),
            assistant_message: assistant_message.into(),
            revision: revision.into(),
            strategy_id: strategy_id.to_string(),
        }),
    ))
}

/// `POST /indicator-workspaces/{id}/revisions/{revision_id}/restore`.
pub async fn restore_revision(
    State(state): State<AppState>,
    user: UserContext,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let restored =
        db::restore_indicator_revision(database(&state)?.pool(), user.user_id, id, revision_id)
            .await?;
    if !restored {
        return Err(ApiError::not_found(
            "validated indicator revision not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /indicator-workspaces/{id}/alerts`.
pub async fn set_alert(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<SetAlertBody>,
) -> Result<StatusCode, ApiError> {
    if body.event_name.trim().is_empty() || !body.channels.is_array() {
        return Err(ApiError::bad_request(
            "INDICATOR_ALERT_INVALID",
            "event_name is required and channels must be an array",
        ));
    }
    if db::get_indicator_revision(database(&state)?.pool(), user.user_id, id, body.revision_id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("indicator revision not found"));
    }
    let saved = db::set_indicator_alert_preference(
        database(&state)?.pool(),
        user.user_id,
        id,
        body.revision_id,
        body.event_name.trim(),
        body.enabled,
        &body.channels,
    )
    .await?;
    if !saved {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /indicator-workspaces/{id}`.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let row = db::get_indicator_workspace(database(&state)?.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    Ok(Json(row.into()))
}

/// `DELETE /indicator-workspaces/{id}`.
pub async fn delete(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted =
        db::delete_indicator_workspace(database(&state)?.pool(), user.user_id, id).await?;
    if !deleted {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /indicator-workspaces/{id}/revisions/{revision_id}`.
///
/// Dedicated single-revision source/preview read, rather than having the
/// client paginate the revisions list to find one.
pub async fn get_revision(
    State(state): State<AppState>,
    user: UserContext,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RevisionResponse>, ApiError> {
    let row = db::get_indicator_revision(database(&state)?.pool(), user.user_id, id, revision_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    Ok(Json(row.into()))
}

/// Body of the screenshot self-review endpoint.
#[derive(Debug, Deserialize)]
pub struct ReviewBody {
    /// Screenshots of the chart the revision is attached to. At least one:
    /// a review without a picture would be a re-read of the source, which
    /// the generator already did.
    pub images: Vec<MessageImage>,
}

/// Response of the screenshot self-review endpoint.
#[derive(Debug, Serialize)]
pub struct ReviewResponse {
    /// The reviewer's prose: what renders, what is wrong, what to change.
    pub review: String,
}

/// `POST /indicator-workspaces/{id}/revisions/{revision_id}/review`.
///
/// The model looks at a screenshot of the chart the revision is attached to
/// and reports bugs and missing pieces against the document it generated --
/// closing the loop the generation path leaves open.
pub async fn review_revision(
    State(state): State<AppState>,
    user: UserContext,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> Result<Json<ReviewResponse>, ApiError> {
    if body.images.is_empty() {
        return Err(ApiError::bad_request(
            "REVIEW_IMAGE_REQUIRED",
            "a review needs at least one screenshot of the chart",
        ));
    }
    if body.images.len() > MAX_MESSAGE_IMAGES {
        return Err(ApiError::bad_request(
            "TOO_MANY_IMAGES",
            &format!("a review may carry at most {MAX_MESSAGE_IMAGES} images"),
        ));
    }
    for image in &body.images {
        if !MESSAGE_IMAGE_TYPES.contains(&image.media_type.as_str()) {
            return Err(ApiError::bad_request(
                "UNSUPPORTED_IMAGE_TYPE",
                &format!(
                    "{} is not accepted; use one of {}",
                    image.media_type,
                    MESSAGE_IMAGE_TYPES.join(", ")
                ),
            ));
        }
    }
    let database = database(&state)?;
    let row = db::get_indicator_revision(database.pool(), user.user_id, id, revision_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    // The original ask lives in the workspace's memory -- the same compact
    // record `create_message` writes -- so the review can measure the render
    // against the request, not only against the source.
    let original_request = db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .and_then(|workspace| {
            workspace
                .memory
                .get("last_request")
                .and_then(|value| value.as_str().map(str::to_string))
        });
    let preview_note = row
        .validation
        .get("preview_note")
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let agent = state.agent.as_ref().ok_or_else(|| {
        ApiError::unavailable("the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID and AWS credentials")
    })?;
    let request = ai_agent::ReviewRequest {
        document_source: row.source,
        original_request,
        preview_note,
        images: body
            .images
            .into_iter()
            .map(|image| ai_agent::chart_context::ChartScreenshot {
                media_type: image.media_type,
                data: image.data,
                label: Some("chart screenshot with the revision attached".into()),
            })
            .collect(),
    };
    let review = agent.review_document(&request).await.map_err(ApiError::from)?;
    Ok(Json(ReviewResponse { review }))
}

/// `GET /indicator-workspaces/{id}/alerts`.
pub async fn list_alerts(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AlertPreferenceResponse>>, ApiError> {
    let database = database(&state)?;
    if db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    let rows =
        db::list_indicator_alert_preferences(database.pool(), user.user_id, id, DEFAULT_LIMIT)
            .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `GET /indicator-workspaces/{id}/bot-drafts`.
pub async fn list_bot_drafts(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<BotDraftResponse>>, ApiError> {
    let database = database(&state)?;
    if db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("indicator workspace not found"));
    }
    let rows =
        db::list_indicator_bot_drafts(database.pool(), user.user_id, id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /indicator-workspaces/{id}/bot-drafts`.
pub async fn create_bot_draft(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateBotDraftBody>,
) -> Result<(StatusCode, Json<BotDraftResponse>), ApiError> {
    if !matches!(body.mode.as_str(), "paper" | "live") {
        return Err(ApiError::bad_request(
            "INDICATOR_BOT_MODE_INVALID",
            "mode must be paper or live",
        ));
    }
    let draft = db::create_indicator_bot_draft(
        database(&state)?.pool(),
        user.user_id,
        id,
        body.revision_id,
        body.strategy_id,
        body.backtest_id,
        &body.mode,
        body.venue.as_deref(),
        &body.risk,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("workspace revision or strategy not found"))?;
    Ok((StatusCode::CREATED, Json(draft.into())))
}

/// Body of the promote endpoint.
#[derive(Debug, Deserialize)]
pub struct PromoteBody {
    /// The revision whose concepts become a tradeable strategy.
    pub revision_id: Uuid,
    /// What the entry should be, in the user's own words. The detector's
    /// concepts are the *measurements*; this says what acting on them means --
    /// "enter on a fresh gap and stop below the band". Optional: absent, the
    /// agent writes the conventional entry for the concepts it finds.
    #[serde(default)]
    pub entry_instruction: Option<String>,
}

/// `POST /indicator-workspaces/{id}/promote`.
///
/// Turns a generated **indicator** revision into a tradeable **strategy**:
/// the revision's concepts are carried over verbatim as the strategy's own
/// measurements, and the agent writes entry/risk/invalidation around them.
/// The result is validated, sandbox-checked, and stored as a strategy the
/// normal bot-draft path can pin -- the funnel from detection to paper trading
/// without re-describing the indicator.
///
/// # Errors
/// 404 when the workspace or revision is not the caller's; 503 without an
/// agent; 422 when the promoted document is refused by the sandbox.
pub async fn promote_revision(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PromoteBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let database = database(&state)?;
    let workspace = db::get_indicator_workspace(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator workspace not found"))?;
    let revision = db::get_indicator_revision(database.pool(), user.user_id, id, body.revision_id)
        .await?
        .ok_or_else(|| ApiError::not_found("indicator revision not found"))?;
    let agent = state
        .agent
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("the agent is not configured"))?;

    // The revision's concepts come from the stored preview, which is where
    // the generator put them; a revision whose preview predates concepts is
    // promoted as a strategy the agent writes from its summary alone.
    let concepts: Vec<serde_json::Value> = revision
        .preview
        .get("concepts")
        .and_then(|c| c.as_array().cloned())
        .unwrap_or_default();
    let concepts_yaml = concepts
        .iter()
        .map(|c| serde_json::to_string(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let instruction = body.entry_instruction.as_deref().unwrap_or(
        "Choose the conventional entry: act when a fresh band appears and confirm \
         with reclaim or rejection; stop on the far side of the band; take profit \
         at two times the risk.",
    );
    let description = format!(
        "Promote this generated indicator into a `kind: strategy` document. The \
         indicator's concepts are BELOW, verbatim -- carry every one into the \
         strategy's `concepts:` block unchanged, and write entry, risk and \
         invalidation conditions that reference them (concepts.<name>.fresh, \
         .mitigated, .top, .bottom). Entry instruction: {instruction}. Workspace: \
         {}. Symbol: {}. timeframes.entry MUST be exactly `{}`. The concepts, in \
         their JSON form, one per line: {concepts_yaml}",
        workspace.name,
        workspace.symbol,
        workspace.timeframe
    );

    let mut request =
        ai_agent::StrategyRequest::new(description, &workspace.symbol, &workspace.timeframe);
    request.max_attempts = Some(5);
    let generated = agent.generate_strategy(&request).await.map_err(ApiError::from)?;
    let attempts = generated.attempts;
    let document = generated.document().clone();
    let yaml = generated.yaml.clone();
    let validated = generated.into_validated();
    // A promoted document WILL be traded, unlike a preview: the sandbox check
    // is mandatory here, not skipped for indicator kinds.
    Decisions::sandboxed(state.sandbox.as_ref(), &validated).map_err(|err| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INDICATOR_SANDBOX_REFUSED",
            err.to_string(),
        )
    })?;

    let strategy = serde_json::to_value(&document)
        .map_err(|err| ApiError::internal(format!("could not store promoted strategy: {err}")))?;
    let strategy_id = db::create_strategy(
        database.pool(),
        user.user_id,
        &document.name,
        &document.version,
        &strategy,
        "ai_agent",
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "strategy_id": strategy_id,
            "revision_id": body.revision_id,
            "kind": document.kind.to_string(),
            "source": yaml,
            "attempts": attempts,
        })),
    ))
}

/// Body of the draft approval endpoint.
#[derive(Debug, Deserialize)]
pub struct ApproveDraftBody {
    /// Optional backtest that validates the strategy's historical performance.
    pub backtest_id: Option<Uuid>,
}

/// `POST /indicator-workspaces/{id}/bot-drafts/{draft_id}/approve`.
///
/// Approves a draft, optionally linking a backtest, then creates a bot from
/// the pinned strategy. The draft transitions through `approved` → `promoted`.
/// The bot is started through the same risk-gated path as `POST /bots`.
pub async fn approve_bot_draft(
    State(state): State<AppState>,
    user: UserContext,
    Path((id, draft_id)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<ApproveDraftBody>,
) -> Result<(StatusCode, Json<BotDraftResponse>), ApiError> {
    let database = database(&state)?;
    let draft = db::approve_indicator_bot_draft(
        database.pool(),
        user.user_id,
        id,
        draft_id,
        body.backtest_id,
    )
    .await?
    .ok_or_else(|| {
        ApiError::bad_request(
            "INDICATOR_DRAFT_NOT_APPROVABLE",
            "draft not found, not owned, not in draft status, or backtest constraint failed",
        )
    })?;

    // Build the bot through the same risk-gated path as POST /bots.
    let strategy = db::strategies::get_strategy(database.pool(), user.user_id, draft.strategy_id)
        .await
        .map_err(|e| ApiError::internal(format!("could not read strategy: {e}")))?
        .ok_or_else(|| ApiError::internal("strategy referenced by draft does not exist"))?;

    let source = serde_json::to_string(&strategy.document)
        .map_err(|e| ApiError::internal(format!("the stored document is not readable: {e}")))?;
    let validated = strategy_dsl::parse_and_validate(&source).map_err(ApiError::from)?;
    let document = validated.document().clone();

    // Validate the strategy can run (direction, risk block, etc.)
    strategy_runtime::StrategyEngine::new(&validated, strategy_runtime::RuntimeConfig::default())
        .map_err(|e| {
        ApiError::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "STRATEGY_NOT_RUNNABLE",
            e.to_string(),
        )
    })?;

    let mode = draft.mode.as_str();
    let limits = trading_engine::RiskLimits {
        max_risk_pct: draft
            .risk
            .get("max_risk_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
        ..trading_engine::RiskLimits::default()
    };
    let rolling = strategy_runtime::RollingConfig::new(
        strategy_runtime::RuntimeConfig::default().max_history,
        500,
        Default::default(),
    );

    // Sandboxed: principle #6 applies to indicator-generated bots too.
    let strategy_exec = trading_engine::Decisions::sandboxed(state.sandbox.as_ref(), &validated)
        .map_err(|e| {
            ApiError::coded(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "STRATEGY_NOT_RUNNABLE",
                e.to_string(),
            )
        })?;

    let bot_config = trading_engine::PaperConfig {
        symbol: document.market.clone(),
        limits,
        fills: strategy_runtime::SimulatorConfig::default(),
        rolling,
    };
    let bot = trading_engine::PaperBot::with_strategy(strategy_exec, &validated, bot_config);
    if let Some(note) = bot.clamp_note() {
        tracing::warn!(%note, "the requested risk was clamped");
    }

    // Create the bot row first, then start the task.
    let (bot_id, _created) = db::bots::create_bot_with_key(
        database.pool(),
        user.user_id,
        draft.strategy_id,
        mode,
        draft.venue.as_deref(),
        None,
    )
    .await?;

    // Link the bot back to the draft.
    db::set_indicator_bot_draft_bot_id(database.pool(), user.user_id, draft_id, bot_id).await?;

    state
        .bots
        .start(bot_id, user.user_id, (**database).clone(), bot);

    // Re-read the draft to return the updated row with bot_id.
    let updated_draft = db::get_indicator_bot_draft(database.pool(), user.user_id, id, draft_id)
        .await
        .map_err(|e| ApiError::internal(format!("could not read updated draft: {e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(
            updated_draft
                .map(BotDraftResponse::from)
                .unwrap_or(draft.into()),
        ),
    ))
}
