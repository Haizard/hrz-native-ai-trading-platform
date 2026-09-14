//! The auth flow, driven through the real router.
//!
//! ## What this checks that the unit tests cannot
//!
//! `auth.rs` proves the token logic in isolation: signatures, expiry, `alg:
//! none`. None of that says whether `/auth/register` actually creates an
//! account, whether the token it returns is accepted by `/auth/me`, or whether
//! a duplicate email produces a 409 rather than a 500. Those are properties of
//! the *route*, so they are checked by calling the route.
//!
//! The router is the one the binary serves -- `api_gateway::router` -- rather
//! than a copy assembled here, because a copy is a thing that drifts.
//!
//! ## It cleans up after itself
//!
//! Each test registers an account with a unique email and deletes it at the
//! end, so the database is left as it was found. Without `DATABASE_URL` the
//! tests print why and return, so the suite still runs on a machine with no
//! database.

use std::sync::Arc;

use api_gateway::{auth::AuthConfig, router, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &str = "a-gateway-test-secret-that-is-long-enough";

/// The app under test, or `None` when there is no database to test against.
async fn app() -> Option<(axum::Router, Arc<db::Database>)> {
    let _ = dotenvy::dotenv();
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL is not set; skipping the auth flow test");
        return None;
    }
    let database = db::Database::from_env().await.ok()?;
    database.migrate().await.ok()?;

    let state = AppState {
        db: Some(Arc::new(database.clone())),
        agent: None,
        skills: Arc::new(ai_agent::SkillLibrary::new()),
        auth: Some(Arc::new(AuthConfig::new(SECRET))),
    };
    Some((router(state), Arc::new(database)))
}

/// POST a JSON body and return the status and parsed body.
async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("the request must build");

    let response = app
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

/// GET a path with an optional bearer token.
async fn get(app: &axum::Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).expect("the request must build"))
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

fn unique_email() -> String {
    format!("gateway-test-{}@example.com", uuid::Uuid::new_v4())
}

#[tokio::test]
async fn register_then_me_round_trips_through_the_router() {
    let Some((app, database)) = app().await else {
        return;
    };
    let email = unique_email();

    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": email, "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().expect("a token").to_string();
    assert_eq!(body["user"]["email"], email);
    assert!(body["expires_in"].as_i64().unwrap_or(0) > 0);

    let user_id: uuid::Uuid = body["user"]["id"]
        .as_str()
        .expect("an id")
        .parse()
        .expect("a uuid");

    // The token register returned is accepted by a protected route.
    let (status, body) = get(&app, "/auth/me", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["email"], email);
    assert_eq!(body["id"], user_id.to_string());

    db::users::delete_user(database.pool(), user_id)
        .await
        .expect("the test must leave nothing behind");
}

#[tokio::test]
async fn login_returns_a_token_that_works() {
    let Some((app, database)) = app().await else {
        return;
    };
    let email = unique_email();
    let (_, registered) = post(
        &app,
        "/auth/register",
        json!({ "email": email, "password": "a-good-password" }),
    )
    .await;
    let user_id: uuid::Uuid = registered["user"]["id"].as_str().unwrap().parse().unwrap();

    let (status, body) = post(
        &app,
        "/auth/login",
        json!({ "email": email, "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("a token");

    let (status, _) = get(&app, "/auth/me", Some(token)).await;
    assert_eq!(status, StatusCode::OK);

    db::users::delete_user(database.pool(), user_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_email_is_case_insensitive_at_the_router() {
    // The column is UNIQUE and case-sensitive, so this only holds because the
    // router normalizes before it queries. Registering `Alice@…` and logging in
    // as `alice@…` is what a user expects to work.
    let Some((app, database)) = app().await else {
        return;
    };
    let email = unique_email();
    let shouting = email.to_uppercase();

    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": shouting.clone(), "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let user_id: uuid::Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let (status, body) = post(
        &app,
        "/auth/login",
        json!({ "email": email, "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "lower-case login failed: {body}");

    db::users::delete_user(database.pool(), user_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_duplicate_email_is_a_conflict_not_a_server_error() {
    let Some((app, database)) = app().await else {
        return;
    };
    let email = unique_email();
    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": email.clone(), "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let user_id: uuid::Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": email, "password": "another-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "EMAIL_ALREADY_REGISTERED");

    db::users::delete_user(database.pool(), user_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_login_failure_looks_identical_to_the_client() {
    let Some((app, database)) = app().await else {
        return;
    };
    let email = unique_email();
    let (_, registered) = post(
        &app,
        "/auth/register",
        json!({ "email": email.clone(), "password": "a-good-password" }),
    )
    .await;
    let user_id: uuid::Uuid = registered["user"]["id"].as_str().unwrap().parse().unwrap();

    let (wrong_status, wrong_body) = post(
        &app,
        "/auth/login",
        json!({ "email": email.clone(), "password": "not-the-password" }),
    )
    .await;
    let (unknown_status, unknown_body) = post(
        &app,
        "/auth/login",
        json!({ "email": unique_email(), "password": "not-the-password" }),
    )
    .await;

    // Same status, same code, same message: a client cannot tell a registered
    // email from an unregistered one, which is the point.
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_body["error"], unknown_body["error"]);
    assert_eq!(wrong_body["error"]["code"], "INVALID_CREDENTIALS");

    db::users::delete_user(database.pool(), user_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_protected_route_without_a_token_is_401() {
    let Some((app, _)) = app().await else {
        return;
    };
    let (status, body) = get(&app, "/auth/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    // A malformed token is the same 401, not a 500.
    let (status, _) = get(&app, "/auth/me", Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_token_signed_with_another_secret_is_refused_by_the_route() {
    let Some((app, _)) = app().await else {
        return;
    };
    let forged = AuthConfig::new("a-different-secret-that-is-also-long-enough")
        .issue(uuid::Uuid::new_v4(), "attacker@example.com", 1_700_000_000)
        .unwrap();
    let (status, body) = get(&app, "/auth/me", Some(&forged)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn a_short_password_and_a_bad_email_are_400_with_codes() {
    let Some((app, _)) = app().await else {
        return;
    };

    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": unique_email(), "password": "short" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "PASSWORD_TOO_SHORT");

    let (status, body) = post(
        &app,
        "/auth/register",
        json!({ "email": "not-an-email", "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "EMAIL_INVALID");
}

#[tokio::test]
async fn the_public_routes_need_no_token() {
    let Some((app, _)) = app().await else {
        return;
    };
    // docs/12: the public set is decided explicitly rather than defaulted open.
    // Health and market data are public; nothing that belongs to a user is.
    let (status, _) = get(&app, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/skills", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_deployment_without_a_secret_says_so_instead_of_401ing() {
    // No JWT_SECRET means nothing can be authenticated. A 401 would send the
    // client to a login form that cannot work; 503 names the missing variable.
    let _ = dotenvy::dotenv();
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL is not set; skipping");
        return;
    }
    let Ok(database) = db::Database::from_env().await else {
        return;
    };
    let state = AppState {
        db: Some(Arc::new(database)),
        agent: None,
        skills: Arc::new(ai_agent::SkillLibrary::new()),
        auth: None,
    };
    let app = router(state);

    let (status, body) = get(&app, "/auth/me", Some("any-token")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "UNAVAILABLE");

    let (status, body) = post(
        &app,
        "/auth/login",
        json!({ "email": "a@b.co", "password": "a-good-password" }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JWT_SECRET"),
        "{body}"
    );
}
