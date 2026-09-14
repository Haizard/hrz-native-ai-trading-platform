//! `/bots` (`docs/12-API-GATEWAY.md`).
//!
//! ## Starting a bot is two things, and both have to happen
//!
//! A `bots` row is the durable record; a [`BotSupervisor`] task is the thing
//! that runs. The row is written first and the task second, because a row with
//! no task is recoverable (an operator sees a `running` bot that is not
//! consuming candles, and `paper-cli status` already warns about exactly that)
//! while a task with no row has nowhere to write.
//!
//! ## `live` is refused, not ignored
//!
//! `docs/12` lists one `POST /bots` for "paper or live". `docs/11` gates live
//! trading behind a paper track record, per-venue opt-in and an `ExchangeAdapter`
//! that does not exist until Phase 8. Accepting `mode: "live"` and quietly
//! running a paper bot would be the worst of both -- so it is a 501 naming the
//! phase.
//!
//! [`BotSupervisor`]: crate::bots::BotSupervisor

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use strategy_runtime::{RuntimeConfig, SimulatorConfig, StrategyEngine};
use trading_engine::{PaperBot, PaperConfig, RiskLimits};

use crate::auth::UserContext;
use crate::bots::BotSupervisor;
use crate::error::ApiError;
use crate::AppState;

/// Default page size for the list endpoint.
const DEFAULT_LIMIT: i64 = 50;

/// `POST /bots`.
#[derive(Debug, Deserialize)]
pub struct CreateBotRequest {
    /// The stored strategy to run.
    pub strategy_id: String,
    /// `paper` (the default) or `live`, which is refused until Phase 8.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Venue, for a bot that trades a real one. Recorded but unused in paper.
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
/// 404 when the strategy is not the caller's, 422 when it cannot be run, 501
/// for `mode: "live"`, 503 without a database.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
    Json(request): Json<CreateBotRequest>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let database = database(&state)?;

    if request.mode != "paper" {
        return Err(ApiError::coded(
            StatusCode::NOT_IMPLEMENTED,
            "LIVE_TRADING_NOT_ENABLED",
            "only `paper` bots can be started. Live trading is Phase 8 and is gated behind a \
             paper track record, per-venue opt-in and an exchange adapter that does not exist yet",
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
    let bot = PaperBot::new(
        engine,
        PaperConfig {
            symbol: symbol.clone(),
            limits,
            fills: SimulatorConfig::default(),
            rolling: strategy_runtime::RollingConfig::new(
                RuntimeConfig::default().max_history,
                500,
                Default::default(),
            ),
        },
    );
    if let Some(note) = bot.clamp_note() {
        tracing::warn!(%note, "the requested risk was clamped");
    }

    let bot_id = db::bots::create_bot(
        database.pool(),
        user.user_id,
        strategy.id,
        &request.mode,
        request.venue.as_deref(),
    )
    .await?;

    // The task owns the bot from here. The row exists first, so a crash between
    // these two lines leaves a `running` bot with no task -- visible and
    // recoverable -- rather than a task with nowhere to write.
    state
        .bots
        .start(bot_id, user.user_id, database.as_ref().clone(), bot);

    Ok((
        StatusCode::CREATED,
        Json(BotResponse {
            id: bot_id.to_string(),
            strategy_id: strategy.id.to_string(),
            mode: request.mode,
            status: "running".into(),
            venue: request.venue,
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
    fn a_bad_bot_id_is_a_400_not_a_404() {
        let err = parse_id("not-a-uuid").expect_err("must refuse");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "ID_INVALID");
    }

    #[test]
    fn paper_is_the_default_mode() {
        assert_eq!(default_mode(), "paper");
    }
}
