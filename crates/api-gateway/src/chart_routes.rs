//! `/chart-sessions`, `/chart-snapshots`, `/pattern-library`, `/scan/patterns`
//! (`docs/45`).
//!
//! ## Sessions are the workspace, kept
//!
//! The multi-panel grid lives in the tab without these routes: close the
//! browser and the arrangement is gone. A session is the layout, named, with
//! one row per panel; `is_template` marks a session as a starting point, and
//! `restore` copies a template into a new workspace rather than editing it --
//! a template that ordinary saves rewrite is not a template.
//!
//! ## Snapshots are the user's door into the same table the agent writes
//!
//! The agent's `take_snapshot` tool and this route write the same rows; the
//! `created_by` column says which door. The digest the route freezes is the
//! *tool's own* [`ai_agent::structure_digest`], because a compare that read
//! the two kinds of capture differently would be a diff of the formats, not
//! of the chart.
//!
//! ## The pattern library is explicit saves
//!
//! Detection is deterministic and cheap, so a match is not inherently worth
//! keeping -- see `db::pattern_library`'s header. This route validates the
//! shape (known kind, direction vocabulary, finite levels, confidence in
//! range) and stores what it was given.
//!
//! ## Scan is patterns over many symbols, on demand
//!
//! `POST /scan/patterns` is the scanner's shape with a pattern detector for
//! the metric: visit each symbol, load a window, detect, report. It writes
//! nothing -- matches worth keeping are saved explicitly, per the library's
//! rule. Public like `/scan`: patterns over candles are market data, not user
//! data.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ai_agent::MarketDataSource;
use analytics_core::types::Timeframe;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiQuery};
use crate::AppState;

// ---------------------------------------------------------------------------
// Chart sessions
// ---------------------------------------------------------------------------

/// `POST /chart-sessions` and `PUT /chart-sessions/{id}`.
#[derive(Debug, Deserialize)]
pub struct SessionBody {
    /// What to call it.
    pub name: String,
    /// Whether it is a starting point rather than a workspace.
    #[serde(default)]
    pub is_template: bool,
    /// The panels, in grid order.
    #[serde(default)]
    pub panels: Vec<PanelBody>,
}

/// One panel in a save.
#[derive(Debug, Deserialize)]
pub struct PanelBody {
    /// The instrument.
    pub symbol: String,
    /// The timeframe, in the engine's vocabulary.
    pub timeframe: String,
    /// The chart style, when set.
    #[serde(default)]
    pub chart_type: Option<String>,
    /// Configured indicators, opaque `[{name, params, ...}]`.
    #[serde(default = "empty_indicators")]
    pub indicators: serde_json::Value,
}

fn empty_indicators() -> serde_json::Value {
    serde_json::json!([])
}

/// `POST /chart-sessions/{id}/restore`.
#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    /// What to call the copy; defaults to the template's name.
    #[serde(default)]
    pub name: Option<String>,
}

/// A session as a client reads it, panels included on the single fetch.
#[derive(Debug, Serialize)]
pub struct SessionResponse {
    /// The row id.
    pub id: String,
    /// The name.
    pub name: String,
    /// Template or workspace.
    pub is_template: bool,
    /// The panels, in slot order.
    pub panels: Vec<PanelResponse>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds.
    pub updated_at: i64,
}

/// One panel as a client reads it.
#[derive(Debug, Serialize)]
pub struct PanelResponse {
    /// The row id.
    pub id: String,
    /// The grid slot.
    pub position: i32,
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// The chart style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart_type: Option<String>,
    /// The configured indicators.
    pub indicators: serde_json::Value,
}

/// `GET /chart-sessions`: one line per session, panels omitted.
#[derive(Debug, Serialize)]
pub struct SessionsResponse {
    /// The sessions, most recently saved first.
    pub sessions: Vec<SessionResponse>,
}

fn session_response(
    row: &db::ChartSessionRow,
    panels: Vec<db::ChartPanelRow>,
) -> SessionResponse {
    SessionResponse {
        id: row.id.to_string(),
        name: row.name.clone(),
        is_template: row.is_template,
        panels: panels
            .iter()
            .map(|p| PanelResponse {
                id: p.id.to_string(),
                position: p.position,
                symbol: p.symbol.clone(),
                timeframe: p.timeframe.clone(),
                chart_type: p.chart_type.clone(),
                indicators: p.indicators.clone(),
            })
            .collect(),
        created_at: row.created_at / 1_000_000,
        updated_at: row.updated_at / 1_000_000,
    }
}

/// Validate and convert a save body. Every panel's timeframe must be one the
/// engine knows: a session the engine cannot restore is a save that lied.
fn prepare_session(body: &SessionBody) -> Result<db::NewChartSession, ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_session",
            "a session needs a name",
        ));
    }
    if body.panels.len() > 16 {
        return Err(ApiError::bad_request(
            "invalid_session",
            format!(
                "{} panels is past the grid's 16; split the workspace",
                body.panels.len()
            ),
        ));
    }
    let mut panels = Vec::with_capacity(body.panels.len());
    for panel in &body.panels {
        if panel.symbol.trim().is_empty() {
            return Err(ApiError::bad_request(
                "invalid_session",
                "every panel needs a symbol",
            ));
        }
        panel.timeframe.parse::<Timeframe>().map_err(|_| {
            ApiError::bad_request(
                "invalid_session",
                format!(
                    "`{}` is not a timeframe this platform supports; the panel cannot be restored",
                    panel.timeframe
                ),
            )
        })?;
        if !panel.indicators.is_array() {
            return Err(ApiError::bad_request(
                "invalid_session",
                "a panel's `indicators` is a list, e.g. []",
            ));
        }
        panels.push(db::NewChartPanel {
            symbol: panel.symbol.trim().to_uppercase(),
            timeframe: panel.timeframe.clone(),
            chart_type: panel.chart_type.clone(),
            indicators: panel.indicators.clone(),
        });
    }
    Ok(db::NewChartSession {
        name: body.name.trim().to_string(),
        is_template: body.is_template,
        panels,
    })
}

/// `GET /chart-sessions`
pub async fn list_sessions(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<SessionsResponse>, ApiError> {
    let database = database(&state)?;
    let rows = db::list_chart_sessions(database.pool(), user.user_id).await?;
    Ok(Json(SessionsResponse {
        sessions: rows
            .iter()
            .map(|row| session_response(row, Vec::new()))
            .collect(),
    }))
}

/// `POST /chart-sessions`
pub async fn create_session(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<SessionBody>,
) -> Result<(StatusCode, Json<SessionResponse>), ApiError> {
    let database = database(&state)?;
    let new = prepare_session(&body)?;
    let row = db::create_chart_session(database.pool(), user.user_id, &new).await?;
    let panels = db::list_chart_panels(database.pool(), row.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(session_response(&row, panels)),
    ))
}

/// `GET /chart-sessions/{id}`
pub async fn get_session(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "SESSION")?;
    let (row, panels) = db::get_chart_session(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| unknown("SESSION"))?;
    Ok(Json(session_response(&row, panels)))
}

/// `PUT /chart-sessions/{id}` — save over the session, panels replaced.
pub async fn update_session(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<SessionBody>,
) -> Result<Json<SessionResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "SESSION")?;
    let new = prepare_session(&body)?;
    let row = db::update_chart_session(database.pool(), user.user_id, id, &new)
        .await?
        .ok_or_else(|| unknown("SESSION"))?;
    let panels = db::list_chart_panels(database.pool(), row.id).await?;
    Ok(Json(session_response(&row, panels)))
}

/// `DELETE /chart-sessions/{id}` — idempotent like the drawings route.
pub async fn delete_session(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<DeletedResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "SESSION")?;
    let deleted = db::delete_chart_session(database.pool(), user.user_id, id).await?;
    Ok(Json(DeletedResponse {
        id: id.to_string(),
        deleted,
    }))
}

/// `POST /chart-sessions/{id}/restore` — copy a template into a workspace.
///
/// Refuses a non-template: restoring a live workspace would fork a session
/// the user is still trading from, and a copy of it changing under them is
/// the confusion the template flag exists to prevent.
pub async fn restore_session(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<RestoreBody>,
) -> Result<(StatusCode, Json<SessionResponse>), ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "SESSION")?;
    let (row, panels) = db::get_chart_session(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| unknown("SESSION"))?;
    if !row.is_template {
        return Err(ApiError::bad_request(
            "not_a_template",
            "only a template can be restored; save the session as a template first",
        ));
    }
    let copy = db::NewChartSession {
        name: body
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| row.name.clone()),
        is_template: false,
        panels: panels
            .iter()
            .map(|p| db::NewChartPanel {
                symbol: p.symbol.clone(),
                timeframe: p.timeframe.clone(),
                chart_type: p.chart_type.clone(),
                indicators: p.indicators.clone(),
            })
            .collect(),
    };
    let created = db::create_chart_session(database.pool(), user.user_id, &copy).await?;
    let created_panels = db::list_chart_panels(database.pool(), created.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(session_response(&created, created_panels)),
    ))
}

// ---------------------------------------------------------------------------
// Chart snapshots
// ---------------------------------------------------------------------------

/// `GET /chart-snapshots?symbol=`.
#[derive(Debug, Deserialize)]
pub struct SnapshotsQuery {
    /// The instrument.
    pub symbol: String,
    /// How many, newest first (default 20, at most 50).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `POST /chart-snapshots`.
#[derive(Debug, Deserialize)]
pub struct CaptureBody {
    /// The instrument.
    pub symbol: String,
    /// The timeframe the chart is showing.
    pub timeframe: String,
    /// What this capture is for.
    #[serde(default)]
    pub note: Option<String>,
    /// Retrieval tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A snapshot as a client reads it.
#[derive(Debug, Serialize)]
pub struct SnapshotResponse {
    /// The row id.
    pub id: String,
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// Last close at capture.
    pub price: f64,
    /// The frozen drawings.
    pub drawings: serde_json::Value,
    /// The frozen structure digest.
    pub structure: serde_json::Value,
    /// The note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The tags.
    pub tags: Vec<String>,
    /// `user` or `ai`.
    pub created_by: String,
    /// Unix milliseconds.
    pub created_at: i64,
}

fn snapshot_response(row: &db::ChartSnapshotRow) -> SnapshotResponse {
    SnapshotResponse {
        id: row.id.to_string(),
        symbol: row.symbol.clone(),
        timeframe: row.timeframe.clone(),
        price: row.price,
        drawings: row.drawings.clone(),
        structure: row.structure.clone(),
        note: row.note.clone(),
        tags: row.tags.clone(),
        created_by: row.created_by.clone(),
        created_at: row.created_at / 1_000_000,
    }
}

/// `GET /chart-snapshots`
pub async fn list_snapshots(
    State(state): State<AppState>,
    user: UserContext,
    ApiQuery(query): ApiQuery<SnapshotsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let limit = i64::from(query.limit.unwrap_or(20).clamp(1, 50));
    let rows = db::list_chart_snapshots(
        database.pool(),
        user.user_id,
        &query.symbol.to_uppercase(),
        limit,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "symbol": query.symbol.to_uppercase(),
        "snapshots": rows.iter().map(snapshot_response).collect::<Vec<_>>(),
    })))
}

/// `GET /chart-snapshots/{id}`
pub async fn get_snapshot(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<SnapshotResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "SNAPSHOT")?;
    let row = db::get_chart_snapshot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| unknown("SNAPSHOT"))?;
    Ok(Json(snapshot_response(&row)))
}

/// `POST /chart-snapshots` — the user's own capture, stamped `user`.
///
/// The digest is the agent tool's own [`ai_agent::structure_digest`]: two
/// writers, one shape, or `compare_snapshots` would diff formats.
pub async fn capture_snapshot(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<CaptureBody>,
) -> Result<(StatusCode, Json<SnapshotResponse>), ApiError> {
    let database = database(&state)?;
    let symbol = body.symbol.to_uppercase();
    let timeframe = body.timeframe.parse::<Timeframe>().map_err(|_| {
        ApiError::bad_request(
            "unknown_timeframe",
            format!("`{}` is not a timeframe this platform supports", body.timeframe),
        )
    })?;

    // The same window the agent's capture reads: the last 300 bars ending at
    // the newest stored candle, through the window service, never the venue
    // direct. A symbol nobody watches gets a feed claim from the bots layer,
    // exactly as an ask does.
    state.bots.ensure_feed_for(&symbol);
    let data = crate::market_data::WindowMarketData::new(state.windows.clone());
    let latest = data
        .latest_candle_time(&symbol, timeframe)
        .await
        .map_err(|e| ApiError::unavailable(format!("candles for {symbol} are not available: {e}")))?
        .ok_or_else(|| {
            ApiError::unavailable(format!(
                "no candles for {symbol} {timeframe} yet; a snapshot of nothing is not a snapshot"
            ))
        })?;
    let bars: i64 = 300;
    let from = latest - (bars - 1) * timeframe.nanos();
    let candles = data
        .candles(&symbol, timeframe, from, latest + timeframe.nanos())
        .await
        .map_err(|e| ApiError::unavailable(format!("candles for {symbol} are not available: {e}")))?;
    let price = candles.last().map(|c| c.close).ok_or_else(|| {
        ApiError::unavailable(format!("no candles for {symbol} {timeframe} in the window"))
    })?;
    let structure = ai_agent::structure_digest(
        &candles,
        analytics_core::MarketStateConfig::default(),
    );

    // The user's drawings, frozen in the same shape the agent's captures
    // carry, ids included -- the compare diff is by identity.
    let drawing_rows = db::list_drawings(database.pool(), user.user_id, &symbol).await?;
    let drawings: Vec<serde_json::Value> = drawing_rows
        .iter()
        .map(|row| {
            serde_json::to_value(ai_agent::UserDrawing {
                id: Some(row.id.to_string()),
                kind: row.kind.clone(),
                label: row.label.clone(),
                time1_ms: row.a1_time_ms,
                price1: row.a1_price,
                time2_ms: row.a2_time_ms,
                price2: row.a2_price,
                time3_ms: row.a3_time_ms,
                price3: row.a3_price,
            })
            .unwrap_or_default()
        })
        .collect();

    let row = db::insert_chart_snapshot(
        database.pool(),
        user.user_id,
        &db::NewChartSnapshot {
            symbol,
            timeframe: timeframe.to_string(),
            price,
            drawings: serde_json::Value::Array(drawings),
            structure,
            note: body.note.filter(|n| !n.trim().is_empty()),
            tags: body
                .tags
                .iter()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            created_by: "user".into(),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(snapshot_response(&row))))
}

// ---------------------------------------------------------------------------
// Pattern library
// ---------------------------------------------------------------------------

/// `GET /pattern-library?symbol=&limit=`.
#[derive(Debug, Deserialize)]
pub struct PatternsQuery {
    /// Narrow to one instrument.
    #[serde(default)]
    pub symbol: Option<String>,
    /// How many, newest first (default 50, at most 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `POST /pattern-library`.
#[derive(Debug, Deserialize)]
pub struct SavePatternBody {
    /// The instrument.
    pub symbol: String,
    /// The timeframe it was detected on.
    pub timeframe: String,
    /// The detector's vocabulary.
    pub kind: String,
    /// bullish | bearish | neutral.
    pub direction: String,
    /// The detector's confidence, 0..1.
    pub confidence: f64,
    /// The swings that defined it.
    pub anchors: serde_json::Value,
    /// The neckline / breakout level.
    pub entry_level: f64,
    /// The measured-move objective.
    pub target: f64,
    /// The level that kills the read.
    pub invalidation: f64,
    /// One line, in the detector's words.
    pub summary: String,
    /// The saver's note.
    #[serde(default)]
    pub note: Option<String>,
}

/// A saved pattern as a client reads it.
#[derive(Debug, Serialize)]
pub struct PatternResponse {
    /// The row id.
    pub id: String,
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// The kind.
    pub kind: String,
    /// The direction.
    pub direction: String,
    /// The confidence at detection.
    pub confidence: f64,
    /// The frozen anchors.
    pub anchors: serde_json::Value,
    /// The entry level.
    pub entry_level: f64,
    /// The measured target.
    pub target: f64,
    /// The invalidation level.
    pub invalidation: f64,
    /// The summary line.
    pub summary: String,
    /// The saver's note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `user` or `ai`.
    pub created_by: String,
    /// Unix milliseconds.
    pub created_at: i64,
}

fn pattern_response(row: &db::PatternRow) -> PatternResponse {
    PatternResponse {
        id: row.id.to_string(),
        symbol: row.symbol.clone(),
        timeframe: row.timeframe.clone(),
        kind: row.kind.clone(),
        direction: row.direction.clone(),
        confidence: row.confidence,
        anchors: row.anchors.clone(),
        entry_level: row.entry_level,
        target: row.target,
        invalidation: row.invalidation,
        summary: row.summary.clone(),
        note: row.note.clone(),
        created_by: row.created_by.clone(),
        created_at: row.created_at / 1_000_000,
    }
}

/// `GET /pattern-library`
pub async fn list_library(
    State(state): State<AppState>,
    user: UserContext,
    ApiQuery(query): ApiQuery<PatternsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let database = database(&state)?;
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = db::list_patterns(
        database.pool(),
        user.user_id,
        query.symbol.as_deref(),
        limit,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "patterns": rows.iter().map(pattern_response).collect::<Vec<_>>(),
    })))
}

/// `POST /pattern-library` — an explicit save, validated at the door.
pub async fn save_pattern(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<SavePatternBody>,
) -> Result<(StatusCode, Json<PatternResponse>), ApiError> {
    let database = database(&state)?;
    let kind = analytics_core::PatternKind::from_name(&body.kind).ok_or_else(|| {
        ApiError::bad_request(
            "unknown_pattern",
            format!(
                "`{}` is not a pattern this platform detects; the kinds are: {}",
                body.kind,
                analytics_core::PatternKind::ALL
                    .iter()
                    .map(|k| k.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;
    if !matches!(body.direction.as_str(), "bullish" | "bearish" | "neutral") {
        return Err(ApiError::bad_request(
            "invalid_direction",
            format!("`{}` is not a direction; bullish, bearish or neutral", body.direction),
        ));
    }
    if !(0.0..=1.0).contains(&body.confidence) || !body.confidence.is_finite() {
        return Err(ApiError::bad_request(
            "invalid_confidence",
            "confidence is the detector's own number, 0..1",
        ));
    }
    for (name, value) in [
        ("entry_level", body.entry_level),
        ("target", body.target),
        ("invalidation", body.invalidation),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(ApiError::bad_request(
                "invalid_level",
                format!("`{name}` must be a positive price; a pattern without its levels is a label"),
            ));
        }
    }
    if !body.anchors.is_array() {
        return Err(ApiError::bad_request(
            "invalid_anchors",
            "`anchors` is the list of swings the detector named",
        ));
    }
    body.timeframe.parse::<Timeframe>().map_err(|_| {
        ApiError::bad_request(
            "unknown_timeframe",
            format!("`{}` is not a timeframe this platform supports", body.timeframe),
        )
    })?;

    let row = db::insert_pattern(
        database.pool(),
        user.user_id,
        &db::NewPattern {
            symbol: body.symbol.to_uppercase(),
            timeframe: body.timeframe.clone(),
            kind: kind.name().to_string(),
            direction: body.direction.clone(),
            confidence: body.confidence,
            anchors: body.anchors.clone(),
            entry_level: body.entry_level,
            target: body.target,
            invalidation: body.invalidation,
            summary: body.summary.clone(),
            note: body.note.filter(|n| !n.trim().is_empty()),
            created_by: "user".into(),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(pattern_response(&row))))
}

/// `DELETE /pattern-library/{id}` — idempotent like the drawings route.
pub async fn delete_saved_pattern(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<DeletedResponse>, ApiError> {
    let database = database(&state)?;
    let id = row_id(&id, "PATTERN")?;
    let deleted = db::delete_pattern(database.pool(), user.user_id, id).await?;
    Ok(Json(DeletedResponse {
        id: id.to_string(),
        deleted,
    }))
}

// ---------------------------------------------------------------------------
// Pattern scan
// ---------------------------------------------------------------------------

/// At most this many symbols per scan request.
///
/// A pattern scan loads a full window per symbol rather than reducing to one
/// number, so its ceiling is tighter than `/scan`'s: twenty windows is a page
/// of results a human reads, and past that the answer is a feed, not a scan.
const MAX_SCAN_SYMBOLS: usize = 20;

/// `POST /scan/patterns`.
#[derive(Debug, Deserialize)]
pub struct ScanPatternsBody {
    /// The instruments to scan.
    pub symbols: Vec<String>,
    /// The resolution to detect on.
    pub timeframe: String,
    /// Narrow to these kinds; absent detects all.
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    /// Swing-equality tolerance (default 0.004).
    #[serde(default)]
    pub tolerance_pct: Option<f64>,
    /// Drop matches below this confidence (default 0.5).
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Bars per symbol (default 300).
    #[serde(default)]
    pub lookback: Option<u32>,
}

/// One symbol's matches, or why it has none to report.
#[derive(Debug, Serialize)]
pub struct ScanPatternRow {
    /// The instrument.
    pub symbol: String,
    /// The matches, best-confidence first.
    pub patterns: Vec<serde_json::Value>,
    /// Why this symbol could not be scanned, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /scan/patterns` — detect across a named set, on demand.
///
/// Public like `/scan`: patterns over candles are market data. Writes nothing;
/// a match worth keeping goes to the library explicitly.
pub async fn scan_patterns(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<ScanPatternsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let timeframe = body.timeframe.parse::<Timeframe>().map_err(|_| {
        ApiError::bad_request(
            "unknown_timeframe",
            format!("`{}` is not a timeframe this platform supports", body.timeframe),
        )
    })?;
    let mut symbols: Vec<String> = Vec::new();
    for raw in &body.symbols {
        let symbol = raw.trim().to_uppercase();
        if !symbol.is_empty() && !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
    }
    if symbols.is_empty() {
        return Err(ApiError::bad_request(
            "no_symbols",
            "name at least one symbol to scan",
        ));
    }
    if symbols.len() > MAX_SCAN_SYMBOLS {
        return Err(ApiError::bad_request(
            "too_many_symbols",
            format!(
                "{} symbols were named and one pattern scan reads at most {MAX_SCAN_SYMBOLS}",
                symbols.len()
            ),
        ));
    }
    let kinds: Option<Vec<analytics_core::PatternKind>> = match &body.kinds {
        None => None,
        Some(raws) => {
            let mut parsed = Vec::with_capacity(raws.len());
            for raw in raws {
                parsed.push(analytics_core::PatternKind::from_name(raw).ok_or_else(|| {
                    ApiError::bad_request(
                        "unknown_pattern",
                        format!(
                            "`{raw}` is not a pattern this platform detects; the kinds are: {}",
                            analytics_core::PatternKind::ALL
                                .iter()
                                .map(|k| k.name())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?);
            }
            Some(parsed)
        }
    };
    let config = analytics_core::PatternConfig {
        tolerance_pct: body.tolerance_pct.unwrap_or(0.004).clamp(0.0005, 0.05),
        ..analytics_core::PatternConfig::default()
    };
    let min_confidence = body.min_confidence.unwrap_or(0.5).clamp(0.0, 1.0);
    let lookback = i64::from(body.lookback.unwrap_or(300).clamp(50, 1000));

    let data = crate::market_data::WindowMarketData::new(state.windows.clone());
    let mut rows = Vec::with_capacity(symbols.len());
    for symbol in &symbols {
        state.bots.ensure_feed_for(symbol);
        let row = scan_one(&data, symbol, timeframe, lookback, &kinds, &config, min_confidence).await;
        rows.push(row);
    }

    let found: usize = rows.iter().map(|r| r.patterns.len()).sum();
    Ok(Json(serde_json::json!({
        "timeframe": timeframe.to_string(),
        "scanned": rows.len(),
        "found": found,
        "rows": rows,
        "summary": format!("{} pattern(s) across {} symbol(s) on {}", found, rows.len(), timeframe),
    })))
}

/// One symbol of the scan: detect, or record why there is nothing to detect on.
async fn scan_one(
    data: &crate::market_data::WindowMarketData,
    symbol: &str,
    timeframe: Timeframe,
    lookback: i64,
    kinds: &Option<Vec<analytics_core::PatternKind>>,
    config: &analytics_core::PatternConfig,
    min_confidence: f64,
) -> ScanPatternRow {
    let result = async {
        let latest = data
            .latest_candle_time(symbol, timeframe)
            .await
            .map_err(|e| format!("candles for {symbol} are not available: {e}"))?
            .ok_or_else(|| format!("no candles for {symbol} {timeframe} yet"))?;
        let from = latest - (lookback - 1) * timeframe.nanos();
        let candles = data
            .candles(symbol, timeframe, from, latest + timeframe.nanos())
            .await
            .map_err(|e| format!("candles for {symbol} are not available: {e}"))?;
        if candles.len() < 30 {
            return Err(format!(
                "{} bars is too thin a window for a pattern read",
                candles.len()
            ));
        }
        let structure =
            analytics_core::detect_market_structure(&candles, Default::default());
        let mut matches = match kinds {
            // Every kind at once, then narrowed below -- one pass over the
            // swings, not one per kind.
            None => analytics_core::detect_patterns(&candles, &structure, None, config),
            Some(kinds) => {
                let mut all = Vec::new();
                for kind in kinds {
                    all.extend(analytics_core::detect_patterns(
                        &candles,
                        &structure,
                        Some(*kind),
                        config,
                    ));
                }
                all
            }
        };
        matches.retain(|m| m.confidence >= min_confidence);
        matches.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        Ok(matches)
    }
    .await;

    match result {
        Ok(matches) => ScanPatternRow {
            symbol: symbol.to_string(),
            patterns: matches
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "kind": m.kind.name(),
                        "direction": serde_json::to_value(m.direction).unwrap_or_default(),
                        "confidence": m.confidence,
                        "anchors": m.anchors.iter().map(|p| serde_json::json!({
                            "time_ms": p.timestamp / 1_000_000,
                            "price": p.price,
                        })).collect::<Vec<_>>(),
                        "entry_level": m.entry_level,
                        "target": m.target,
                        "invalidation": m.invalidation,
                        "summary": m.summary,
                    })
                })
                .collect(),
            error: None,
        },
        Err(reason) => ScanPatternRow {
            symbol: symbol.to_string(),
            patterns: Vec::new(),
            error: Some(reason),
        },
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// `DELETE` answers `deleted`, so the client can reconcile two tabs.
#[derive(Debug, Serialize)]
pub struct DeletedResponse {
    /// The id asked for.
    pub id: String,
    /// Whether a row was actually removed.
    pub deleted: bool,
}

/// Parse a path id; a malformed one is "no such row", like the drawings route.
fn row_id(raw: &str, resource: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| unknown(resource))
}

/// 404 for a row this user does not have.
fn unknown(resource: &str) -> ApiError {
    ApiError::coded(
        StatusCode::NOT_FOUND,
        format!("{resource}_UNKNOWN"),
        "no such row for this user",
    )
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; this resource cannot be stored"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panels(symbols: &[&str]) -> Vec<PanelBody> {
        symbols
            .iter()
            .map(|s| PanelBody {
                symbol: s.to_string(),
                timeframe: "1h".into(),
                chart_type: None,
                indicators: serde_json::json!([]),
            })
            .collect()
    }

    #[test]
    fn a_session_body_validates_its_panels_timeframes() {
        let mut body = SessionBody {
            name: "morning scan".into(),
            is_template: false,
            panels: panels(&["BTCUSDT"]),
        };
        let prepared = prepare_session(&body).expect("a good session");
        assert_eq!(prepared.name, "morning scan");
        assert_eq!(prepared.panels.len(), 1);
        assert_eq!(prepared.panels[0].symbol, "BTCUSDT");

        body.panels[0].timeframe = "3w".into();
        let err = prepare_session(&body).expect_err("an unrestorable panel is refused");
        assert!(err.message().contains("3w"), "{}", err.message());
    }

    #[test]
    fn a_nameless_session_is_refused() {
        let body = SessionBody {
            name: "   ".into(),
            is_template: false,
            panels: panels(&["BTCUSDT"]),
        };
        assert!(prepare_session(&body).is_err());
    }

    #[test]
    fn a_session_past_the_grid_limit_is_refused() {
        let body = SessionBody {
            name: "everything".into(),
            is_template: false,
            panels: panels(&[
                "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P",
                "Q",
            ]),
        };
        let err = prepare_session(&body).expect_err("17 panels is past the grid");
        assert!(err.message().contains("16"), "{}", err.message());
    }

    #[test]
    fn a_malformed_id_is_absent_not_an_error_about_formats() {
        let err = row_id("not-a-uuid", "SESSION").expect_err("must refuse");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.code(), "SESSION_UNKNOWN");
        assert!(row_id(&Uuid::nil().to_string(), "SESSION").is_ok());
    }

    #[test]
    fn the_session_wire_pins_the_keys_the_shell_reads() {
        let response = session_response(
            &db::ChartSessionRow {
                id: Uuid::nil(),
                name: "morning scan".into(),
                is_template: true,
                created_at: 1_700_000_000_000_000_000,
                updated_at: 1_700_000_000_000_000_000,
            },
            vec![db::ChartPanelRow {
                id: Uuid::nil(),
                position: 0,
                symbol: "BTCUSDT".into(),
                timeframe: "1h".into(),
                chart_type: Some("candlesticks".into()),
                indicators: serde_json::json!([{"name": "rsi", "period": 14}]),
            }],
        );
        let wire = serde_json::to_value(&response).expect("serializes");
        assert_eq!(wire["name"], "morning scan");
        assert_eq!(wire["is_template"], true);
        assert_eq!(wire["created_at"], 1_700_000_000_000_i64);
        let panel = &wire["panels"][0];
        for key in ["id", "position", "symbol", "timeframe", "chart_type", "indicators"] {
            assert!(panel.get(key).is_some(), "the panel wire lost `{key}`: {panel}");
        }
    }

    #[test]
    fn the_snapshot_wire_reports_milliseconds() {
        // The same rule the drawings wire follows: the browser's unit is
        // milliseconds, and a nanosecond leak renders as a date in 1970's
        // distant future rather than as an error.
        let response = snapshot_response(&db::ChartSnapshotRow {
            id: Uuid::nil(),
            symbol: "BTCUSDT".into(),
            timeframe: "1h".into(),
            price: 84_250.0,
            drawings: serde_json::json!([]),
            structure: serde_json::json!({"trend": "Up"}),
            note: None,
            tags: vec!["ny-open".into()],
            created_by: "user".into(),
            created_at: 1_700_000_000_000_000_000,
        });
        let wire = serde_json::to_value(&response).expect("serializes");
        assert_eq!(wire["created_at"], 1_700_000_000_000_i64);
        assert_eq!(wire["created_by"], "user");
    }
}
