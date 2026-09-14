//! `/auth/*` (`docs/12-API-GATEWAY.md`).
//!
//! ## Register and login return the same shape
//!
//! Both hand back `{ token, user }`, so a client has one code path for "now I
//! am logged in" rather than two that drift.
//!
//! ## Login says one thing about every failure
//!
//! Wrong password, no such account, and a corrupt stored hash all produce the
//! same 401 with the same message. Telling them apart is a gift to anyone
//! enumerating accounts: "no such user" confirms an email is not registered,
//! and "wrong password" confirms it is. The distinction goes in the server log,
//! where it is useful, and not in the response.
//!
//! ## Password rules are deliberately minimal
//!
//! A minimum length and nothing else. Composition rules ("one digit, one
//! symbol") push people towards `Password1!` and are no longer recommended;
//! Argon2 makes the length the only thing that matters much. The check exists
//! so an empty or one-character password is refused, not to grade the user.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::auth::{self, UserContext};
use crate::error::ApiError;
use crate::AppState;

/// Shortest password accepted.
const MIN_PASSWORD_LEN: usize = 8;

/// Longest email accepted, matching the practical RFC limit.
const MAX_EMAIL_LEN: usize = 254;

/// `POST /auth/register`.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    /// Email to register.
    pub email: String,
    /// Password, in the clear over TLS only.
    pub password: String,
}

/// `POST /auth/login`.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Email to log in with.
    pub email: String,
    /// Password to check.
    pub password: String,
}

/// The account, as a client may see it.
#[derive(Debug, Serialize, Deserialize)]
pub struct PublicUser {
    /// Account id.
    pub id: String,
    /// Account email.
    pub email: String,
}

/// A successful authentication.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    /// The bearer token.
    pub token: String,
    /// Who it belongs to.
    pub user: PublicUser,
    /// Seconds until it expires, so a client can refresh proactively.
    pub expires_in: i64,
}

/// Check the shape of an email without pretending to validate deliverability.
fn check_email(email: &str) -> Result<(), ApiError> {
    let email = email.trim();
    if email.is_empty() {
        return Err(ApiError::bad_request(
            "EMAIL_REQUIRED",
            "an email address is required",
        ));
    }
    if email.len() > MAX_EMAIL_LEN {
        return Err(ApiError::bad_request(
            "EMAIL_TOO_LONG",
            format!("an email address may be at most {MAX_EMAIL_LEN} characters"),
        ));
    }
    // One `@` with something either side. Anything more ambitious is a
    // deliverability question that only sending mail can answer.
    let (local, domain) = email.split_once('@').unwrap_or(("", ""));
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return Err(ApiError::bad_request(
            "EMAIL_INVALID",
            "that does not look like an email address",
        ));
    }
    Ok(())
}

fn check_password(password: &str) -> Result<(), ApiError> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(
            "PASSWORD_TOO_SHORT",
            format!("a password must be at least {MIN_PASSWORD_LEN} characters"),
        ));
    }
    Ok(())
}

/// The database, or a clear 503.
fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; accounts cannot be stored"))
}

/// The auth configuration, or a clear 503.
fn auth_config(state: &AppState) -> Result<&std::sync::Arc<auth::AuthConfig>, ApiError> {
    state.auth.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "authentication is not configured in this deployment (JWT_SECRET is unset)",
        )
    })
}

/// `POST /auth/register`
///
/// # Errors
/// 400 for a malformed email or a short password, 409 if the email is taken,
/// 503 if the deployment has no database or no signing secret.
pub async fn register(
    State(state): State<AppState>,
    Json(request): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<SessionResponse>), ApiError> {
    let database = database(&state)?;
    let auth = auth_config(&state)?;
    check_email(&request.email)?;
    check_password(&request.password)?;

    let hash = auth::hash_password(&request.password)
        .map_err(|e| ApiError::internal(format!("could not hash the password: {e}")))?;

    let Some(user_id) = db::users::create_user(database.pool(), &request.email, &hash).await?
    else {
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            "EMAIL_ALREADY_REGISTERED",
            "that email is already registered",
        ));
    };

    let email = db::users::normalize_email(&request.email);
    info!(user = %user_id, "account registered");
    let token = auth
        .issue(user_id, &email, auth::now_seconds())
        .map_err(|e| ApiError::internal(format!("could not issue a token: {e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(SessionResponse {
            token,
            user: PublicUser {
                id: user_id.to_string(),
                email,
            },
            expires_in: auth::TOKEN_LIFETIME_SECONDS,
        }),
    ))
}

/// `POST /auth/login`
///
/// # Errors
/// 401 for every authentication failure, with one message and one code.
pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    let database = database(&state)?;
    let auth = auth_config(&state)?;

    // The one message every failure returns.
    let rejected = || {
        ApiError::coded(
            StatusCode::UNAUTHORIZED,
            "INVALID_CREDENTIALS",
            "the email or password is incorrect",
        )
    };

    let Some(user) = db::users::find_by_email(database.pool(), &request.email).await? else {
        // Logged with the reason the client is not told, so an operator can
        // tell a typo from an enumeration attempt.
        warn!(email = %db::users::normalize_email(&request.email), "login for an unknown email");
        return Err(rejected());
    };

    if !auth::verify_password(&request.password, &user.password_hash) {
        warn!(user = %user.id, "login with a wrong password");
        return Err(rejected());
    }

    let token = auth
        .issue(user.id, &user.email, auth::now_seconds())
        .map_err(|e| ApiError::internal(format!("could not issue a token: {e}")))?;

    info!(user = %user.id, "login");
    Ok(Json(SessionResponse {
        token,
        user: PublicUser {
            id: user.id.to_string(),
            email: user.email,
        },
        expires_in: auth::TOKEN_LIFETIME_SECONDS,
    }))
}

/// `GET /auth/me`
///
/// Reads the account back from the database rather than echoing the token's
/// claims: a token issued before an email change would otherwise report the old
/// address, and the endpoint's whole job is to say who the caller is *now*.
///
/// # Errors
/// 401 without a valid token; 404 if the account has since been deleted.
pub async fn me(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<PublicUser>, ApiError> {
    let database = database(&state)?;
    let Some(row) = db::users::find_by_id(database.pool(), user.user_id).await? else {
        return Err(ApiError::coded(
            StatusCode::UNAUTHORIZED,
            "ACCOUNT_GONE",
            "the account this token belongs to no longer exists",
        ));
    };
    Ok(Json(PublicUser {
        id: row.id.to_string(),
        email: row.email,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plausible_email_passes() {
        for good in [
            "a@b.co",
            "trader@example.com",
            "first.last+tag@sub.example.co.uk",
        ] {
            assert!(check_email(good).is_ok(), "rejected {good}");
        }
    }

    #[test]
    fn an_implausible_email_is_refused_with_a_code() {
        for bad in [
            "",
            "  ",
            "no-at-sign",
            "@example.com",
            "user@",
            "user@host",
            "a@b@c.com",
        ] {
            let err = check_email(bad).expect_err(&format!("accepted {bad:?}"));
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
            assert!(
                err.code() == "EMAIL_INVALID" || err.code() == "EMAIL_REQUIRED",
                "unexpected code {} for {bad:?}",
                err.code()
            );
        }
    }

    #[test]
    fn a_long_email_is_refused_before_it_reaches_the_column() {
        let long = format!("{}@example.com", "a".repeat(MAX_EMAIL_LEN));
        let err = check_email(&long).expect_err("an over-long email must be refused");
        assert_eq!(err.code(), "EMAIL_TOO_LONG");
    }

    #[test]
    fn a_short_password_is_refused_with_a_code() {
        let err = check_password("short").expect_err("a short password must be refused");
        assert_eq!(err.code(), "PASSWORD_TOO_SHORT");
        assert!(check_password("longenough").is_ok());
    }
}
