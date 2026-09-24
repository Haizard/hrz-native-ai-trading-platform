//! A user's own AI provider configuration (bring-your-own model).
//!
//! ## This layer moves ciphertext and never looks inside it
//!
//! `api_key_ciphertext` is an opaque `BYTEA`. Nothing here parses, validates
//! or logs it, and no function in this module can turn it back into a key --
//! that needs the key-encryption key, which lives in
//! `trading_engine::secrets` and in the process environment. The same split as
//! [`crate::broker_accounts`], and for the same reason: one crate can decrypt,
//! and it is not the crate that talks to the database.
//!
//! ## One row per user, upserted
//!
//! A provider config is a *setting*, not a versioned artifact: `upsert`
//! overwrites, and the newest value is the only one that matters. Every read
//! takes a `user_id` and filters on it -- there is no `by_id`, because an id
//! is a value a caller can obtain from a URL, and an ownership check that
//! worked on an id alone would make the scoping decorative.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;

/// A stored provider config, without its key.
///
/// This is the type every route handler holds and every response serialises.
/// It cannot carry the ciphertext, which is what keeps a sealed key out of an
/// API response by construction rather than by discipline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigRow {
    /// The owner.
    pub user_id: Uuid,
    /// Provider wire name, e.g. `openai` (matches `ai_agent::ProviderId`).
    pub provider: String,
    /// Model id as the endpoint spells it, e.g. `gpt-4o`.
    pub model_id: String,
    /// Override of the provider's default endpoint, when the user set one.
    pub base_url: Option<String>,
    /// Extra headers the endpoint requires, as a JSON object.
    pub extra_headers: serde_json::Value,
    /// Which KEK sealed the key, so a rotation can find the row.
    pub kek_fingerprint: String,
    /// When it was last changed, unix nanoseconds.
    pub updated_at: i64,
}

/// The ciphertext of a stored key, for the one caller that can decrypt it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedProviderKey {
    /// The sealed API key.
    pub api_key_ciphertext: Vec<u8>,
    /// Which KEK sealed it.
    pub kek_fingerprint: String,
}

/// A provider config about to be stored.
///
/// Ciphertext rather than a key: this type cannot be built from an API key,
/// only from something that has already been sealed, which is what keeps
/// "encrypt before insert" a property of the type rather than a rule someone
/// has to remember.
#[derive(Debug)]
pub struct NewProviderConfig<'a> {
    /// Provider wire name.
    pub provider: &'a str,
    /// Model id.
    pub model_id: &'a str,
    /// The sealed API key.
    pub api_key_ciphertext: Vec<u8>,
    /// Endpoint override, when the user set one.
    pub base_url: Option<&'a str>,
    /// Extra headers, as a JSON object.
    pub extra_headers: &'a serde_json::Value,
    /// Which KEK sealed the key.
    pub kek_fingerprint: &'a str,
}

fn row_from(row: &sqlx::postgres::PgRow) -> Result<ProviderConfigRow, sqlx::Error> {
    Ok(ProviderConfigRow {
        user_id: row.try_get("user_id")?,
        provider: row.try_get("provider")?,
        model_id: row.try_get("model_id")?,
        base_url: row.try_get("base_url")?,
        extra_headers: row.try_get("extra_headers")?,
        kek_fingerprint: row.try_get("kek_fingerprint")?,
        updated_at: crate::repositories::dt_to_ns(row.try_get("updated_at")?),
    })
}

/// Store (or replace) a user's provider config.
///
/// The insert is an upsert on the primary key: a user has exactly one
/// override, and saving a new one replaces the old. The old ciphertext is
/// gone when this returns -- there is no history to leak.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn upsert_provider_config(
    pool: &PgPool,
    user_id: Uuid,
    config: &NewProviderConfig<'_>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO provider_configs \
             (user_id, provider, model_id, api_key_ciphertext, base_url, extra_headers, \
              kek_fingerprint, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now()) \
         ON CONFLICT (user_id) DO UPDATE SET \
             provider = EXCLUDED.provider, \
             model_id = EXCLUDED.model_id, \
             api_key_ciphertext = EXCLUDED.api_key_ciphertext, \
             base_url = EXCLUDED.base_url, \
             extra_headers = EXCLUDED.extra_headers, \
             kek_fingerprint = EXCLUDED.kek_fingerprint, \
             updated_at = now()",
    )
    .bind(user_id)
    .bind(config.provider)
    .bind(config.model_id)
    .bind(config.api_key_ciphertext.clone())
    .bind(config.base_url)
    .bind(config.extra_headers.clone())
    .bind(config.kek_fingerprint)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read a user's provider config, without its key.
///
/// `None` when the user has not stored one -- an ordinary state, not an
/// error: the deployment primary applies.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_provider_config(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<ProviderConfigRow>, DbError> {
    let row = sqlx::query(
        "SELECT user_id, provider, model_id, base_url, extra_headers, kek_fingerprint, updated_at \
         FROM provider_configs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Some(row_from(&row)?)),
        None => Ok(None),
    }
}

/// Read the sealed key for the one caller that can decrypt it.
///
/// The only function here that returns ciphertext, and it takes a `user_id`
/// and filters on it: the same rule as [`crate::broker_accounts`]'s secrets
/// reader, for the same reason.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn provider_config_key(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<SealedProviderKey>, DbError> {
    let row = sqlx::query(
        "SELECT api_key_ciphertext, kek_fingerprint FROM provider_configs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(SealedProviderKey {
        api_key_ciphertext: row.try_get("api_key_ciphertext")?,
        kek_fingerprint: row.try_get("kek_fingerprint")?,
    }))
}

/// Remove a user's provider config.
///
/// Returns `false` when there was nothing stored, which is the outcome the
/// DELETE route reports as "already gone" rather than an error.
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_provider_config(pool: &PgPool, user_id: Uuid) -> Result<bool, DbError> {
    let result = sqlx::query("DELETE FROM provider_configs WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
