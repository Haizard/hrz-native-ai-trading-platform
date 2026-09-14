//! The `api-gateway` binary.
//!
//! Everything that decides anything is in the library next to this file; what
//! remains here is reading the environment, wiring the state, and serving. That
//! split exists so `tests/` can build the same router this binary serves --
//! see the note at the top of `lib.rs`.

use std::net::SocketAddr;
use std::sync::Arc;

use tracing::{error, info, warn};

use ai_agent::{Agent, AgentConfig, SkillLibrary};
use api_gateway::{build_auth, load_skills, router, AppState};
use db::Database;

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
    let auth = build_auth();

    let state = AppState {
        db,
        agent,
        skills: Arc::new(skills),
        auth,
    };

    let app = router(state);

    let addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()?;

    info!(%addr, "api-gateway listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
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
