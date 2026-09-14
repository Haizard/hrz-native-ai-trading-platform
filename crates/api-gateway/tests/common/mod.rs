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
use api_gateway::{auth::AuthConfig, router, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
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
}

impl Harness {
    /// The app under test, or `None` when there is no database to test against.
    pub async fn new() -> Option<Self> {
        let _ = dotenvy::dotenv();
        if std::env::var("DATABASE_URL").is_err() {
            eprintln!("DATABASE_URL is not set; skipping");
            return None;
        }
        let database = db::Database::from_env().await.ok()?;
        database.migrate().await.ok()?;

        let supervisor = Arc::new(BotSupervisor::with_flush_interval(
            FeedMode::Off,
            std::time::Duration::from_millis(100),
        ));
        let limits = Arc::new(RateLimiter::new(RateLimit::default()));
        let state = AppState {
            db: Some(Arc::new(database.clone())),
            agent: None,
            skills: Arc::new(ai_agent::SkillLibrary::new()),
            auth: Some(Arc::new(AuthConfig::new(SECRET))),
            bots: Arc::clone(&supervisor),
            agent_limits: Arc::clone(&limits),
        };
        Some(Self {
            app: router(state),
            database: Arc::new(database),
            supervisor,
            limits,
        })
    }

    /// The same app, but with no signing secret configured.
    pub async fn without_auth() -> Option<Self> {
        let _ = dotenvy::dotenv();
        if std::env::var("DATABASE_URL").is_err() {
            return None;
        }
        let database = db::Database::from_env().await.ok()?;
        let supervisor = Arc::new(BotSupervisor::with_flush_interval(
            FeedMode::Off,
            std::time::Duration::from_millis(100),
        ));
        let limits = Arc::new(RateLimiter::new(RateLimit::default()));
        let state = AppState {
            db: Some(Arc::new(database.clone())),
            agent: None,
            skills: Arc::new(ai_agent::SkillLibrary::new()),
            auth: None,
            bots: Arc::clone(&supervisor),
            agent_limits: Arc::clone(&limits),
        };
        Some(Self {
            app: router(state),
            database: Arc::new(database),
            supervisor,
            limits,
        })
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
