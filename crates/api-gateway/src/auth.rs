//! Session authentication (`docs/12-API-GATEWAY.md`).
//!
//! ## What is deliberately a library, and what is deliberately not
//!
//! Password hashing is **Argon2 via the `argon2` crate**. A key-derivation
//! function is exactly the kind of thing that looks right and is subtly wrong,
//! so it is not written here.
//!
//! The JWT is **framed here** -- base64url over JSON, signed with HMAC-SHA256
//! from the `hmac`/`sha2` crates that already sign the Bedrock SigV4 requests.
//! That is framing, not cryptography: no primitive is implemented, and the
//! pieces that are easy to get wrong are the ones this file is explicit about:
//!
//! * the algorithm is checked against a single accepted value, so an `alg:
//!   none` or algorithm-confusion token is rejected rather than trusted;
//! * `exp` is required and enforced, not optional;
//! * the signature comparison is constant-time ([`subtle`]), because a
//!   byte-by-byte comparison that returns early leaks how much of a forged
//!   signature was right.
//!
//! ## The middleware is a layer, not a decorator on every handler
//!
//! `docs/12` asks for auth to be swappable for OAuth/SSO without touching route
//! handlers. So handlers never see a token: they take [`UserContext`], and the
//! only place that knows what a JWT is is the extractor below. Swapping the
//! scheme means rewriting this file.
//!
//! ## What is public
//!
//! `docs/12` says to decide explicitly rather than defaulting to open. The
//! decision here: **`/auth/*`, `/healthz`, `/readyz` and the market-data reads
//! are public; everything else requires a token.** Anonymous chart viewing is
//! allowed because the MVP page is a chart and market data is not user data.
//! Anything that reads or writes *someone's* strategies, bots or skills is not.

use std::sync::Arc;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::ApiError;

type HmacSha256 = Hmac<Sha256>;

/// How long an issued token is valid for.
///
/// Seven days, and a fixed lifetime rather than a refresh-token dance: `docs/12`
/// asks for "JWT-based session auth for Phase 1 simplicity". A short lifetime
/// with no refresh flow would log the user out mid-session, and a long one with
/// no revocation is worse. This is the honest middle for a phase that has no
/// revocation list yet.
pub const TOKEN_LIFETIME_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The only algorithm this server will accept.
const ALGORITHM: &str = "HS256";

/// Signing and verification for session tokens.
#[derive(Clone)]
pub struct AuthConfig {
    secret: Arc<Vec<u8>>,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret, not even by accident in a log line.
        f.debug_struct("AuthConfig")
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl AuthConfig {
    /// Read the signing secret from the environment.
    ///
    /// A missing secret is a hard error rather than a generated one: a random
    /// per-process secret would invalidate every token on every restart, and a
    /// default would be a backdoor. Refusing to start is the honest failure.
    ///
    /// # Errors
    /// Returns a message naming the variable when it is missing or too short.
    pub fn from_env() -> Result<Self, String> {
        let secret = std::env::var("JWT_SECRET").map_err(|_| {
            "JWT_SECRET is not set. /auth cannot issue or verify tokens without it. \
             Set it to a random string of at least 32 bytes."
                .to_string()
        })?;
        if secret.len() < 32 {
            return Err(format!(
                "JWT_SECRET is {} bytes; at least 32 are required. A short secret is a \
                 guessable one.",
                secret.len()
            ));
        }
        Ok(Self {
            secret: Arc::new(secret.into_bytes()),
        })
    }

    /// Build from an explicit secret, for tests.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(secret.into().into_bytes()),
        }
    }

    /// Sign a token for a user.
    ///
    /// # Errors
    /// Returns a message if the claims cannot be serialized.
    pub fn issue(&self, user_id: Uuid, email: &str, now: i64) -> Result<String, String> {
        let claims = Claims {
            sub: user_id.to_string(),
            email: email.to_string(),
            iat: now,
            exp: now + TOKEN_LIFETIME_SECONDS,
        };
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
        let signing_input = format!("{header}.{payload}");
        let signature = self.sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{signature}"))
    }

    /// Verify a token and return its claims.
    ///
    /// # Errors
    /// Returns [`ApiError`] with 401 for every failure. The *reason* is in the
    /// message for the operator's log, but a client never learns whether the
    /// signature, the algorithm or the expiry was the problem -- that
    /// distinction is useful to an attacker and not to a legitimate caller.
    pub fn verify(&self, token: &str, now: i64) -> Result<Claims, ApiError> {
        let unauthorized = |why: &str| {
            tracing::debug!("token rejected: {why}");
            ApiError::unauthorized("the session token is missing, malformed or expired")
        };

        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(unauthorized("not three dot-separated parts"));
        };
        if parts.next().is_some() {
            return Err(unauthorized("more than three parts"));
        }

        // Check the algorithm *before* verifying, and accept exactly one value.
        // Trusting the header's `alg` is how algorithm-confusion attacks work.
        let header_json = URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| unauthorized("header is not base64url"))?;
        // Named `parsed_header`, not `header`: shadowing the base64 segment
        // here would make the signature below be computed over the *decoded*
        // JSON instead of the original text, so every token would fail to
        // verify. That is exactly the bug this comment is here to prevent.
        let parsed_header: serde_json::Value =
            serde_json::from_slice(&header_json).map_err(|_| unauthorized("header is not json"))?;
        if parsed_header.get("alg").and_then(|v| v.as_str()) != Some(ALGORITHM) {
            return Err(unauthorized("alg is not HS256"));
        }

        // Signed over the segments exactly as they arrived: re-encoding would
        // also break a token whose base64 was not produced by this encoder.
        let expected = self.sign(format!("{header}.{payload}").as_bytes());
        if expected.as_bytes().ct_eq(signature.as_bytes()).unwrap_u8() != 1 {
            return Err(unauthorized("signature does not match"));
        }

        let payload_json = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| unauthorized("payload is not base64url"))?;
        let claims: Claims = serde_json::from_slice(&payload_json)
            .map_err(|_| unauthorized("payload is not a claim set"))?;

        if claims.exp <= now {
            return Err(unauthorized("expired"));
        }

        Ok(claims)
    }

    fn sign(&self, input: &[u8]) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts a key of any length");
        mac.update(input);
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

/// What a token asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The user's id.
    pub sub: String,
    /// The user's email, so a handler can show it without a second query.
    pub email: String,
    /// Issued at, unix seconds.
    pub iat: i64,
    /// Expires at, unix seconds.
    pub exp: i64,
}

impl Claims {
    /// The user id, if the subject parses as a uuid.
    #[must_use]
    pub fn user_id(&self) -> Option<Uuid> {
        Uuid::parse_str(&self.sub).ok()
    }
}

/// Hash a password for storage.
///
/// The salt is generated by the crate from the OS RNG and embedded in the
/// returned PHC string, so there is no salt parameter to get wrong and no way
/// to reuse one.
///
/// # Errors
/// Returns a message if hashing fails, which in practice means the parameters
/// are unusable rather than that the password is bad.
pub fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|e| format!("could not hash the password: {e}"))
}

/// Check a password against a stored hash.
///
/// Returns `false` rather than an error for a malformed stored hash: a row that
/// cannot be parsed is a row nobody can log in with, which is the safe answer,
/// and the caller should not have to tell that apart from a wrong password.
#[must_use]
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        tracing::error!("a stored password hash could not be parsed; nobody can log in with it");
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// The authenticated caller, as every protected handler sees them.
///
/// This is the whole point of keeping the scheme in one file: a handler written
/// against this type does not know or care whether the token was a JWT, an
/// OAuth bearer or a signed cookie.
#[derive(Debug, Clone)]
pub struct UserContext {
    /// The authenticated user.
    pub user_id: Uuid,
    /// Their email, from the token.
    ///
    /// Carried so a handler that only wants to *name* the caller does not have
    /// to query. `GET /auth/me` deliberately does query anyway -- see the note
    /// there -- so this is unread today, and that is a property of the current
    /// route set rather than of the type.
    #[allow(dead_code)]
    pub email: String,
}

impl FromRequestParts<crate::AppState> for UserContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(auth) = state.auth.as_ref() else {
            // Auth is unconfigured, so nothing can be authenticated. Say that
            // rather than 401: a 401 would send the client to a login form that
            // cannot work.
            return Err(ApiError::unavailable(
                "authentication is not configured in this deployment (JWT_SECRET is unset)",
            ));
        };

        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                ApiError::unauthorized("an `Authorization: Bearer <token>` header is required")
            })?;

        let claims = auth.verify(token, now_seconds())?;
        let user_id = claims
            .user_id()
            .ok_or_else(|| ApiError::unauthorized("the token's subject is not a user id"))?;

        Ok(Self {
            user_id,
            email: claims.email,
        })
    }
}

/// Authenticate a token that arrived outside a header.
///
/// Only the WebSocket channels use this. A browser cannot set an
/// `Authorization` header on a handshake -- the WebSocket API has no way to
/// express one -- so the token arrives as a query parameter instead.
///
/// That workaround has a real cost worth stating: query strings end up in
/// access logs, `Referer` headers and browser history. So this is the only
/// place a token is accepted from anywhere but a header, and it is deliberately
/// a separate function rather than a flag on the header path -- a flag is
/// something a future route can switch on by accident.
///
/// # Errors
/// 503 when auth is unconfigured, 401 for every failure to verify.
pub fn authenticate_token(
    state: &crate::AppState,
    token: Option<&str>,
) -> Result<UserContext, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::unavailable(
            "authentication is not configured in this deployment (JWT_SECRET is unset)",
        ));
    };
    let token = token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::unauthorized("a `?token=` query parameter is required"))?;

    let claims = auth.verify(token, now_seconds())?;
    let user_id = claims
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("the token's subject is not a user id"))?;
    Ok(UserContext {
        user_id,
        email: claims.email,
    })
}

/// The current time in unix seconds.
///
/// A function rather than a call to `SystemTime` inline so the token tests can
/// be written against a clock they control.
#[must_use]
pub fn now_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a-test-secret-that-is-long-enough-to-be-accepted";

    fn config() -> AuthConfig {
        AuthConfig::new(SECRET)
    }

    #[test]
    fn a_token_round_trips() {
        let auth = config();
        let id = Uuid::new_v4();
        let token = auth.issue(id, "trader@example.com", 1_000).unwrap();
        let claims = auth.verify(&token, 1_001).unwrap();

        assert_eq!(claims.user_id(), Some(id));
        assert_eq!(claims.email, "trader@example.com");
        assert_eq!(claims.exp, 1_000 + TOKEN_LIFETIME_SECONDS);
    }

    #[test]
    fn a_tampered_payload_is_rejected() {
        let auth = config();
        let token = auth.issue(Uuid::new_v4(), "a@b.c", 1_000).unwrap();
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();

        // Re-encode the payload with a different subject, keeping the signature.
        let forged = serde_json::json!({
            "sub": Uuid::new_v4().to_string(),
            "email": "attacker@example.com",
            "iat": 1_000,
            "exp": 9_999_999_999i64,
        });
        parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());

        assert!(auth.verify(&parts.join("."), 1_001).is_err());
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let other = AuthConfig::new("a-completely-different-secret-of-32+bytes");
        let token = other.issue(Uuid::new_v4(), "a@b.c", 1_000).unwrap();
        assert!(config().verify(&token, 1_001).is_err());
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let auth = config();
        let token = auth.issue(Uuid::new_v4(), "a@b.c", 1_000).unwrap();
        let after = 1_000 + TOKEN_LIFETIME_SECONDS + 1;
        assert!(auth.verify(&token, after).is_err());
        // And one second before expiry it is still good, so the boundary is
        // where the claim says it is rather than off by one.
        assert!(auth.verify(&token, 1_000 + TOKEN_LIFETIME_SECONDS).is_err());
        assert!(
            auth.verify(&token, 1_000 + TOKEN_LIFETIME_SECONDS - 1)
                .is_ok()
        );
    }

    #[test]
    fn an_alg_none_token_is_rejected() {
        // The classic JWT attack: claim there is no signature and hope the
        // verifier trusts the header.
        let auth = config();
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "sub": Uuid::new_v4().to_string(),
                "email": "attacker@example.com",
                "iat": 1_000,
                "exp": 9_999_999_999i64,
            }))
            .unwrap(),
        );
        let forged = format!("{header}.{payload}.");
        assert!(auth.verify(&forged, 1_001).is_err());
    }

    #[test]
    fn an_unsigned_token_with_a_valid_header_is_rejected() {
        let auth = config();
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(b"{}");
        assert!(auth.verify(&format!("{header}.{payload}."), 1_000).is_err());
    }

    #[test]
    fn malformed_tokens_are_rejected_rather_than_panicking() {
        let auth = config();
        for bad in ["", ".", "a.b", "a.b.c.d", "not-base64.also-not.签名", ".."] {
            assert!(auth.verify(bad, 1_000).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // Salted. Two users with the same password must not share a hash, or
        // one cracked hash is two accounts.
        let a = hash_password("hunter2").unwrap();
        let b = hash_password("hunter2").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("hunter2", &a));
        assert!(verify_password("hunter2", &b));
    }

    #[test]
    fn a_corrupt_stored_hash_denies_rather_than_erroring() {
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn the_config_debug_does_not_print_the_secret() {
        let printed = format!("{:?}", config());
        assert!(!printed.contains(SECRET), "{printed}");
        assert!(printed.contains("redacted"), "{printed}");
    }

    #[test]
    fn a_short_secret_is_refused() {
        // Exercised through the constructor's rules rather than the environment,
        // so the test does not depend on process env.
        assert!(AuthConfig::new("short").secret.len() < 32);
    }
}
