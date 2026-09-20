//! The `api-gateway` binary.
//!
//! Everything that decides anything is in the library next to this file; what
//! remains here is reading the environment, wiring the state, and serving. That
//! split exists so `tests/` can build the same router this binary serves --
//! see the note at the top of `lib.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};

use ai_agent::{Agent, AgentConfig, SkillLibrary};
use api_gateway::bots::{BotSupervisor, FeedMode};
use api_gateway::rate_limit::{RateLimit, RateLimiter};
use api_gateway::{AppState, build_auth, load_skills, router};
use db::Database;
use observability::QueueSink;
use observability::metrics::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present. Never required -- in deployment the values come
    // from the platform's environment.
    if dotenvy::dotenv().is_ok() {
        eprintln!("loaded .env");
    }

    // `LOG_FORMAT=json` for machine-readable logs in deployment; the default is
    // the human-readable line format so local runs stay readable.
    observability::init_logging("api-gateway");

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

    let feed = FeedMode::from_env();
    if feed == FeedMode::Off {
        info!("MARKET_FEED is not `binance`: bots started through the API will receive no candles");
    }
    let bots = Arc::new(BotSupervisor::new(feed));

    // Chart history older than the in-memory buffer is fetched from the venue
    // and dropped, not stored: `MARKET_REST_URL` exists so a test or a proxy can
    // point it somewhere else.
    let backfill = market_data::BackfillClient::new(
        std::env::var("MARKET_REST_URL").unwrap_or_else(|_| "https://api.binance.com".to_string()),
    );

    // One window service, over the same buffer the feeds fill, built from the
    // same venue client. Constructed from the supervisor's own registries
    // rather than fresh ones: a second `HistoryRegistry` would be a second,
    // permanently empty answer to the same question, which is the shape of the
    // defect this replaced.
    let windows = market_data::WindowService::new(bots.history(), bots.live(), backfill.clone());

    // The venue's own listing, for search and validation. Fetched lazily on
    // first use rather than here, so a venue that is slow or unreachable at boot
    // delays a symbol lookup instead of delaying the whole process -- and so a
    // cold start's first chart does not wait on a 2 MB payload it does not need.
    let symbols = market_data::SymbolIndex::new();

    let agent_limits = Arc::new(RateLimiter::new(RateLimit::from_env()));
    let limit = agent_limits.limit();
    info!(
        per_minute = limit.per_minute,
        burst = limit.burst,
        "/agent rate limit"
    );

    // Alerts are raised by a background rule evaluation, not by the code paths
    // that trip them -- a kill-switch must report itself even if the code that
    // tripped it is about to panic.
    let alert_queue = std::env::var("ALERT_WEBHOOK_URL").ok().map(|url| {
        info!(%url, "alerts will be forwarded to a webhook");
        Arc::new(QueueSink::new())
    });
    if alert_queue.is_none() {
        info!("ALERT_WEBHOOK_URL is not set; alerts go to the log and the audit trail only");
    }

    // The watchlist's feeds start with the process rather than on the first
    // chart that asks. A feed only begins when something requests its symbol, so
    // without this a freshly deployed gateway has an empty buffer and answers
    // its first chart entirely from the venue -- slower, and the buffer stays
    // empty for the one request that would have warmed it.
    for symbol in api_gateway::market_routes::watchlist() {
        bots.ensure_feed_for(symbol);
    }

    // And they are closed again when nobody wants them. A feed is a websocket
    // plus a builder per resolution plus RAM, and `ensure_feed_for` is reached
    // by routes -- so without a ceiling and a sweep, how many sockets this
    // process holds open is decided by how many symbol strings a client cares
    // to send. See `MAX_ACTIVE_FEEDS`.
    bots.spawn_reaper();

    let state = AppState {
        db,
        agent,
        skills: Arc::new(skills),
        auth,
        bots,
        backfill,
        windows,
        symbols,
        agent_limits,
        metrics: Registry::global_handle(),
        alert_queue: alert_queue.clone(),
        sandbox: Arc::new(sandbox::Sandbox::new().expect("the embedded guest module compiles")),
    };

    api_gateway::metrics::spawn_alert_task(state.clone());
    if let (Some(queue), Ok(url)) = (alert_queue, std::env::var("ALERT_WEBHOOK_URL")) {
        api_gateway::metrics::spawn_webhook_task(url, queue);
    }

    // Indicator workspace alert delivery: monitors bot decisions for events
    // that match enabled alert preferences and delivers via webhook.
    if let Some(db) = &state.db {
        let indicator_webhook = std::env::var("INDICATOR_ALERT_WEBHOOK_URL")
            .ok()
            .or_else(|| std::env::var("ALERT_WEBHOOK_URL").ok());
        let indicator_state = api_gateway::indicator_alerts::AlertDeliveryState {
            db: db.clone(),
            webhook_url: indicator_webhook,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        };
        let events_rx = state.bots.subscribe_events();
        api_gateway::indicator_alerts::spawn_indicator_alert_delivery(
            indicator_state, events_rx,
        );
    }

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
