//! # `api-gateway`
//!
//! The single REST + WebSocket surface the frontend talks to
//! (`docs/12-API-GATEWAY.md`). Authentication, rate limiting and the WebSocket
//! fan-out land in Phase 7.
//!
//! In Phase 5 the routes that matter are the agent ones: `/agent/ask` returns
//! an explainable thesis, `/candles` feeds the chart it sits next to, and
//! `/skills` exposes the methodology library. `/` serves the MVP chart from
//! the same origin, so there is no CORS and no second process to run.
//!
//! Every capability is optional at startup. A deployment with no Bedrock
//! credentials still serves `/healthz` and `/candles`; only `/agent/*` answers
//! 503. "Up but degraded" beats crash-looping because one environment variable
//! is missing.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tracing::{error, info, warn};

use ai_agent::{Agent, AgentConfig, SkillLibrary};
use db::Database;

mod agent_routes;
mod error;
mod market_data;
mod market_routes;
mod skills_routes;

/// Shared application state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    /// Database handle. `None` when running without a database configured.
    pub db: Option<Arc<Database>>,
    /// The agent. `None` when Bedrock is not configured.
    pub agent: Option<Arc<Agent>>,
    /// Skill library, loaded from disk at startup.
    pub skills: Arc<SkillLibrary>,
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
const SKILLS_DIR_ENV: &str = "SKILLS_DIR";

/// Where the MVP page is served from.
const FRONTEND_DIR_ENV: &str = "FRONTEND_DIR";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present. Never required -- in deployment the values come
    // from the platform's environment.
    if dotenvy::dotenv().is_ok() {
        eprintln!("loaded .env");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let db = match Database::from_env().await {
        Ok(db) => {
            info!("database connection verified");
            Some(Arc::new(db))
        }
        Err(e) => {
            // Deliberately non-fatal: the gateway still serves /healthz so the
            // deployment can report "up but not ready" rather than crash-loop.
            error!("database unavailable: {e}");
            None
        }
    };

    let skills = load_skills();
    let agent = build_agent(&skills);

    let state = AppState {
        db,
        agent,
        skills: Arc::new(skills),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/candles", get(market_routes::candles))
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
        // Served from the same origin as the API, so the page needs no CORS
        // policy and no second process.
        .route("/", get(index))
        .with_state(state);

    let addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()?;

    info!(%addr, "api-gateway listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Serve the MVP chart at `/`.
///
/// The page is one self-contained file -- no bundled assets, no build step --
/// so a handler reading it is enough and avoids pulling in a static-file
/// service for a single response. A missing file is a 404, not a boot
/// failure: the container may legitimately run the API without the page.
///
/// "Legitimately" is doing work in that sentence, so the warning says which of
/// the two it is. A deployed image that forgot to ship `frontend/` is not the
/// same as an operator who set [`FRONTEND_DIR_ENV`] somewhere else, and only
/// one of them is a bug.
async fn index() -> Result<axum::response::Html<String>, StatusCode> {
    let dir = std::env::var(FRONTEND_DIR_ENV).unwrap_or_else(|_| "frontend/mvp".into());
    let path = std::path::Path::new(&dir).join("index.html");
    match std::fs::read_to_string(&path) {
        Ok(html) => Ok(axum::response::Html(html)),
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                env = FRONTEND_DIR_ENV,
                "MVP page not served (404). If this is a deployed image, it does not contain \
                 frontend/ -- the Dockerfile has to copy it."
            );
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
fn load_skills() -> SkillLibrary {
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
            info!(count = library.len(), dir = %dir, "skills loaded");
            library
        }
        Err(e) => {
            warn!(dir = %dir, error = %e, "skills not loaded; starting with an empty library");
            SkillLibrary::new()
        }
    }
}

/// Build the agent if Bedrock is configured, otherwise leave it absent.
fn build_agent(skills: &SkillLibrary) -> Option<Arc<Agent>> {
    let configured = ai_agent::BedrockConfig::from_env().and_then(|config| {
        let model = config.model_id.clone();
        ai_agent::BedrockClient::new(config).map(|client| (client, model))
    });

    match configured {
        Ok((client, model)) => {
            info!(model = %model, "bedrock configured; /agent endpoints enabled");
            Some(Arc::new(Agent::new(
                Arc::new(client),
                skills.clone(),
                AgentConfig::default(),
            )))
        }
        Err(e) => {
            warn!("bedrock not configured; /agent endpoints will return 503: {e}");
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
