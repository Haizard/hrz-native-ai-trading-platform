//! # `api-gateway`
//!
//! The single REST + WebSocket surface the frontend talks to
//! (`docs/12-API-GATEWAY.md`). Full routing, auth, rate limiting and the
//! WebSocket fan-out land in Phase 7.
//!
//! In Phase 0 this binary exists to prove the stack is wired end to end:
//! it loads `DATABASE_URL`, opens the Postgres pool and exposes `/healthz`
//! and `/readyz`. If you can curl `/readyz` against your database, Phase 0's
//! infrastructure requirement is satisfied.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::{error, info};

use db::Database;

/// Shared application state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    /// Database handle. `None` when running without a database configured.
    pub db: Option<Arc<Database>>,
}

/// Health response body.
#[derive(Serialize)]
pub struct HealthResponse {
    /// Always "ok" when the process is alive.
    pub status: &'static str,
    /// Whether the database answered a live probe.
    pub database: &'static str,
}

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

    let state = AppState { db };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state);

    let addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()?;

    info!(%addr, "api-gateway listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
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
