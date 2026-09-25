//! Agent memory: what the AI believes, kept between conversations (`docs/09`).
//!
//! ## The gap this module exists for
//!
//! The agent could read the user's drawings and the market, but every
//! conversation started from zero. It would rediscover "4H resistance at
//! 108,500" on Tuesday what it had established on Monday, because a thesis is
//! derived fresh from tools and then discarded. This module is the fact store
//! between conversations: one row per fact, addressed by the user, a scope,
//! and a key.
//!
//! ## Newest-wins semantics
//!
//! A memory is a *claim the agent currently stands behind*, not a chat log.
//! When the agent states the same key again -- the level moved, the thesis
//! changed -- [`remember`] **updates** the row and refreshes its
//! `updated_at`, because "what does the agent believe *now*" must be one read,
//! not a GROUP BY. The history is in the conversation, not here.
//!
//! ## Two scopes, one shape
//!
//! `symbol` memories are facts about a market ("4H resistance = 108,500");
//! `global` memories are facts about how this user works ("prefers entries on
//! retests, risks 0.5R"). One table, one write path, one read path per scope.
//!
//! ## Why the upsert names its conflict target
//!
//! The key uniqueness lives in two *partial* unique indexes (0010), one per
//! scope shape, because PostgreSQL 13 treats NULLs as distinct and a global
//! row's symbol is NULL. Partial indexes cannot be merged into one `ON
//! CONFLICT` target, so [`remember`] runs the statement for the scope it was
//! given -- each with the index it owns. A generic `ON CONFLICT DO NOTHING`
//! would have turned a re-statement into a silent no-op, which is the opposite
//! of newest-wins.
//!
//! ## Retention
//!
//! [`remember`] trims the user's rows to [`MAX_MEMORIES`] after each write,
//! evicting the least recently *updated*. The limit keeps a prompt's recall
//! section bounded and the table from growing forever; the right number is a
//! product decision that lives here, where it can change in one line.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// Facts kept per user, across both scopes.
///
/// Large enough to hold every level and preference a working trader accrues in
/// months; small enough that a prompt's recall section -- which injects the
/// newest few dozen -- stays a rounding error against the model's context.
pub const MAX_MEMORIES: usize = 500;

/// A stored memory.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct MemoryRow {
    /// The row id.
    pub id: Uuid,
    /// `symbol` or `global`.
    pub scope: String,
    /// The symbol, exactly when [`MemoryRow::scope`] is `symbol`.
    pub symbol: Option<String>,
    /// What the fact is about, e.g. `4h_resistance`.
    pub key: String,
    /// The fact, in the agent's own words.
    pub content: String,
    /// When it was first stated, unix nanoseconds.
    pub created_at: i64,
    /// When it was last stated. This is the sort key for recall.
    pub updated_at: i64,
}

/// A memory about to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMemory {
    /// `symbol` or `global`. Anything else is refused before the database is
    /// asked, because the CHECK would refuse it anyway and the route can
    /// answer sooner with a better message.
    pub scope: String,
    /// The symbol, required exactly when the scope is `symbol`.
    pub symbol: Option<String>,
    /// What the fact is about.
    pub key: String,
    /// The fact.
    pub content: String,
}

/// Validate a memory's scope shape, for both writers.
fn validate(memory: &NewMemory) -> Result<(), DbError> {
    if memory.scope != "symbol" && memory.scope != "global" {
        return Err(DbError::InvalidConfig(format!(
            "memory scope must be `symbol` or `global`, got `{}`",
            memory.scope
        )));
    }
    if memory.scope == "symbol" && memory.symbol.is_none() {
        return Err(DbError::InvalidConfig(
            "a symbol memory needs a symbol".to_string(),
        ));
    }
    if memory.scope == "global" && memory.symbol.is_some() {
        return Err(DbError::InvalidConfig(
            "a global memory does not carry a symbol".to_string(),
        ));
    }
    if memory.key.trim().is_empty() || memory.content.trim().is_empty() {
        return Err(DbError::InvalidConfig(
            "a memory needs a non-empty key and content".to_string(),
        ));
    }
    Ok(())
}

/// Store a fact, replacing any earlier claim under the same key.
///
/// The upsert is scoped to the user and the key's scope, so the agent stating
/// `4h_resistance` for BTCUSDT never overwrites the same key for ETHUSDT or
/// another user's anything.
///
/// # Errors
/// Returns [`DbError`] if a scope shape is wrong or the write fails.
pub async fn remember(pool: &PgPool, user_id: Uuid, memory: &NewMemory) -> Result<Uuid, DbError> {
    validate(memory)?;

    let id: Uuid = if memory.scope == "symbol" {
        sqlx::query_scalar(
            "INSERT INTO agent_memory (user_id, scope, symbol, key, content) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (user_id, symbol, key) WHERE scope = 'symbol' \
             DO UPDATE SET content = EXCLUDED.content, updated_at = now() \
             RETURNING id",
        )
        .bind(user_id)
        .bind(&memory.scope)
        .bind(memory.symbol.as_deref())
        .bind(memory.key.trim())
        .bind(memory.content.trim())
        .fetch_one(pool)
        .await?
    } else {
        sqlx::query_scalar(
            "INSERT INTO agent_memory (user_id, scope, symbol, key, content) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (user_id, key) WHERE scope <> 'symbol' \
             DO UPDATE SET content = EXCLUDED.content, updated_at = now() \
             RETURNING id",
        )
        .bind(user_id)
        .bind(&memory.scope)
        .bind(None::<String>)
        .bind(memory.key.trim())
        .bind(memory.content.trim())
        .fetch_one(pool)
        .await?
    };

    trim_to_cap(pool, user_id).await?;
    Ok(id)
}

/// What the agent currently believes, for one symbol -- plus its global
/// preferences -- most recently stated first.
///
/// The global rows ride along because "how this user works" belongs beside
/// "what this market is doing" in every recall, and a caller that wants only
/// one scope filters the small vector itself.
///
/// # Errors
/// Returns [`DbError`] if the read fails.
pub async fn recall(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    limit: i64,
) -> Result<Vec<MemoryRow>, DbError> {
    // The timestamps are `TIMESTAMPTZ` in the schema and nanoseconds
    // everywhere else in this crate; the cast below is this module's
    // `dt_to_ns`, inline, because `FromRow` takes the row as it comes.
    let rows = sqlx::query_as::<_, MemoryRow>(
        "SELECT id, scope, symbol, key, content, \
         (extract(epoch from created_at) * 1e9)::bigint AS created_at, \
         (extract(epoch from updated_at) * 1e9)::bigint AS updated_at \
         FROM agent_memory \
         WHERE user_id = $1 AND (symbol = $2 OR scope <> 'symbol') \
         ORDER BY updated_at DESC \
         LIMIT $3",
    )
    .bind(user_id)
    .bind(symbol)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Forget one memory, reporting whether there was one to forget.
///
/// `false` is not an error, for the same reason `delete_drawing`'s `false`
/// is not: forgetting twice leaves the world in the state that was asked for.
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn forget(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<bool, DbError> {
    let deleted: Option<Uuid> =
        sqlx::query_scalar("DELETE FROM agent_memory WHERE id = $1 AND user_id = $2 RETURNING id")
            .bind(id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(deleted.is_some())
}

/// Forget by the fact's address rather than its row id.
///
/// The agent's tools speak (scope, symbol, key) -- the model never sees row
/// ids, because they are storage plumbing and a stale one in a prompt is a
/// correction the model cannot make. `symbol` is `Some` for a symbol-scoped
/// fact and `None` for a global one, mirroring the unique indexes: a global
/// `risk_style` and a BTCUSDT `risk_style` are different rows, and dropping
/// one must not drop the other.
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn forget_by_key(
    pool: &PgPool,
    user_id: Uuid,
    symbol: Option<&str>,
    key: &str,
) -> Result<bool, DbError> {
    let deleted: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM agent_memory \
         WHERE user_id = $1 AND key = $2 AND symbol IS NOT DISTINCT FROM $3 \
         RETURNING id",
    )
    .bind(user_id)
    .bind(key.trim())
    .bind(symbol)
    .fetch_optional(pool)
    .await?;
    Ok(deleted.is_some())
}

/// Keep the newest [`MAX_MEMORIES`] rows for this user, evicting the rest.
///
/// Run after every write, because a cap that is only enforced by a cron job is
/// a cap that quietly stops mattering the day the job stops running.
async fn trim_to_cap(pool: &PgPool, user_id: Uuid) -> Result<(), DbError> {
    sqlx::query(
        "DELETE FROM agent_memory \
         WHERE user_id = $1 AND id NOT IN (\
           SELECT id FROM agent_memory WHERE user_id = $1 \
           ORDER BY updated_at DESC LIMIT $2\
         )",
    )
    .bind(user_id)
    .bind(MAX_MEMORIES as i64)
    .execute(pool)
    .await?;
    Ok(())
}
