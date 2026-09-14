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
//! The router is the one the binary serves -- `api_gateway::router`, via the
//! shared harness in `common` -- rather than a copy assembled here, because a
//! copy is a thing that drifts.
//!
//! ## It cleans up after itself
//!
//! Each test registers an account with a unique email and deletes it at the
//! end, so the database is left as it was found. Without `DATABASE_URL` the
//! tests print why and return, so the suite still runs on a machine with no
//! database.

mod common;

use axum::http::StatusCode;
use common::{Harness, SECRET};
use serde_json::json;

use api_gateway::auth::AuthConfig;

#[tokio::test]
async fn register_then_me_round_trips_through_the_router() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    // The token register returned is accepted by a protected route.
    let (status, body) = h.get("/auth/me", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["email"], user.email);
    assert_eq!(body["id"], user.id.to_string());

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn login_returns_a_token_that_works() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/auth/login",
            json!({ "email": user.email, "password": "a-good-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("a token");

    let (status, _) = h.get("/auth/me", Some(token)).await;
    assert_eq!(status, StatusCode::OK);

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_email_is_case_insensitive_at_the_router() {
    // The column is UNIQUE and case-sensitive, so this only holds because the
    // router normalizes before it queries. Registering `Alice@…` and logging in
    // as `alice@…` is what a user expects to work.
    let Some(h) = Harness::new().await else {
        return;
    };
    let email = format!("gateway-test-{}@example.com", uuid::Uuid::new_v4());

    let (status, body) = h
        .post(
            "/auth/register",
            json!({ "email": email.to_uppercase(), "password": "a-good-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let user_id: uuid::Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let (status, body) = h
        .post(
            "/auth/login",
            json!({ "email": email, "password": "a-good-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "lower-case login failed: {body}");

    db::users::delete_user(h.database.pool(), user_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_duplicate_email_is_a_conflict_not_a_server_error() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/auth/register",
            json!({ "email": user.email, "password": "another-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "EMAIL_ALREADY_REGISTERED");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn every_login_failure_looks_identical_to_the_client() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (wrong_status, wrong_body) = h
        .post(
            "/auth/login",
            json!({ "email": user.email, "password": "not-the-password" }),
            None,
        )
        .await;
    let (unknown_status, unknown_body) = h
        .post(
            "/auth/login",
            json!({ "email": "nobody-here@example.com", "password": "not-the-password" }),
            None,
        )
        .await;

    // Same status, same code, same message: a client cannot tell a registered
    // email from an unregistered one, which is the point.
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_body["error"], unknown_body["error"]);
    assert_eq!(wrong_body["error"]["code"], "INVALID_CREDENTIALS");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_protected_route_without_a_token_is_401() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (status, body) = h.get("/auth/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    // A malformed token is the same 401, not a 500.
    let (status, _) = h.get("/auth/me", Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_token_signed_with_another_secret_is_refused_by_the_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let forged = AuthConfig::new("a-different-secret-that-is-also-long-enough")
        .issue(uuid::Uuid::new_v4(), "attacker@example.com", 1_700_000_000)
        .unwrap();
    let (status, body) = h.get("/auth/me", Some(&forged)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // And a real token still works, so the check is not simply refusing
    // everything.
    let user = h.register().await;
    let (status, _) = h.get("/auth/me", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_short_password_and_a_bad_email_are_400_with_codes() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h
        .post(
            "/auth/register",
            json!({ "email": "someone@example.com", "password": "short" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "PASSWORD_TOO_SHORT");

    let (status, body) = h
        .post(
            "/auth/register",
            json!({ "email": "not-an-email", "password": "a-good-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "EMAIL_INVALID");
}

#[tokio::test]
async fn the_public_routes_need_no_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // docs/12: the public set is decided explicitly rather than defaulted open.
    // Health and market data are public; nothing that belongs to a user is.
    let (status, _) = h.get("/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/skills", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_deployment_without_a_secret_says_so_instead_of_401ing() {
    // No JWT_SECRET means nothing can be authenticated. A 401 would send the
    // client to a login form that cannot work; 503 names the missing variable.
    let Some(h) = Harness::without_auth().await else {
        return;
    };

    let (status, body) = h.get("/auth/me", Some("any-token")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "UNAVAILABLE");

    let (status, body) = h
        .post(
            "/auth/login",
            json!({ "email": "a@b.co", "password": "a-good-password" }),
            None,
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

#[test]
fn the_test_secret_is_long_enough_for_the_real_rule() {
    // The harness constructs `AuthConfig` directly, so nothing would notice if
    // the constant fell below the minimum `from_env` enforces -- and then these
    // tests would be exercising a configuration the server refuses to start
    // with.
    assert!(
        SECRET.len() >= 32,
        "the test secret is {} bytes",
        SECRET.len()
    );
}
