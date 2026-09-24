//! Shared harness for the gateway integration tests.
//!
//! ## Why this is shared rather than copied
//!
//! Two test files driving the same router with two copies of "build the app and
//! send a request" is two things that drift: one gets a new header, the other
//! does not, and a test starts passing for a reason nobody wrote down. The
//! helpers live here so there is one answer to what a request looks like.
//!
//! ## It talks to a real database, and skips without one
//!
//! `docs/12`'s routes are thin wrappers over SQL, so a mocked database would
//! test the mock. Every test here registers a real account with a unique email,
//! does its work, and deletes what it made -- a shared database that
//! accumulates test rows makes the *next* run's counts wrong.

#![allow(dead_code)]

use std::sync::Arc;

use api_gateway::bots::{BotSupervisor, FeedMode};
use api_gateway::rate_limit::{RateLimit, RateLimiter};
use api_gateway::tickers;
use api_gateway::{auth::AuthConfig, router, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use observability::metrics::Registry;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The signing secret the tests use. Long enough for `AuthConfig::from_env`'s
/// rule, though these tests construct the config directly.
pub const SECRET: &str = "a-gateway-test-secret-that-is-long-enough";

/// Everything a test needs to talk to the app.
pub struct Harness {
    /// The router the binary serves.
    pub app: axum::Router,
    /// The database, so a test can clean up after itself.
    pub database: Arc<db::Database>,
    /// The bot supervisor, so a test can publish candles into the feed and
    /// check what a bot did with them.
    pub supervisor: Arc<api_gateway::bots::BotSupervisor>,
    /// The agent limiter, so a test can read the limit it is testing against
    /// rather than hard-coding a number that would drift.
    pub limits: Arc<RateLimiter>,
    /// The registry the app records into, so a test can read a metric the app
    /// wrote rather than trusting that a handler called the right function.
    ///
    /// The same `Arc` the `AppState` holds, so it sees what the router sees.
    pub metrics: Arc<Registry>,
    /// Held for the harness's whole life, so one test at a time owns the pool.
    ///
    /// Never read. It exists to be dropped, which is what releases the turn.
    _turn: tokio::sync::MutexGuard<'static, ()>,
}

/// The right to touch the database, one test at a time, per test process.
///
/// ## Why this exists
///
/// Every `Harness` opens a connection pool against the **same** managed
/// Postgres, and that database costs roughly a second per statement. A test
/// binary runs its tests on threads by default, so `cargo test --workspace`
/// ran six `load_flow` tests — and eleven `bot_flow` tests — concurrently
/// against ten connections between them.
///
/// The failure that produces is not "slow", it is "wrong": a `DELETE` waits out
/// the ten-second acquire timeout and returns 500, the bot row is never
/// removed, and the test then fails later on a foreign-key violation from
/// deleting the strategy — reporting the consequence and pointing at the wrong
/// line. That is exactly how `stopping_one_bot_leaves_the_feed_working_for_the_others`
/// failed, and it passed every time it was run on its own.
///
/// Serialising at the harness makes that impossible rather than unlikely. These
/// tests were never actually parallel — they were racing, and paying for the
/// contention in retries and confusion.
static DB_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Install a log subscriber, once per process.
///
/// The gateway warns about the things that are wrong but not fatal -- a bot task
/// that had to be aborted, a flush that failed -- and without this those warnings
/// go nowhere. A failing async test then reports a count that is one too low and
/// no reason, which is how a stop that timed out looked like a stop that was
/// never recorded.
///
/// `try_init` fails when a subscriber is already installed, which is the common
/// case with several tests in one process, so the result is ignored on purpose.
/// The default is `warn`: a passing run stays quiet.
fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

impl Harness {
    /// The app under test, or `None` when there is no database to test against.
    pub async fn new() -> Option<Self> {
        Self::build(None, Some(SECRET)).await
    }

    /// The same app, but with a scripted agent installed, so the routes that
    /// generate a document from a message can be driven end-to-end without a
    /// live model. `None` still means "no database".
    pub async fn with_agent(agent: Arc<ai_agent::Agent>) -> Option<Self> {
        Self::build(Some(agent), Some(SECRET)).await
    }

    /// Build the harness. One owner of the `AppState` construction, so a field
    /// added for `new` cannot be forgotten in a variant -- the failure that
    /// produces is a test passing against a differently-wired app.
    async fn build(agent: Option<Arc<ai_agent::Agent>>, secret: Option<&str>) -> Option<Self> {
        init_logging();
        let _ = dotenvy::dotenv();
        if std::env::var("DATABASE_URL").is_err() {
            eprintln!("DATABASE_URL is not set; skipping");
            return None;
        }
        // Taken before the first database call and held until the harness is
        // dropped. See `DB_TURN`.
        let turn = DB_TURN.lock().await;
        let database = db::Database::from_env().await.ok()?;
        // Migrations are idempotent and only meaningful where a schema is used;
        // the auth-less harness exists to test the 503 path and never touches it.
        if secret.is_some() {
            database.migrate().await.ok()?;
        }

        let supervisor = Arc::new(BotSupervisor::with_flush_interval(
            FeedMode::Off,
            std::time::Duration::from_millis(100),
        ));
        let limits = Arc::new(RateLimiter::new(RateLimit::default()));
        let metrics = Arc::new(Registry::new());
        let state = AppState {
            db: Some(Arc::new(database.clone())),
            agent,
            skills: Arc::new(ai_agent::SkillLibrary::new()),
            auth: secret.map(|secret| Arc::new(AuthConfig::new(secret))),
            bots: Arc::clone(&supervisor),
            // Nothing in a test may reach a venue. Port 1 refuses instantly, so
            // a route that tries to fetch history fails fast rather than
            // hanging -- and `GET /candles` degrades to what RAM can answer.
            backfill: market_data::BackfillClient::new("http://127.0.0.1:1"),
            // Built over the supervisor's own registries, exactly as `main.rs`
            // does: a window service with its own fresh buffer would satisfy
            // every assertion here and answer nothing in production.
            windows: market_data::WindowService::new(
                supervisor.history(),
                supervisor.live(),
                market_data::BackfillClient::new("http://127.0.0.1:1"),
            ),
            // Empty and never refreshed: no test may reach a venue. An empty
            // index is also the *interesting* state, because it is what makes a
            // lookup say "we do not know" rather than inventing an answer.
            symbols: market_data::SymbolIndex::new(),
            agent_limits: Arc::clone(&limits),
            metrics: Arc::clone(&metrics),
            // A fixed test key, not `build_vault()`. The value is irrelevant to
            // the properties under test -- that a credential round-trips, that
            // one user's ciphertext will not open for another -- and a fixed one
            // keeps `BROKER_KEK` out of the harness's requirements. A test that
            // needed the variable would skip on a machine without it, which is
            // how a security path ends up untested in CI.
            vault: Some(Arc::new(trading_engine::SecretVault::new([0x42; 32]))),
            // The same as `backfill` above: port 1 refuses instantly, so a route
            // that checks a user's exchange credentials fails fast and
            // deterministically rather than reaching the real venue. That is
            // also the *interesting* state for this path -- a credential check
            // that cannot reach the exchange must not read as a valid key.
            binance_base_url: "http://127.0.0.1:1".to_string(),
            // Fresh and empty: port 1 refuses instantly, so a tickers request
            // answers 503 fast instead of hanging on the venue. Tests that
            // need rows seed their own cache (see `TickerCache::seeded`),
            // which also keeps this path off the network entirely.
            tickers: Arc::new(tickers::TickerCache::new()),
            alert_queue: None,
            sandbox: Arc::new(sandbox::Sandbox::new().expect("the embedded guest module compiles")),
        };
        Some(Self {
            app: router(state),
            database: Arc::new(database),
            supervisor,
            limits,
            metrics,
            _turn: turn,
        })
    }

    /// The same app, but with no signing secret configured.
    pub async fn without_auth() -> Option<Self> {
        Self::build(None, None).await
    }

    /// Register an account and return it, so the test can delete it later.
    pub async fn register(&self) -> TestUser {
        let email = format!("gateway-test-{}@example.com", uuid::Uuid::new_v4());
        let (status, body) = self
            .post(
                "/auth/register",
                serde_json::json!({ "email": email, "password": "a-good-password" }),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "register failed: {body}");
        TestUser {
            id: body["user"]["id"].as_str().expect("an id").parse().unwrap(),
            email,
            token: body["token"].as_str().expect("a token").to_string(),
        }
    }

    /// POST a JSON body, optionally authenticated.
    pub async fn post(&self, path: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::from(body.to_string())).expect("request"))
            .await
    }

    /// POST a resource and return its `id`, asserting that it was created.
    ///
    /// ## Why this is a helper and not `body["id"].as_str().unwrap()`
    ///
    /// Most of these tests create something and then read `body["id"]`. When
    /// the status is discarded, *any* failed create -- including one with
    /// nothing to do with the thing under test -- arrives as
    /// `called \`Option::unwrap()\` on a \`None\` value` at the extraction,
    /// which names neither the route nor the reason. The reader then goes
    /// looking for a bug in the code that was never reached.
    ///
    /// That is not hypothetical. The managed Postgres occasionally exceeds its
    /// ten-second acquire timeout while a full `--workspace` run holds the
    /// pool; the create answers `500 DATABASE_ERROR`; and the test failed with
    /// an `unwrap` on `None` pointing at the wrong line. Asserting the status
    /// here is what makes a slow database say "slow database".
    ///
    /// Same rule `load_flow`'s cleanup deletes are held to: a test asserts its
    /// own setup.
    ///
    /// The status is asserted exactly rather than as "any 2xx": the create
    /// routes promise `201 Created`, and `is_success()` would let a route that
    /// started answering `200` pass every test in this suite.
    pub async fn created(&self, path: &str, body: Value, token: Option<&str>) -> String {
        let (status, body) = self.post(path, body, token).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "POST {path} should have created something, got {status}: {body}"
        );
        body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("POST {path} returned no id: {body}"))
            .to_string()
    }

    /// GET a path, asserting the request succeeded, and return the body.
    ///
    /// Same reasoning as [`Harness::created`]: a discarded status turns a
    /// failed read into a null body, and every assertion written against that
    /// null body then fails for a reason that is not the one being tested --
    /// `listed.as_array()` returning `None` reads as "the route returned the
    /// wrong shape", not "the database was unreachable".
    pub async fn ok(&self, path: &str, token: Option<&str>) -> Value {
        let (status, body) = self.get(path, token).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GET {path} should have succeeded, got {status}: {body}"
        );
        body
    }

    /// Serve this router on a real port and return its base URL.
    ///
    /// The WebSocket tests need an actual socket: `tower::oneshot` drives a
    /// request through the router but cannot upgrade a connection, so a
    /// handshake has to go over TCP.
    ///
    /// Binding port 0 lets the OS pick, so parallel test binaries cannot
    /// collide on a fixed port.
    pub async fn serve(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free port");
        let addr = listener.local_addr().expect("the bound address");
        let app = self.app.clone();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("127.0.0.1:{}", addr.port())
    }

    /// PUT a JSON body, optionally authenticated.
    pub async fn put(&self, path: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("PUT")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::from(body.to_string())).expect("request"))
            .await
    }

    /// DELETE a path, optionally authenticated.
    pub async fn delete(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method("DELETE").uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::empty()).expect("request"))
            .await
    }

    /// GET a path, optionally authenticated.
    pub async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::empty()).expect("request"))
            .await
    }

    /// GET a path and return the body as text.
    ///
    /// `/metrics` answers in Prometheus text exposition format, and `get`
    /// parses JSON -- so a scrape read through `get` arrives as `Value::Null`
    /// and every assertion on it passes vacuously. This is the accessor for the
    /// one route that is not JSON.
    pub async fn get_text(&self, path: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut builder = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("the router must answer");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body must be readable")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router must answer");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body must be readable")
            .to_bytes();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }
}

/// An account created by a test, which the test must delete.
pub struct TestUser {
    /// The account id.
    pub id: uuid::Uuid,
    /// The email, normalized.
    pub email: String,
    /// A token for it.
    pub token: String,
}

impl TestUser {
    /// Delete the account. Call this at the end of every test.
    pub async fn cleanup(self, database: &db::Database) {
        db::users::delete_user(database.pool(), self.id)
            .await
            .expect("the test must leave nothing behind");
    }
}

/// A small but complete strategy document, in the DSL's YAML.
///
/// Deliberately 5m-only: a document with a coarse context timeframe would make
/// every backtest depend on 4h candles being loaded, which is a fact about the
/// database rather than about the route under test.
pub const SIMPLE_STRATEGY: &str = r#"
name: "Route test strategy"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: close > vwap
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: "risk_multiple"
    value: 1.0
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
"#;

/// A document that parses but does not validate: `entry` is missing.
/// The same shape with a parameterised stop, a string condition and a function
/// call, so a test can check how the schema's two stop encodings come back.
pub const ATR_STOP_STRATEGY: &str = r#"
name: "ATR route test"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 4h
  entry: 5m
entry:
  direction: long
  any_of:
    - timeframe: entry
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close_below(stop_price)
risk:
  max_risk_pct: 0.5
  stop: {kind: atr, multiple: 1.5, period: 14}
  take_profit:
    type: "risk_multiple"
    value: 2.0
invalidation:
  - timeframe: entry
    condition: close < val
"#;

pub const INVALID_STRATEGY: &str = r#"
name: "Broken"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
"#;

/// A window the database has 5m candles for.
pub const WINDOW_FROM: &str = "2026-09-10";
pub const WINDOW_TO: &str = "2026-09-11";

/// A minimal skill document, as `POST /skills` takes it (JSON, not YAML --
/// the route deserializes into `Skill` directly).
///
/// The name is unique per test via [`unique_skill`] so two tests publishing
/// "Test skill 1.0" do not collide on `UNIQUE (user_id, name, version)`.
pub fn unique_skill(version: &str) -> Value {
    let name = format!("Route test skill {}", uuid::Uuid::new_v4());
    json!({
        "name": name,
        "version": version,
        "category": "liquidity",
        "knowledge": "Sweep a recent low, then buy the reclaim.",
        "rules": ["the sweep must have taken out a swing low"],
        "preferred_markets": ["BTCUSDT"],
    })
}
