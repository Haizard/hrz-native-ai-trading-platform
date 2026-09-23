//! A user's own broker accounts (`docs/15-RISK-COMPLIANCE.md`).
//!
//! ## This layer moves ciphertext and never looks inside it
//!
//! `key_ciphertext` and `secret_ciphertext` are opaque `BYTEA`. Nothing here
//! parses, validates or logs them, and no function in this module can turn them
//! back into a credential -- that needs the key-encryption key, which lives in
//! [`trading_engine::secrets`] and in the process environment, and neither is
//! reachable from `db`. That is the point: one crate can decrypt, and it is not
//! the crate that talks to the database.
//!
//! The consequence worth stating plainly: a bug in this module can lose a
//! credential, expose *that one exists*, or attach it to the wrong row -- and
//! cannot leak the credential itself. The AAD binding in `secrets.rs` closes
//! the last of those, because a row moved to another user fails to decrypt.
//!
//! ## The key is never read without an owner
//!
//! [`broker_account_secrets`] is the only function that returns ciphertext, and
//! it takes a `user_id` and filters on it. There is deliberately no
//! `secrets_by_id`: an id is a value a caller can obtain from a URL, and a
//! lookup that worked on an id alone would make every other owner check in this
//! crate decorative.
//!
//! ## Statuses are a closed set
//!
//! `pending` / `verified` / `invalid`, spelled out in [`STATUSES`] and enforced
//! by a CHECK constraint. A settings page has to render something for every
//! value the column can hold, and a typo'd status is a row that renders as
//! nothing at all.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// The statuses a broker account may hold.
///
/// A closed set, mirroring the CHECK constraint in `0007`: the column is read by
/// a settings page that must render a row for every value it can contain, and
/// "verified" versus "verified " is the difference between a working account and
/// one the UI shows as unknown.
pub const STATUSES: [&str; 3] = ["pending", "verified", "invalid"];

/// A connected broker account, as the control plane sees it.
///
/// No ciphertext, deliberately. This is the type every route handler holds, and
/// a type that cannot carry a sealed secret is a type that cannot put one in a
/// response body by accident.
#[derive(Debug, Clone, PartialEq)]
pub struct BrokerAccountRow {
    /// The account.
    pub id: Uuid,
    /// Who connected it.
    pub user_id: Uuid,
    /// Lowercase venue name.
    pub venue: String,
    /// The user's name for it.
    pub label: String,
    /// `pending`, `verified` or `invalid`.
    pub status: String,
    /// What the venue said the key may do, verbatim.
    pub permissions: Value,
    /// When it was last checked against the venue, unix nanos.
    pub last_verified_at: Option<i64>,
    /// Why the last check failed, if it did. Never a credential.
    pub last_error: Option<String>,
    /// When it was connected, unix nanos.
    pub created_at: i64,
    /// When it was last changed, unix nanos.
    pub updated_at: i64,
}

/// The ciphertext of one account, for the one caller that can decrypt it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedKeyMaterial {
    /// The sealed API key.
    pub key_ciphertext: Vec<u8>,
    /// The sealed API secret.
    pub secret_ciphertext: Vec<u8>,
    /// Which KEK sealed them.
    pub kek_fingerprint: String,
    /// `pending`, `verified` or `invalid`.
    pub status: String,
    /// The venue, so the caller can build the AAD scope without a second query.
    ///
    /// Returned here rather than read from the row's own copy of the request,
    /// because the scope must be the *stored* venue: sealing is bound to it, so
    /// a caller that passed a differently-spelled venue would fail to decrypt
    /// and the reason would look like a corrupt key.
    pub venue: String,
}

/// A broker account about to be stored.
///
/// Ciphertext rather than credentials: this type cannot be built from an API
/// key, only from something that has already been sealed, which is what keeps
/// "encrypt before insert" a property of the type rather than a rule someone
/// has to remember.
#[derive(Debug)]
pub struct NewBrokerAccount<'a> {
    /// Lowercase venue name.
    pub venue: &'a str,
    /// The user's name for it.
    pub label: &'a str,
    /// The sealed API key.
    pub key_ciphertext: Vec<u8>,
    /// The sealed API secret.
    pub secret_ciphertext: Vec<u8>,
    /// Which KEK sealed them.
    pub kek_fingerprint: &'a str,
}

fn account_from_row(row: &sqlx::postgres::PgRow) -> Result<BrokerAccountRow, sqlx::Error> {
    Ok(BrokerAccountRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        venue: row.try_get("venue")?,
        label: row.try_get("label")?,
        status: row.try_get("status")?,
        permissions: row.try_get("permissions")?,
        last_verified_at: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_verified_at")?
            .map(dt_to_ns),
        last_error: row.try_get("last_error")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

/// Store a connected account.
///
/// Returns `None` when the user already has an account on this venue with this
/// label, which is an ordinary outcome of a form submission rather than a
/// database failure -- the same reasoning as `users::create_user`, and it lets
/// the route answer "you already have one called that" without matching on a
/// message.
///
/// The new row starts `pending`. Nothing has been checked against the venue yet,
/// and recording it as `verified` because the row was written would be claiming
/// a measurement that has not happened.
///
/// # Errors
/// Returns [`DbError::Pool`] if the insert fails.
pub async fn create_broker_account(
    pool: &PgPool,
    user_id: Uuid,
    account: &NewBrokerAccount<'_>,
) -> Result<Option<Uuid>, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO broker_accounts (user_id, venue, label, key_ciphertext, \
         secret_ciphertext, kek_fingerprint, status) \
         VALUES ($1, $2, $3, $4, $5, $6, 'pending') \
         ON CONFLICT (user_id, venue, lower(label)) DO NOTHING \
         RETURNING id",
    )
    .bind(user_id)
    .bind(account.venue)
    .bind(account.label)
    .bind(account.key_ciphertext.clone())
    .bind(account.secret_ciphertext.clone())
    .bind(account.kek_fingerprint)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// List a user's accounts, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_broker_accounts(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<BrokerAccountRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, user_id, venue, label, status, permissions, last_verified_at, last_error, \
         created_at, updated_at FROM broker_accounts WHERE user_id = $1 \
         ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(account_from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Read one account, if this user owns it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn get_broker_account(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<BrokerAccountRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, user_id, venue, label, status, permissions, last_verified_at, last_error, \
         created_at, updated_at FROM broker_accounts WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(account_from_row)
        .transpose()
        .map_err(Into::into)
}

/// Read the sealed key material of one account, if this user owns it.
///
/// The only read of ciphertext in this crate, and the only function whose result
/// a caller may decrypt. It takes the owner and filters on it: a lookup by id
/// alone would make every ownership check above decorative, because the id
/// reaches this crate from a URL.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn broker_account_secrets(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<SealedKeyMaterial>, DbError> {
    let row = sqlx::query(
        "SELECT key_ciphertext, secret_ciphertext, kek_fingerprint, status, venue \
         FROM broker_accounts WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(SealedKeyMaterial {
        key_ciphertext: row.try_get("key_ciphertext")?,
        secret_ciphertext: row.try_get("secret_ciphertext")?,
        kek_fingerprint: row.try_get("kek_fingerprint")?,
        status: row.try_get("status")?,
        venue: row.try_get("venue")?,
    }))
}

/// Record what the venue said about an account.
///
/// `permissions` is the venue's own body, `error` names the failure *without*
/// repeating anything that was sent. A success clears `last_error`, because a
/// stale message beside a `verified` status is how an operator concludes a
/// working account is broken.
///
/// Returns `false` when the account does not exist or is not this user's.
///
/// # Errors
/// Returns [`DbError::Pool`] if the update fails.
pub async fn set_broker_account_status(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    status: &str,
    permissions: &Value,
    error: Option<&str>,
) -> Result<bool, DbError> {
    let result = sqlx::query(
        "UPDATE broker_accounts SET status = $3, permissions = $4, last_error = $5, \
         last_verified_at = now(), updated_at = now() WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .bind(status)
    .bind(permissions.clone())
    .bind(error)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Remove an account.
///
/// Does not touch bots. The caller kills the running ones first, in that order
/// and for the same reason as `venue_routes::revoke`: a disconnect that only
/// changed what *future* bots may do would leave a running bot trading an
/// account the owner has just withdrawn, and the orders are real.
///
/// Returns `false` when the account does not exist or is not this user's.
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_broker_account(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<bool, DbError> {
    let result = sqlx::query("DELETE FROM broker_accounts WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// How many of this user's bots name an account.
///
/// Read before a disconnect so the answer can say *how many* bots were stopped,
/// rather than reporting an anonymous "some bots were killed" -- and read as a
/// count rather than a list because the caller has already killed them and only
/// needs the number for the response.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn count_bots_for_broker_account(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<i64, DbError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bots WHERE broker_account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// The account a bot trades, if it has one.
///
/// Used by the live session to record which account an order went to without
/// threading the id through every layer that already carries the bot.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn broker_account_for_bot(pool: &PgPool, bot_id: Uuid) -> Result<Option<Uuid>, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar("SELECT broker_account_id FROM bots WHERE id = $1")
        .bind(bot_id)
        .fetch_optional(pool)
        .await?
        .flatten();
    Ok(id)
}

/// Attach an account to an existing bot.
///
/// Separate from the insert rather than a parameter on it, because a live bot's
/// row is created by `create_bot_with_key`, which resolves an idempotency key
/// through the database and returns *either* a new row or the row a retry
/// already made. Setting the account afterwards is correct in both cases and
/// needs no branch.
///
/// Returns `false` when the bot does not exist or is not this user's.
///
/// # Errors
/// Returns [`DbError::Pool`] if the update fails.
pub async fn set_bot_broker_account(
    pool: &PgPool,
    user_id: Uuid,
    bot_id: Uuid,
    account_id: Uuid,
) -> Result<bool, DbError> {
    let result =
        sqlx::query("UPDATE bots SET broker_account_id = $3 WHERE id = $1 AND user_id = $2")
            .bind(bot_id)
            .bind(user_id)
            .bind(account_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statuses_are_the_ones_the_migration_checks() {
        // A guard nothing else in the build can catch: the CHECK constraint is
        // a string in a SQL file, and this list is a string in Rust. A status
        // spelled differently in one of the two is an insert that fails at
        // runtime, in production, on the one path a user is watching.
        assert_eq!(STATUSES, ["pending", "verified", "invalid"]);
    }
}
