//! Accounts (`docs/12-API-GATEWAY.md`, `docs/13-DATABASE-SCHEMA.md`).
//!
//! ## What this layer does not know
//!
//! It stores and returns a password *hash* and never sees a password. Hashing
//! and verification live in `api-gateway`, because that is where the HTTP
//! request is and because a `db` crate that can hash is a `db` crate that can
//! get hashing wrong in a place nobody reviews.
//!
//! ## Emails are matched case-insensitively
//!
//! The schema has `email TEXT UNIQUE`, which is case-*sensitive*. So
//! `Alice@example.com` and `alice@example.com` are two rows today, and a user
//! who signs up with one capital letter would find their account "taken" but
//! unable to log in if they typed it differently. Both functions here lower-case
//! before touching the database, which makes the unique index behave the way a
//! user expects without a migration.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;

/// A stored account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    /// The account id.
    pub id: Uuid,
    /// The account email, as stored.
    pub email: String,
    /// The Argon2 PHC string. Never logged, never returned to a client.
    pub password_hash: String,
}

/// Normalize an email for storage and lookup.
#[must_use]
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Create an account, or return `None` if the email is already registered.
///
/// `None` rather than an error: "that email is taken" is an ordinary outcome of
/// a registration attempt, not a failure of the database, and making it an
/// error would force the caller to pattern-match on a message to tell it apart
/// from a connection problem.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn create_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<Option<Uuid>, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2) \
         ON CONFLICT (email) DO NOTHING RETURNING id",
    )
    .bind(normalize_email(email))
    .bind(password_hash)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Look up an account by email.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<UserRow>, DbError> {
    let row = sqlx::query("SELECT id, email, password_hash FROM users WHERE email = $1")
        .bind(normalize_email(email))
        .fetch_optional(pool)
        .await?;
    row.map(|row| {
        Ok(UserRow {
            id: row.try_get("id")?,
            email: row.try_get("email")?,
            password_hash: row.try_get("password_hash")?,
        })
    })
    .transpose()
}

/// Look up an account by id, for `GET /auth/me`.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<UserRow>, DbError> {
    let row = sqlx::query("SELECT id, email, password_hash FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(|row| {
        Ok(UserRow {
            id: row.try_get("id")?,
            email: row.try_get("email")?,
            password_hash: row.try_get("password_hash")?,
        })
    })
    .transpose()
}

/// Remove an account.
///
/// Same standing as [`crate::paper::purge_bot`]: not part of any request path,
/// and it exists so an integration test can create a real account and leave the
/// database as it found it. Deleting a user's strategies and bots is
/// deliberately *not* done here -- a real deletion is a product decision with
/// a cascade policy, not something to guess at from a helper.
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_user(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_email_is_normalized_before_it_reaches_the_database() {
        // The column is UNIQUE and case-sensitive, so without this a user could
        // register `Alice@x.com` and then be unable to log in as `alice@x.com`.
        assert_eq!(normalize_email("  Alice@Example.COM "), "alice@example.com");
        assert_eq!(normalize_email("a@b.c"), "a@b.c");
        assert_eq!(normalize_email(""), "");
    }

    #[test]
    fn two_spellings_of_one_email_normalize_together() {
        assert_eq!(
            normalize_email("Trader@Example.com"),
            normalize_email("trader@example.com")
        );
    }
}
