//! # `api-gateway`
//!
//! The single REST + WebSocket surface the frontend talks to
//! (`docs/12-API-GATEWAY.md`).
//!
//! ## Why this is a library with a thin binary
//!
//! Everything that decides anything lives here, and `main.rs` only reads the
//! environment, wires the state and serves. The reason is testability: an
//! integration test in `tests/` cannot reach into a binary crate, so while this
//! was all `main.rs` the only way to check a route was to start a server and
//! curl it. Now `tests/` can build the same router the binary serves.
//!
//! ## Every capability is optional at startup
//!
//! A deployment with no Bedrock credentials still serves `/healthz` and
//! `/candles`; only `/agent/*` answers 503. The same is true of the database
//! and of `JWT_SECRET`. "Up but degraded" beats crash-looping because one
//! environment variable is missing -- and each degraded capability says which
//! variable is missing rather than failing anonymously.
//!
//! ## What is public
//!
//! `docs/12` says to decide explicitly rather than default to open. The
//! decision: **`/auth/*`, `/healthz`, `/readyz` and the market-data reads are
//! public; everything that touches someone's strategies, bots or skills
//! requires a bearer token.** Anonymous chart viewing is allowed because the
//! page is a chart and market data is not user data.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tracing::warn;

use ai_agent::{Agent, SkillLibrary};
use db::Database;

use crate::auth::AuthConfig;

pub mod agent_routes;
pub mod auth;
pub mod auth_routes;
pub mod bot_routes;
pub mod bots;
pub mod error;
pub mod extract;
pub mod footprint_routes;
pub mod market_data;
pub mod market_routes;
pub mod rate_limit;
pub mod skills_routes;
pub mod strategy_routes;
pub mod ws;

/// Shared application state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    /// Database handle. `None` when running without a database configured.
    pub db: Option<Arc<Database>>,
    /// The agent. `None` when Bedrock is not configured.
    pub agent: Option<Arc<Agent>>,
    /// Skill library, loaded from disk at startup.
    pub skills: Arc<SkillLibrary>,
    /// Session signing. `None` when `JWT_SECRET` is unset, in which case
    /// `/auth/*` answers 503 rather than pretending to authenticate.
    pub auth: Option<Arc<AuthConfig>>,
    /// Owns the running bots and the market feed they share.
    pub bots: Arc<bots::BotSupervisor>,
    /// Per-user limits on the endpoints that cost money (`docs/12`).
    pub agent_limits: Arc<rate_limit::RateLimiter>,
}

/// Health response body.
#[derive(Serialize)]
pub struct HealthResponse {
    /// Always "ok" when the process is alive.
    pub status: &'static str,
    /// Whether the database answered a live probe.
    pub database: &'static str,
}

/// Where skill documents are read from.
///
/// An environment override rather than a hardcoded `./skills`, so the deployed
/// container can point at a mounted volume.
pub const SKILLS_DIR_ENV: &str = "SKILLS_DIR";

/// Where the workstation is served from.
///
/// `docs/14`'s decision put the chart engine in `frontend/app` beside a
/// vanilla-JS shell, so that directory is the application.
pub const FRONTEND_DIR_ENV: &str = "FRONTEND_DIR";

/// Where the Phase 5 stopgap page lives.
///
/// Kept reachable at `/mvp` rather than deleted: it is a working chart, it is
/// referenced by `docs/02`, and having two ways to look at the same API is
/// useful while the workstation is new.
const MVP_DIR: &str = "frontend/mvp";

/// Build the router.
///
/// A function rather than an inline chain in `main` so tests can construct the
/// exact router that ships, instead of a copy of it that drifts.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/auth/register", post(auth_routes::register))
        .route("/auth/login", post(auth_routes::login))
        .route("/auth/me", get(auth_routes::me))
        .route("/candles", get(market_routes::candles))
        .route("/symbols", get(market_routes::symbols))
        .route("/footprint", get(footprint_routes::footprint))
        .route("/footprint/coverage", get(footprint_routes::coverage))
        .route("/orderbook", get(market_routes::orderbook))
        .route(
            "/strategies",
            get(strategy_routes::list).post(strategy_routes::create),
        )
        // Both before `/strategies/{id}`, or the literal paths would be read as
        // an id.
        .route("/strategies/validate", post(strategy_routes::validate))
        .route("/strategies/reference", get(strategy_routes::reference))
        .route("/strategies/examples", get(strategy_routes::examples))
        .route("/strategies/{id}", get(strategy_routes::get))
        .route(
            "/strategies/{id}/validate",
            post(strategy_routes::validate_stored),
        )
        .route("/strategies/{id}/backtest", post(strategy_routes::backtest))
        .route(
            "/strategies/{id}/backtests",
            get(strategy_routes::list_backtests),
        )
        .route("/backtests/{id}", get(strategy_routes::get_backtest))
        .route("/bots", get(bot_routes::list).post(bot_routes::create))
        .route(
            "/bots/{id}",
            get(bot_routes::get).delete(bot_routes::delete),
        )
        .route("/bots/{id}/pause", post(bot_routes::pause))
        .route("/bots/{id}/resume", post(bot_routes::resume))
        .route("/agent/ask", post(agent_routes::ask))
        .route(
            "/agent/generate-strategy",
            post(agent_routes::generate_strategy),
        )
        .route(
            "/skills",
            get(skills_routes::list).post(skills_routes::create),
        )
        .route(
            "/skills/{id}",
            get(skills_routes::get).put(skills_routes::create_version),
        )
        // WebSocket channels (`docs/12`). Registered alongside the REST
        // routes because they share the state and the auth story.
        .route("/ws/market/{symbol}/{timeframe}", get(ws::market))
        .route("/ws/orderbook/{symbol}", get(ws::orderbook))
        .route("/ws/agent/{session_id}", get(ws::agent))
        .route("/ws/bots/{bot_id}", get(ws::bot))
        // Served from the same origin as the API, so the page needs no CORS
        // policy and no second process.
        //
        // Three explicit routes rather than a static-file service: the shell is
        // exactly three files, and naming them means an unknown path is a JSON
        // 404 from the API rather than an HTML one from a directory listing.
        .route("/", get(app_index))
        .route("/app.js", get(app_js))
        .route("/chart_engine.wasm", get(chart_wasm))
        .route("/mvp", get(index))
        .with_state(state)
}

/// Where the workstation's files live.
fn frontend_dir() -> String {
    std::env::var(FRONTEND_DIR_ENV).unwrap_or_else(|_| "frontend/app".into())
}

/// Read one frontend file, or explain why it is missing.
///
/// A missing file is a 404 rather than a boot failure: the container may
/// legitimately run the API without the page. "Legitimately" is doing work in
/// that sentence, so the warning says which of the two it is -- a deployed
/// image that forgot to ship `frontend/` is not the same as an operator who set
/// [`FRONTEND_DIR_ENV`] somewhere else, and only one of them is a bug.
fn read_frontend(name: &str) -> Result<Vec<u8>, StatusCode> {
    let path = std::path::Path::new(&frontend_dir()).join(name);
    std::fs::read(&path).map_err(|e| {
        warn!(
            path = %path.display(),
            error = %e,
            env = FRONTEND_DIR_ENV,
            "frontend file not served (404). If this is a deployed image, it does not contain \
             frontend/ -- the Dockerfile has to copy it."
        );
        StatusCode::NOT_FOUND
    })
}

/// The workstation at `/`.
///
/// # Errors
/// 404 when the shell is not present beside the binary.
pub async fn app_index() -> Result<axum::response::Html<String>, StatusCode> {
    let bytes = read_frontend("index.html")?;
    String::from_utf8(bytes)
        .map(axum::response::Html)
        .map_err(|e| {
            warn!("frontend/app/index.html is not valid UTF-8: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// The shell's script at `/app.js`.
///
/// # Errors
/// 404 when the shell is not present beside the binary.
pub async fn app_js() -> Result<axum::response::Response, StatusCode> {
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        read_frontend("app.js")?,
    )
        .into_response())
}

/// The chart engine at `/chart_engine.wasm`.
///
/// The content type matters: `WebAssembly.instantiateStreaming` refuses a
/// response that is not `application/wasm`, and falling back to
/// `instantiate(arrayBuffer)` is a slower path nobody should need.
///
/// # Errors
/// 404 when the engine has not been built. `cargo run -p xtask --
/// build-frontend` puts it there.
pub async fn chart_wasm() -> Result<axum::response::Response, StatusCode> {
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/wasm")],
        read_frontend("chart_engine.wasm")?,
    )
        .into_response())
}

/// The Phase 5 chart, at `/mvp`.
///
/// Kept reachable rather than deleted: it is a working chart, `docs/02`
/// references it, and two ways to look at the same API is useful while the
/// workstation is new.
///
/// # Errors
/// 404 when the page is not present beside the binary.
pub async fn index() -> Result<axum::response::Html<String>, StatusCode> {
    let path = std::path::Path::new(MVP_DIR).join("index.html");
    match std::fs::read_to_string(&path) {
        Ok(html) => Ok(axum::response::Html(html)),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "the MVP page is not served (404)");
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// Load the skill library.
///
/// Three outcomes, and the log has to tell them apart, because only one of them
/// is benign:
///
/// * the directory is **missing** -- a packaging error, or a bad
///   [`SKILLS_DIR_ENV`]. The agent will answer confidently with no methodology
///   behind it, which is precisely the failure [`SkillLibrary::load_dir`] says
///   it refuses to have;
/// * the directory is **present but empty** -- also worth saying out loud;
/// * it **loaded** -- report the count.
///
/// A missing directory used to log `skills loaded count=0` at INFO, which is
/// how a deploy that shipped no skills at all looked like a healthy one. The
/// gateway still starts, because a running API that answers "no matching skill"
/// beats a boot loop -- but it does not get to be quiet about it.
pub fn load_skills() -> SkillLibrary {
    let dir = std::env::var(SKILLS_DIR_ENV).unwrap_or_else(|_| "skills".into());

    if !std::path::Path::new(&dir).exists() {
        warn!(
            dir = %dir,
            env = SKILLS_DIR_ENV,
            "skills directory does not exist: starting with an EMPTY library. The agent will \
             report \"no matching skill\" for every question. A deployed image must contain \
             skills/ -- the Dockerfile has to copy it."
        );
        return SkillLibrary::new();
    }

    match SkillLibrary::load_dir(&dir) {
        Ok(library) if library.is_empty() => {
            warn!(dir = %dir, "skills directory holds no skill documents");
            library
        }
        Ok(library) => {
            tracing::info!(count = library.len(), dir = %dir, "skills loaded");
            library
        }
        Err(e) => {
            warn!(dir = %dir, error = %e, "skills not loaded; starting with an empty library");
            SkillLibrary::new()
        }
    }
}

/// Build the session signer if a secret is configured, otherwise leave it
/// absent.
///
/// Non-fatal like the database: a deployment without `JWT_SECRET` still serves
/// health and market data, and `/auth/*` answers 503 saying exactly what is
/// missing. Generating a secret here would invalidate every token on restart,
/// and defaulting one would be a backdoor.
pub fn build_auth() -> Option<Arc<AuthConfig>> {
    match AuthConfig::from_env() {
        Ok(config) => {
            tracing::info!("JWT_SECRET set; /auth endpoints enabled");
            Some(Arc::new(config))
        }
        Err(e) => {
            warn!("{e}");
            None
        }
    }
}

/// Liveness: is the process up?
async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        database: "unknown",
    })
}

/// Readiness: can we serve traffic (i.e. is the database reachable)?
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let db_ok = match &state.db {
        Some(db) => db.health().await.is_ok(),
        None => false,
    };

    if db_ok {
        (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                database: "up",
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "not ready",
                database: "down",
            }),
        )
    }
}
