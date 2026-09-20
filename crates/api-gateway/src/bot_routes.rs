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

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sandbox::SandboxError;
use strategy_runtime::{RuntimeConfig, SimulatorConfig, StrategyEngine};
use trading_engine::{
    BinanceRest, Decisions, GateRequirements, GateVerdict, LiveBot, LiveConfig, LiveGate, PaperBot,
    PaperConfig, RiskLimits, TrackRecord,
};

use crate::AppState;
use crate::auth::UserContext;
use crate::bots::BotSupervisor;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::now_ns;

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
    /// A key that makes this request safe to retry (`docs/19` row 16).
    ///
    /// Send the same value again and the bot the first attempt made is
    /// returned, rather than a second one being started. Omit it to create a
    /// new bot every time, which is the behaviour for a deliberate second bot.
    pub idempotency_key: Option<String>,
}

/// The longest idempotency key accepted.
///
/// The key is stored on the row, so an unbounded one is an unbounded column.
/// 200 is well past any real client's key -- a UUID is 36 -- and short enough
/// that a pasted document is refused rather than stored.
const MAX_IDEMPOTENCY_KEY: usize = 200;

/// Check a client-supplied idempotency key, or `None` for no key at all.
///
/// An empty or whitespace-only key is **refused**, not treated as absent. Every
/// request carrying `""` would be the same request, so the first bot a user
/// created would come back for all their later creates and the symptom would
/// look like a bot ignoring its own settings. Saying so is better than silently
/// choosing one of the two meanings.
fn checked_idempotency_key(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IDEMPOTENCY_KEY_EMPTY",
            "`idempotency_key` was sent but is empty. Omit it to create a new bot every time, or \
             send a unique value to make a retry return the bot the first attempt made."
                .to_string(),
        ));
    }
    if trimmed.len() > MAX_IDEMPOTENCY_KEY {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IDEMPOTENCY_KEY_TOO_LONG",
            format!(
                "`idempotency_key` is {} characters; the limit is {MAX_IDEMPOTENCY_KEY}",
                trimmed.len()
            ),
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Turn a sandbox refusal into the 422 the gateway answers.
///
/// `Sandbox::start` can refuse for two reasons: the module speaks a different
/// ABI, or the document is rejected by the guest. Both mean "this document
/// cannot run", which is the same category as `STRATEGY_NOT_RUNNABLE`, and the
/// message names which.
fn sandbox_refusal(err: &SandboxError) -> ApiError {
    ApiError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "STRATEGY_NOT_RUNNABLE",
        err.to_string(),
    )
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

/// How many notifications one request returns.
///
/// Notifications are rare -- a risk breach, or a limit clamped at startup -- so
/// a bot that has produced more than this has produced a story worth paging
/// through, and the newest fifty carry it. There is no cursor: the alternative
/// is an endpoint nobody has needed yet.
const NOTIFICATION_LIMIT: i64 = 50;

/// One notification, as the panel shows it.
///
/// The four display fields are lifted out of the audit payload rather than
/// passed through as JSON. `trading_engine::store::notification_payload` writes
/// them for exactly this reason, and pinning the names here means a rename is a
/// failing test rather than a blank row in the panel.
#[derive(Debug, Serialize)]
pub struct NotificationResponse {
    /// `audit_log` row id, so a client can key a list on it.
    pub id: String,
    /// Short machine name: `killed` or `clamped`.
    pub kind: String,
    /// `critical` or `warning`.
    pub severity: String,
    /// One line, fit for a list.
    pub title: String,
    /// The detail behind the title.
    pub body: String,
    /// When it was raised, unix nanoseconds.
    pub at: i64,
}

/// `GET /bots/{id}/notifications`
///
/// ## Why this route exists
///
/// `docs/11` asks for the user to be notified on a breach. The writer for that
/// was built -- every alert the risk engine raises becomes a `bot.notification`
/// row -- and no reader was. The bots pane showed `activity.notifications`, a
/// *count*: it said something had happened and nothing about what.
///
/// A count with no list behind it reads as delivery. That is the same failure
/// as a metric with no writer, and it is why this route is not optional.
///
/// # Errors
/// 404 when the bot does not exist or belongs to someone else -- ownership
/// failures are 404s rather than 403s throughout `docs/12`, so a caller cannot
/// probe for ids that exist.
pub async fn notifications(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<Vec<NotificationResponse>>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;

    // Ownership first, and through the same lookup every other bot route uses.
    // Reading a bot's notifications is reading its trading decisions, so it
    // needs the same answer to "is this yours".
    db::bots::get_bot(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;

    let rows = db::paper::list_bot_notifications(database.pool(), id, NOTIFICATION_LIMIT).await?;

    Ok(Json(
        rows.into_iter()
            .map(|row| NotificationResponse {
                id: row.id.to_string(),
                kind: row.kind,
                severity: row.severity,
                title: row.title,
                body: row.body,
                at: row.ts,
            })
            .collect(),
    ))
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

    // Checked before anything is read, so a malformed key is refused without a
    // round trip and without a strategy being loaded.
    let idempotency_key = checked_idempotency_key(request.idempotency_key.as_deref())?;

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

    // Built to **refuse**, not to run.
    //
    // `StrategyEngine::new` is the only place a document that cannot trade is
    // rejected -- an `indicator`, one declaring no direction, one with no `risk`
    // block -- and its message names which. `strategy-dsl`'s validator does not
    // check this, so dropping the call would turn a 422 into a bot that is
    // created, runs, and never fires, which is exactly the shape of defect this
    // repository keeps finding. The bot's actual decisions come from the sandbox
    // (`PaperBot::with_strategy`), so this engine is dropped once the document has
    // passed.
    StrategyEngine::new(&validated, RuntimeConfig::default()).map_err(|e| {
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
            &validated,
            symbol,
            limits,
            rolling,
            idempotency_key.as_deref(),
        )
        .await;
    }

    start_live(
        &state,
        database,
        &user,
        &request,
        strategy.id,
        &validated,
        symbol,
        limits,
        rolling,
        idempotency_key.as_deref(),
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
    document: &strategy_dsl::ValidatedStrategy,
    symbol: String,
    limits: RiskLimits,
    rolling: strategy_runtime::RollingConfig,
    idempotency_key: Option<&str>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    // The row first, so a crash between this and the start below leaves a
    // `running` bot with no task -- visible and recoverable -- rather than a
    // task with nowhere to write.
    let (bot_id, created) = db::bots::create_bot_with_key(
        database.pool(),
        user.user_id,
        strategy_id,
        "paper",
        request.venue.as_deref(),
        idempotency_key,
    )
    .await?;

    if !created {
        return replay(state, database, user.user_id, bot_id).await;
    }

    // Built only once the row is ours. On a retry there is nothing to build --
    // starting a second task here is the exact thing the key exists to stop.
    //
    // Sandboxed, because this is where an agent-authored document becomes a
    // running bot: principle #6 says it must not execute unsandboxed, and this
    // call is what makes that true rather than aspirational.
    // MUTATION: make the route build a native engine instead of sandboxed,
    // so the guard `a_bot_created_by_the_route_is_sandboxed` fails.
    let strategy =
        Decisions::sandboxed(state.sandbox.as_ref(), document).map_err(|e| sandbox_refusal(&e))?;
    let bot = PaperBot::with_strategy(
        strategy,
        document,
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
    document: &strategy_dsl::ValidatedStrategy,
    symbol: String,
    limits: RiskLimits,
    rolling: strategy_runtime::RollingConfig,
    idempotency_key: Option<&str>,
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
    let (bot_id, created) = db::bots::create_bot_with_key(
        database.pool(),
        user.user_id,
        strategy_id,
        "live",
        Some(&venue),
        idempotency_key,
    )
    .await?;

    if !created {
        // A retry, and the one place this matters most: without this the second
        // request would place its own orders against the real account, and the
        // "spending real money" warning below would be logged a second time as
        // though a new bot were starting.
        return replay(state, database, user.user_id, bot_id).await;
    }

    let strategy =
        Decisions::sandboxed(state.sandbox.as_ref(), document).map_err(|e| sandbox_refusal(&e))?;
    let bot = LiveBot::with_strategy(
        strategy,
        document,
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

/// The answer for a request whose idempotency key had already been used.
///
/// Read back from the row rather than echoed from the request, because the
/// caller is asking "what did my earlier attempt make?" and the row is the only
/// thing that knows. Three deliberate choices:
///
/// * **200, not 201.** Nothing was created, and a client that branches on the
///   status should not be told otherwise.
/// * **No task is started.** This is the whole point of the key; the bot is
///   already running under whatever instance made it.
/// * **`supervised_here` is read, not assumed.** A retry that landed on a
///   different instance than the original request is honestly `false` -- the
///   same answer `GET /bots/{id}` would give, so the two cannot disagree.
async fn replay(
    state: &AppState,
    database: &db::Database,
    user_id: Uuid,
    bot_id: Uuid,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let row = db::bots::get_bot(database.pool(), user_id, bot_id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such bot"))?;
    let activity = db::paper::bot_summary(database.pool(), row.id).await?;
    Ok((StatusCode::OK, Json(respond(row, &state.bots, activity))))
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
        // The count and the list are two views of the same rows, so a panel
        // showing both must be able to tell they are named differently.
        assert_eq!(body["activity"]["notifications"], 1);
    }

    /// The notification list's keys, pinned for the same reason as the bot's.
    ///
    /// These four names are the ones `notification_payload` writes into the
    /// audit row and the ones the panel reads out of the response. Nothing
    /// joins the two ends: the writer is in `trading-engine`, the reader is
    /// here, and a rename on either side is a silently blank row rather than a
    /// compile error. This is the only place both ends are in scope at once.
    #[test]
    fn the_notification_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(vec![NotificationResponse {
            id: "9c1f".into(),
            kind: "killed".into(),
            severity: "critical".into(),
            title: "Paper bot stopped by the risk engine".into(),
            body: "daily loss limit".into(),
            at: 1_700_000_000_000_000_000,
        }])
        .expect("serializes");

        let first = &body[0];
        assert_eq!(first["id"], "9c1f");
        assert_eq!(first["kind"], "killed");
        assert_eq!(first["severity"], "critical");
        assert_eq!(first["title"], "Paper bot stopped by the risk engine");
        assert_eq!(first["body"], "daily loss limit");
        assert_eq!(first["at"], 1_700_000_000_000_000_000i64);
    }

    /// The names this route reads must be the names the writer writes.
    ///
    /// A stronger guard than the one above, and the one that would have caught
    /// the original gap: the payload keys are produced by
    /// `trading_engine::store::notification_payload`, so the test builds a real
    /// one and asserts the SQL's `payload->>'…'` keys are all present in it. A
    /// typo in either the query or the payload fails here, rather than
    /// returning rows whose `title` is the empty string.
    #[test]
    fn the_payload_keys_the_reader_uses_are_the_keys_the_writer_writes() {
        use trading_engine::{BotAlert, notification_payload};

        let payload = notification_payload(
            &BotAlert::Killed {
                reason: "daily loss limit".into(),
                positions: "flat".into(),
            },
            Uuid::nil(),
        );

        for key in ["bot_id", "kind", "severity", "title", "body"] {
            assert!(
                payload.get(key).is_some(),
                "the reader selects `payload->>'{key}'` but the writer does not \
                 write it, so every row would come back with a default"
            );
        }
        assert_eq!(payload["kind"], "killed");
        assert_eq!(payload["severity"], "critical");
    }
}
