//! `/strategies` and `/backtests` (`docs/12-API-GATEWAY.md`).
//!
//! ## Everything is validated before it is stored
//!
//! A strategy document is data that a bot will later execute. Storing one that
//! does not validate means storing something that will fail at the worst
//! possible moment -- when a bot tries to run it -- rather than now, when the
//! author is looking. So `POST /strategies` validates first and refuses with
//! the validator's own field-level issues.
//!
//! ## Additions to the documented route list
//!
//! `docs/12` says its list is "representative, extend as needed -- document
//! additions here as built". Two are added:
//!
//! * `POST /strategies/validate` -- validate DSL text *without* storing it.
//!   The strategy editor has to underline errors as the user types, before
//!   there is anything to attach an id to. `POST /strategies/{id}/validate` is
//!   also built, because `docs/12` lists it and re-validating a stored document
//!   is the cheap way to answer "is this still runnable after a schema change".
//! * `GET /strategies` and `GET /strategies/{id}/backtests` -- a dashboard
//!   needs to list, and an id alone is not something a user can browse.
//!
//! ## Ownership is a 404, never a 403
//!
//! A strategy belonging to somebody else reports as *absent*. A 403 would
//! confirm that the id exists, which is a fact a caller has no business
//! learning by guessing.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use analytics_core::types::Timeframe;
use backtester::replay::{run_backtest, ReplayConfig, ReplayInput};
use strategy_runtime::{RuntimeConfig, StrategyEngine};

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// Default page size for list endpoints.
const DEFAULT_LIMIT: i64 = 50;

/// Longest a strategy document may be, in bytes.
///
/// `strategy-dsl` enforces its own size limit; this one exists so a huge body
/// is refused before it is read into memory.
const MAX_SOURCE_BYTES: usize = 256 * 1024;

/// `POST /strategies`.
#[derive(Debug, Deserialize)]
pub struct CreateStrategyRequest {
    /// The DSL document as YAML or JSON text.
    ///
    /// Text rather than a JSON object because that is what all three editor
    /// modes produce: the raw editor has YAML, the visual builder serializes
    /// to JSON, and the agent returns YAML. One field, one path.
    pub source: String,
    /// Who produced it. Defaults to `developer_sdk`.
    pub created_by: Option<String>,
}

/// `POST /strategies/validate`.
#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    /// The DSL document as YAML or JSON text.
    pub source: String,
}

/// `POST /strategies/{id}/backtest`.
#[derive(Debug, Deserialize)]
pub struct BacktestRequest {
    /// Symbol to replay.
    pub symbol: String,
    /// Window start, `YYYY-MM-DD`, inclusive.
    pub from: String,
    /// Window end, `YYYY-MM-DD`, inclusive.
    pub to: String,
    /// Resolution to resample from when a declared timeframe has no candles.
    #[serde(default = "default_source_timeframe")]
    pub source_timeframe: String,
    /// Slippage applied to market orders, in basis points.
    #[serde(default = "default_slippage_bps")]
    pub slippage_bps: f64,
}

fn default_source_timeframe() -> String {
    "1m".to_string()
}

const fn default_slippage_bps() -> f64 {
    2.0
}

/// A strategy as a client sees it.
#[derive(Debug, Serialize)]
pub struct StrategyResponse {
    /// Row id.
    pub id: String,
    /// The document's own name.
    pub name: String,
    /// The document's own version.
    pub version: String,
    /// Who produced it.
    pub created_by: String,
    /// When it was stored, unix nanos.
    pub created_at: i64,
    /// The document itself.
    pub document: serde_json::Value,
}

impl From<db::strategies::StrategyRow> for StrategyResponse {
    fn from(row: db::strategies::StrategyRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name,
            version: row.version,
            created_by: row.created_by,
            created_at: row.created_at,
            document: row.document,
        }
    }
}

/// A backtest as a client sees it.
#[derive(Debug, Serialize)]
pub struct BacktestResponse {
    /// Row id.
    pub id: String,
    /// The strategy it ran.
    pub strategy_id: String,
    /// Symbol tested.
    pub symbol: String,
    /// Window start, unix nanos.
    pub from: i64,
    /// Window end, unix nanos.
    pub to: i64,
    /// When it ran, unix nanos.
    pub created_at: i64,
    /// The performance report.
    pub report: serde_json::Value,
}

impl From<db::strategies::BacktestRow> for BacktestResponse {
    fn from(row: db::strategies::BacktestRow) -> Self {
        Self {
            id: row.id.to_string(),
            strategy_id: row.strategy_id.to_string(),
            symbol: row.symbol,
            from: row.date_from,
            to: row.date_to,
            created_at: row.created_at,
            report: row.report,
        }
    }
}

/// What a validation attempt found.
#[derive(Debug, Serialize)]
pub struct ValidationResponse {
    /// Always true on a 200 -- a failure is a 4xx with the issues.
    pub valid: bool,
    /// The document's name, so a caller can confirm it parsed the right thing.
    pub name: String,
    /// The document's version.
    pub version: String,
    /// The declared timeframes.
    pub timeframes: std::collections::BTreeMap<String, String>,
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; strategies cannot be stored"))
}

/// Parse and validate DSL text.
///
/// The error carries the validator's own `{path, message}` issues, which is what
/// the editor underlines and what the agent's repair loop feeds back to the
/// model (`docs/12`).
fn validate_source(source: &str) -> Result<strategy_dsl::ValidatedStrategy, ApiError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ApiError::bad_request(
            "STRATEGY_TOO_LARGE",
            format!("a strategy document may be at most {MAX_SOURCE_BYTES} bytes"),
        ));
    }
    strategy_dsl::parse_and_validate(source).map_err(ApiError::from)
}

fn validation_summary(validated: &strategy_dsl::ValidatedStrategy) -> ValidationResponse {
    let document = validated.document();
    ValidationResponse {
        valid: true,
        name: document.name.clone(),
        version: document.version.clone(),
        timeframes: document
            .timeframes
            .iter()
            .map(|(name, tf)| (name.clone(), tf.to_string()))
            .collect(),
    }
}

/// `POST /strategies`
///
/// # Errors
/// 422 with field-level issues when the document does not validate, 400 when it
/// does not parse, 503 without a database.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(request): ApiJson<CreateStrategyRequest>,
) -> Result<(StatusCode, Json<StrategyResponse>), ApiError> {
    let database = database(&state)?;
    let validated = validate_source(&request.source)?;
    let document = validated.document();

    let created_by = request.created_by.unwrap_or_else(|| "developer_sdk".into());
    if !matches!(
        created_by.as_str(),
        "ai_agent" | "visual_builder" | "developer_sdk"
    ) {
        return Err(ApiError::bad_request(
            "CREATED_BY_INVALID",
            "`created_by` must be one of `ai_agent`, `visual_builder`, `developer_sdk`",
        ));
    }

    let stored = serde_json::to_value(document)
        .map_err(|e| ApiError::internal(format!("could not serialize the document: {e}")))?;

    let id = db::strategies::create_strategy(
        database.pool(),
        user.user_id,
        &document.name,
        &document.version,
        &stored,
        &created_by,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(StrategyResponse {
            id: id.to_string(),
            name: document.name.clone(),
            version: document.version.clone(),
            created_by,
            created_at: now_ns(),
            document: stored,
        }),
    ))
}

/// `POST /strategies/validate`
///
/// Validates text without storing it, so the editor can check as the user
/// types.
///
/// # Errors
/// 422 with the validator's issues, 400 for a parse failure.
pub async fn validate(
    ApiJson(request): ApiJson<ValidateRequest>,
) -> Result<Json<ValidationResponse>, ApiError> {
    let validated = validate_source(&request.source)?;
    Ok(Json(validation_summary(&validated)))
}

/// `GET /strategies`
///
/// # Errors
/// 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<StrategyResponse>>, ApiError> {
    let database = database(&state)?;
    let rows =
        db::strategies::list_strategies(database.pool(), user.user_id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `GET /strategies/{id}`
///
/// # Errors
/// 404 when the strategy does not exist or belongs to someone else.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<StrategyResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id, "strategy")?;
    let row = db::strategies::get_strategy(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such strategy"))?;
    Ok(Json(row.into()))
}

/// `POST /strategies/{id}/validate`
///
/// Re-validates a stored document. Cheap, and the honest answer to "is this
/// still runnable" after the DSL gains a rule.
///
/// # Errors
/// 404 when the strategy is not the caller's; 422 if the stored document no
/// longer validates.
pub async fn validate_stored(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<ValidationResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id, "strategy")?;
    let row = db::strategies::get_strategy(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such strategy"))?;

    let source = serde_json::to_string(&row.document)
        .map_err(|e| ApiError::internal(format!("the stored document is not readable: {e}")))?;
    let validated = validate_source(&source)?;
    Ok(Json(validation_summary(&validated)))
}

/// `POST /strategies/{id}/backtest`
///
/// Runs the replay and stores the report. The report is stored rather than
/// recomputed on read because the candles underneath it change: a backtest is
/// an observation made at a moment, and `docs/13` keeps it as one.
///
/// # Errors
/// 404 when the strategy is not the caller's; 422 when the window has no data
/// or the document no longer runs.
pub async fn backtest(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<BacktestRequest>,
) -> Result<(StatusCode, Json<BacktestResponse>), ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id, "strategy")?;
    let row = db::strategies::get_strategy(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such strategy"))?;

    let source = serde_json::to_string(&row.document)
        .map_err(|e| ApiError::internal(format!("the stored document is not readable: {e}")))?;
    let validated = validate_source(&source)?;

    let source_timeframe: Timeframe = request.source_timeframe.parse().map_err(|e| {
        ApiError::bad_request(
            "TIMEFRAME_INVALID",
            format!(
                "unknown source timeframe `{}`: {e}",
                request.source_timeframe
            ),
        )
    })?;
    let from_ns = parse_date_ns(&request.from)?;
    let to_ns = parse_date_ns(&request.to)? + 86_400 * 1_000_000_000;
    if to_ns <= from_ns {
        return Err(ApiError::bad_request(
            "WINDOW_INVALID",
            "`to` must not be before `from`",
        ));
    }

    let symbol = request.symbol.to_uppercase();
    let document = validated.document();

    let series = db::loading::load_timeframe_series(
        database.pool(),
        &symbol,
        &document.timeframes,
        from_ns,
        to_ns,
        source_timeframe,
    )
    .await?;
    db::loading::warn_about_short_series(&series, &document.timeframes, from_ns, to_ns);

    let input = ReplayInput::new(document, series)?;
    let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default()).map_err(|e| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "STRATEGY_NOT_RUNNABLE",
            e.to_string(),
        )
    })?;

    let report = run_backtest(
        &mut engine,
        &input,
        &ReplayConfig {
            symbol: symbol.clone(),
            from: from_ns,
            to: to_ns - 1,
            simulator: backtester::SimulatorConfig {
                slippage_bps: request.slippage_bps,
                ..backtester::SimulatorConfig::default()
            },
            ..ReplayConfig::default()
        },
    )?;

    let report_json = serde_json::to_value(&report)
        .map_err(|e| ApiError::internal(format!("could not serialize the report: {e}")))?;

    let backtest_id = db::strategies::create_backtest(
        database.pool(),
        row.id,
        &symbol,
        from_ns,
        to_ns - 1,
        &report_json,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(BacktestResponse {
            id: backtest_id.to_string(),
            strategy_id: row.id.to_string(),
            symbol,
            from: from_ns,
            to: to_ns - 1,
            created_at: now_ns(),
            report: report_json,
        }),
    ))
}

/// `GET /backtests/{id}`
///
/// # Errors
/// 404 when the backtest is not the caller's.
pub async fn get_backtest(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BacktestResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id, "backtest")?;
    let row = db::strategies::get_backtest(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such backtest"))?;
    Ok(Json(row.into()))
}

/// `GET /strategies/{id}/backtests`
///
/// # Errors
/// 404 when the strategy is not the caller's.
pub async fn list_backtests(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<Vec<BacktestResponse>>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id, "strategy")?;
    // Confirm the strategy exists and is the caller's before listing: an empty
    // list for someone else's strategy would look like "it has no backtests".
    if db::strategies::get_strategy(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("no such strategy"));
    }
    let rows =
        db::strategies::list_backtests(database.pool(), user.user_id, id, DEFAULT_LIMIT).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

fn parse_id(raw: &str, what: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| {
        ApiError::bad_request("ID_INVALID", format!("`{raw}` is not a valid {what} id"))
    })
}

/// Parse `YYYY-MM-DD` into unix nanoseconds at 00:00 UTC.
fn parse_date_ns(date: &str) -> Result<i64, ApiError> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| {
        ApiError::bad_request(
            "DATE_INVALID",
            format!("`{date}` is not a date; expected YYYY-MM-DD"),
        )
    })?;
    let midnight = parsed.and_hms_opt(0, 0, 0).ok_or_else(|| {
        ApiError::bad_request("DATE_INVALID", format!("`{date}` has no midnight"))
    })?;
    Ok(midnight.and_utc().timestamp() * 1_000_000_000)
}

fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_parses_to_midnight_utc() {
        assert_eq!(parse_date_ns("1970-01-01").unwrap(), 0);
        // 2026-03-13T00:00:00Z, the start of the reference backtest window.
        assert_eq!(
            parse_date_ns("2026-03-13").unwrap(),
            1_773_360_000_000_000_000
        );
    }

    #[test]
    fn a_bad_date_is_a_400_with_a_code() {
        for bad in ["", "13-03-2026", "2026/03/13", "yesterday", "2026-13-01"] {
            let err = parse_date_ns(bad).expect_err(&format!("accepted {bad:?}"));
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
            assert_eq!(err.code(), "DATE_INVALID");
        }
    }

    #[test]
    fn a_bad_id_is_a_400_not_a_404() {
        // A malformed id is a client bug, not a missing resource, and the two
        // deserve different codes.
        let err = parse_id("not-a-uuid", "strategy").expect_err("must refuse");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "ID_INVALID");
    }

    #[test]
    fn an_over_long_document_is_refused_before_it_is_parsed() {
        let huge = "x".repeat(MAX_SOURCE_BYTES + 1);
        let err = validate_source(&huge).expect_err("must refuse");
        assert_eq!(err.code(), "STRATEGY_TOO_LARGE");
    }

    #[test]
    fn a_document_that_parses_but_does_not_validate_is_422() {
        // Parses -- every field serde requires is present -- but has no `entry`
        // block, so it fails *validation* rather than parsing. The distinction
        // matters to a client: one means "fix this field", the other means
        // "this is not a strategy document".
        let err = validate_source(
            "name: x\nversion: '1'\nkind: strategy\nmarket: BTCUSDT\n\
             timeframes:\n  entry: 5m\nrisk:\n  max_risk_pct: 1.0\n  \
             stop: {kind: below_recent_low, bars: 20}\n",
        )
        .expect_err("must fail");
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code(), "STRATEGY_VALIDATION_FAILED");
    }

    #[test]
    fn a_document_missing_required_fields_is_a_parse_error() {
        // The other half of the distinction, pinned so the two cannot quietly
        // collapse into one code.
        let err =
            validate_source("name: x\nversion: '1'\nkind: strategy\n").expect_err("must fail");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "STRATEGY_PARSE_FAILED");
    }
}
