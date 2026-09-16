//! `/bots` (`docs/12-API-GATEWAY.md`).
//!
//! ## Starting a bot is two things, and both have to happen
//!
//! A `bots` row is the durable record; a task is the thing that runs. The row
//! is written first and the task second, because a row with no task is
//! recoverable (an operator sees a `running` bot that is not consuming candles,
//! and `paper-cli status` already warns about exactly that) while a task with
//! no row has nowhere to write.
//!
//! ## `live` is gated, and the gate is checked here
//!
//! `docs/12` lists one `POST /bots` for "paper or live". `docs/11` and
//! `docs/15` gate live trading behind a paper track record, per-venue opt-in
//! and configured risk limits. All three are checked at request time, and the
//! refusal names **every** unmet condition rather than the first -- an operator
//! should not need one round trip per requirement.
//!
//! The check is here rather than in `trading_engine::LiveBot` because it is a
//! decision about a *strategy*, made once, by a request. A bot constructed in a
//! test has no opt-in row and must still be buildable.
//!
//! ## `mode` is validated, not defaulted
//!
//! An unrecognised mode is a 422 rather than a silent fallback to paper.
//! Silently running a paper bot for someone who asked for a live one is the
//! worst possible answer: they believe they have a position and they do not.
//!
//! [`BotSupervisor`]: crate::bots::BotSupervisor

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use strategy_runtime::{RuntimeConfig, SimulatorConfig, StrategyEngine};
use trading_engine::{
    BinanceRest, GateRequirements, GateVerdict, LiveBot, LiveConfig, LiveGate, PaperBot,
    PaperConfig, RiskLimits, TrackRecord,
};

use crate::auth::UserContext;
use crate::bots::BotSupervisor;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::now_ns;
use crate::AppState;

/// Default page size for the list endpoint.
const DEFAULT_LIMIT: i64 = 50;

/// Equity a live bot sizes against until an account balance is read.
///
/// Not a guess about the user's money: it is the *denominator* in
/// fixed-fractional sizing, so it decides position size. Reading the real
/// balance needs a signed account endpoint the adapter does not implement yet,
/// and a bot that silently sizes against an invented balance is worse than one
/// that says what it assumed. `docs/19` carries the follow-up.
const ASSUMED_EQUITY: f64 = 10_000.0;

/// `POST /bots`.
#[derive(Debug, Deserialize)]
pub struct CreateBotRequest {
    /// The stored strategy to run.
    pub strategy_id: String,
    /// `paper` (the default) or `live`, which must pass the gate.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Venue. Required for `live`, since the opt-in is per venue.
    pub venue: Option<String>,
    /// Per-trade risk cap, in percent of equity. Clamped to the platform
    /// ceiling by the risk engine.
    pub max_risk_pct: Option<f64>,
    /// Daily loss budget, in R.
    pub daily_loss_limit_r: Option<f64>,
    /// Weekly loss budget, in R.
    pub weekly_loss_limit_r: Option<f64>,
}

fn default_mode() -> String {
    "paper".to_string()
}

/// A bot as a client sees it.
#[derive(Debug, Serialize)]
pub struct BotResponse {
    /// The bot.
    pub id: String,
    /// The strategy it runs.
    pub strategy_id: String,
    /// `paper` or `live`.
    pub mode: String,
    /// `running`, `paused`, `stopped` or `killed`.
    pub status: String,
    /// Venue, when it trades a real one.
    pub venue: Option<String>,
    /// When it was created, unix nanos.
    pub created_at: i64,
    /// Whether a task for it is live in *this* process.
    ///
    /// Distinct from `status`, and the distinction is the point: a bot can be
    /// `running` in the database while nothing is feeding it, which is what a
    /// process restart looks like from the outside.
    pub supervised_here: bool,
    /// Trades and decisions recorded so far.
    pub activity: Option<ActivityResponse>,
}

/// What a bot has done.
#[derive(Debug, Serialize)]
pub struct ActivityResponse {
    /// Completed trades.
    pub trades: i64,
    /// Trades still open.
    pub open_trades: i64,
    /// Summed R across completed trades.
    pub cumulative_r: f64,
    /// `on_candle` decisions recorded.
    pub decisions: i64,
    /// Notifications raised, including any breach.
    pub notifications: i64,
}

/// `POST /bots`
///
/// # Errors
/// 404 when the strategy is not the caller's, 422 when it cannot be run or the
/// mode is unknown, 403 when the live gate refuses, 503 without a database or
/// without exchange credentials.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(request): ApiJson<CreateBotRequest>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let database = database(&state)?;

    let mode = request.mode.as_str();
    if mode != "paper" && mode != "live" {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "MODE_UNKNOWN",
            format!(
                "`{}` is not a bot mode; expected `paper` or `live`",
                request.mode
            ),
        ));
    }

    let strategy_id = Uuid::parse_str(&request.strategy_id).map_err(|_| {
        ApiError::bad_request("ID_INVALID", "`strategy_id` is not a valid strategy id")
    })?;

    let strategy = db::strategies::get_strategy(database.pool(), user.user_id, strategy_id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such strategy"))?;

    let source = serde_json::to_string(&strategy.document)
        .map_err(|e| ApiError::internal(format!("the stored document is not readable: {e}")))?;
    let validated = strategy_dsl::parse_and_validate(&source).map_err(ApiError::from)?;
    let document = validated.document().clone();

    let limits = RiskLimits {
        max_risk_pct: request.max_risk_pct.unwrap_or(1.0),
        daily_loss_limit_r: request.daily_loss_limit_r.unwrap_or(3.0),
        weekly_loss_limit_r: request.weekly_loss_limit_r.unwrap_or(15.0),
        ..RiskLimits::default()
    };

    let engine = StrategyEngine::new(&validated, RuntimeConfig::default()).map_err(|e| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "STRATEGY_NOT_RUNNABLE",
            e.to_string(),
        )
    })?;

    let symbol = document.market.clone();
    let rolling = strategy_runtime::RollingConfig::new(
        RuntimeConfig::default().max_history,
        500,
        Default::default(),
    );

    if mode == "paper" {
        return start_paper(
            &state,
            database,
            &user,
            &request,
            strategy.id,
            engine,
            symbol,
            limits,
            rolling,
        )
        .await;
    }

    start_live(
        &state,
        database,
        &user,
        &request,
        strategy.id,
        engine,
        symbol,
        limits,
        rolling,
    )
    .await
}

/// Build and start a paper bot.
#[allow(clippy::too_many_arguments)]
async fn start_paper(
    state: &AppState,
    database: &db::Database,
    user: &UserContext,
    request: &CreateBotRequest,
    strategy_id: Uuid,
    engine: StrategyEngine,
    symbol: String,
    limits: RiskLimits,
    rolling: strategy_runtime::RollingConfig,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let bot = PaperBot::new(
        engine,
        PaperConfig {
            symbol: symbol.clone(),
            limits,
            fills: SimulatorConfig::default(),
            rolling,
        },
    );
    if let Some(note) = bot.clamp_note() {
        tracing::warn!(%note, "the requested risk was clamped");
    }

    let bot_id = db::bots::create_bot(
        database.pool(),
        user.user_id,
        strategy_id,
        "paper",
        request.venue.as_deref(),
    )
    .await?;

    // The task owns the bot from here. The row exists first, so a crash between
    // these two lines leaves a `running` bot with no task -- visible and
    // recoverable -- rather than a task with nowhere to write.
    state
        .bots
        .start(bot_id, user.user_id, database.clone(), bot);

    Ok((
        StatusCode::CREATED,
        Json(BotResponse {
            id: bot_id.to_string(),
            strategy_id: strategy_id.to_string(),
            mode: "paper".into(),
            status: "running".into(),
            venue: request.venue.clone(),
            created_at: now_ns(),
            supervised_here: true,
            activity: None,
        }),
    ))
}

/// Build and start a live bot, or refuse with every reason at once.
#[allow(clippy::too_many_arguments)]
async fn start_live(
    state: &AppState,
    database: &db::Database,
    user: &UserContext,
    request: &CreateBotRequest,
    strategy_id: Uuid,
    engine: StrategyEngine,
    symbol: String,
    limits: RiskLimits,
    rolling: strategy_runtime::RollingConfig,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let Some(venue) = request
        .venue
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VENUE_REQUIRED",
            "a live bot must name a venue: the opt-in is per venue, so there is no default to \
             fall back to",
        ));
    };
    let venue = venue.to_ascii_lowercase();

    // The opt-in list is the durable half of the gate; the track record comes
    // from the paper bots this strategy has already run.
    let mut gate = LiveGate::new(GateRequirements::default());
    for opted_in in db::live::opted_in_venues(database.pool(), user.user_id).await? {
        gate.opt_in(&opted_in);
    }

    let record =
        db::paper::strategy_paper_record(database.pool(), user.user_id, strategy_id).await?;
    let verdict = gate.check(
        &venue,
        TrackRecord {
            closed_trades: record.closed_trades.max(0) as usize,
            hours: record.hours(now_ns()),
            cumulative_r: record.cumulative_r,
        },
        &limits,
    );

    if let GateVerdict::Refused { reasons } = verdict {
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            "LIVE_GATE_REFUSED",
            format!(
                "this strategy may not trade {venue} live yet. {}",
                reasons.join("; ")
            ),
        ));
    }

    // Credentials are read from the environment and never stored. A missing key
    // is a 503 rather than a 403: nothing about the *request* is wrong, the
    // deployment is not configured.
    let adapter = BinanceRest::from_env(&symbol).map_err(|e| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "EXCHANGE_CREDENTIALS_MISSING",
            format!(
                "no {venue} credentials are configured, so a live bot cannot be started: {e}. \
                 Set the key and secret in the environment of the API process; they are never \
                 stored in the database."
            ),
        )
    })?;

    // The row first, so the bot's client order ids can carry its own id -- two
    // bots on one account must not be able to generate the same order id.
    let bot_id = db::bots::create_bot(
        database.pool(),
        user.user_id,
        strategy_id,
        "live",
        Some(&venue),
    )
    .await?;

    let bot = LiveBot::new(
        engine,
        adapter,
        LiveConfig {
            symbol: symbol.clone(),
            bot_id: bot_id.to_string(),
            limits,
            rolling,
            equity: ASSUMED_EQUITY,
            min_quantity: 0.0001,
        },
    );

    tracing::warn!(
        %bot_id,
        %venue,
        %symbol,
        "starting a LIVE bot: orders placed by this bot spend real money"
    );

    state
        .bots
        .start_live(bot_id, user.user_id, database.clone(), bot);

    Ok((
        StatusCode::CREATED,
        Json(BotResponse {
            id: bot_id.to_string(),
            strategy_id: strategy_id.to_string(),
            mode: "live".into(),
            status: "running".into(),
            venue: Some(venue),
            created_at: now_ns(),
            supervised_here: true,
            activity: None,
        }),
    ))
}

/// `GET /bots`
///
/// # Errors
/// 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<BotResponse>>, ApiError> {
    let database = database(&state)?;
    let rows = db::bots::list_bots(database.pool(), user.user_id, DEFAULT_LIMIT).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let activity = db::paper::bot_summary(database.pool(), row.id).await?;
        out.push(respond(row, &state.bots, activity));
    }
    Ok(Json(out))
}

/// `GET /bots/{id}`
///
/// # Errors
/// 404 when the bot does not exist or belongs to someone else.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BotResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;
    let row = db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;
    let activity = db::paper::bot_summary(database.pool(), row.id).await?;
    Ok(Json(respond(row, &state.bots, activity)))
}

/// `POST /bots/{id}/pause`
///
/// # Errors
/// 404 when the bot is not the caller's; 409 when it is not running here, since
/// pausing a bot whose task lives in another process (or nowhere) would record
/// a state nothing is honouring.
pub async fn pause(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BotResponse>, ApiError> {
    transition(&state, &user, &id, "paused", |supervisor, bot_id| {
        supervisor.pause(bot_id)
    })
    .await
}

/// `POST /bots/{id}/resume`
///
/// # Errors
/// 404 when the bot is not the caller's; 409 when it is not running here.
pub async fn resume(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BotResponse>, ApiError> {
    transition(&state, &user, &id, "running", |supervisor, bot_id| {
        supervisor.resume(bot_id)
    })
    .await
}

/// `POST /bots/{id}/kill`
///
/// The manual kill-switch `docs/18` asks for. Deliberately not the same thing as
/// [`pause`]: pausing stops the bot asking, killing stops it asking **and**
/// liquidates what it is holding, because a position left open with nothing
/// watching its stop is the loss the switch exists to prevent.
///
/// Returns 202 rather than 200: the liquidation happens in the bot's task, on
/// its next wakeup, because a route cannot block on a network round trip to a
/// venue. The response says what was asked for; `GET /bots/{id}` says what
/// happened.
///
/// # Errors
/// 404 when the bot is not the caller's; 409 when it is not running here, since
/// a switch thrown on a bot whose task lives elsewhere records an intention
/// nothing will act on.
pub async fn kill(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BotResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;

    if db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("no such bot"));
    }

    if !state.bots.kill(id) {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "BOT_NOT_SUPERVISED_HERE",
            "this bot has no running task in this process, so the switch cannot be thrown from \
             here. If it holds a position, close it at the venue by hand and reconcile.",
        ));
    }

    // Recorded before the liquidation finishes, so the trail says the switch
    // was thrown even if the close fails. `killed` is the status the risk
    // engine's own halt uses, and `docs/12` lists it as terminal.
    db::bots::set_status(database.pool(), user.user_id, id, "killed").await?;
    db::paper::insert_audit_events(
        database.pool(),
        &[db::paper::AuditEvent {
            user_id: Some(user.user_id),
            event_type: "bot.kill_switch".into(),
            payload: serde_json::json!({
                "bot_id": id,
                "by": "operator",
                "note": "the liquidation happens in the bot's task, not in this request",
            }),
            ts: now_ns(),
        }],
    )
    .await?;

    let updated = db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;
    let activity = db::paper::bot_summary(database.pool(), updated.id).await?;
    Ok(Json(respond(updated, &state.bots, activity)))
}

/// `DELETE /bots/{id}`
///
/// Stops the task, then deletes the bot and everything it wrote. `docs/12`
/// lists the route, and leaving a deleted bot's trades behind would attach them
/// to nothing.
///
/// # Errors
/// 404 when the bot is not the caller's.
pub async fn delete(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;

    let row = db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;

    // A live bot may hold a position, and a stop that skipped the liquidation
    // would leave it on the exchange with nothing watching its stop. The switch
    // is thrown first; `stop` then waits for the task, and the task runs the
    // liquidation before it checks the stop flag.
    if row.mode == "live" && state.bots.kill(id) {
        tracing::warn!(%id, "a live bot was deleted; liquidating its position first");
    }

    // Stop before deleting: a running task would otherwise flush rows for a bot
    // that no longer exists, and the foreign key would reject them.
    state.bots.stop(id).await;

    if !db::bots::delete_bot(database.pool(), user.user_id, id).await? {
        return Err(ApiError::not_found("no such bot"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Shared body of pause and resume.
async fn transition(
    state: &AppState,
    user: &UserContext,
    raw_id: &str,
    status: &str,
    apply: impl FnOnce(&BotSupervisor, Uuid) -> bool,
) -> Result<Json<BotResponse>, ApiError> {
    let database = database(state)?;
    let id = parse_id(raw_id)?;

    // Ownership first, so a bot belonging to someone else is a 404 rather than
    // a 409 about supervision.
    if db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("no such bot"));
    }

    if !apply(&state.bots, id) {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "BOT_NOT_SUPERVISED_HERE",
            "this bot has no running task in this process, so its state cannot be changed. \
             It may have been stopped, or the process may have restarted since it was created.",
        ));
    }

    db::bots::set_status(database.pool(), user.user_id, id, status).await?;
    let updated = db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;
    let activity = db::paper::bot_summary(database.pool(), updated.id).await?;
    Ok(Json(respond(updated, &state.bots, activity)))
}

fn respond(
    row: db::bots::BotRow,
    supervisor: &BotSupervisor,
    activity: Option<db::paper::BotSummary>,
) -> BotResponse {
    BotResponse {
        supervised_here: supervisor.is_running(row.id),
        id: row.id.to_string(),
        strategy_id: row.strategy_id.to_string(),
        mode: row.mode,
        status: row.status,
        venue: row.venue,
        created_at: row.created_at,
        activity: activity.map(|summary| ActivityResponse {
            trades: summary.trades,
            open_trades: summary.open_trades,
            cumulative_r: summary.cumulative_r,
            decisions: summary.decisions,
            notifications: summary.notifications,
        }),
    }
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; bots cannot be managed"))
}

fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::bad_request("ID_INVALID", format!("`{raw}` is not a valid bot id")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_bot_id_is_a_400_not_a_404() {
        let err = parse_id("not-a-uuid").expect_err("must refuse");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "ID_INVALID");
    }

    #[test]
    fn paper_is_the_default_mode() {
        assert_eq!(default_mode(), "paper");
    }

    /// The field names the bot panel reads.
    ///
    /// `docs/14`'s rule. The panel now branches on `status == "killed"` to
    /// disable the kill switch, and on `mode`/`venue` to say where a bot trades.
    /// None of those is a compile error if it is renamed -- the button would
    /// simply never disable, and an operator would press a switch that is
    /// already thrown and conclude it did nothing.
    #[test]
    fn the_bot_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(BotResponse {
            id: "8f3a".into(),
            strategy_id: "1b2c".into(),
            mode: "live".into(),
            status: "killed".into(),
            venue: Some("binance".into()),
            created_at: 0,
            supervised_here: true,
            activity: Some(ActivityResponse {
                trades: 3,
                open_trades: 1,
                cumulative_r: 2.5,
                decisions: 40,
                notifications: 1,
            }),
        })
        .expect("serializes");

        assert_eq!(body["id"], "8f3a");
        assert_eq!(body["mode"], "live");
        assert_eq!(body["status"], "killed");
        assert_eq!(body["venue"], "binance");
        assert_eq!(body["supervised_here"], true);
        assert_eq!(body["activity"]["trades"], 3);
        assert_eq!(body["activity"]["cumulative_r"], 2.5);
    }
}
