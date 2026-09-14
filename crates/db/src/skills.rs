//! Skills (`docs/10`, `docs/13`).
//!
//! ## Why the row is a document, not columns
//!
//! `docs/13` stores a skill as `document JSONB`. The alternative -- a column
//! per field -- would mean a migration every time `Skill` gains one, and an
//! older row could no longer be read back into the struct that wrote it. A
//! past thesis has to remain explainable against the skill version that
//! produced it (`docs/10`), so the document is stored whole.
//!
//! `name`, `version` and `category` are nevertheless real columns: they are
//! what the unique constraint and the listing order need, and duplicating
//! three fields is cheaper than a functional index over JSONB.
//!
//! ## Versions are appended, never rewritten
//!
//! There is no update statement here. `UNIQUE (user_id, name, version)` makes
//! "publish the same version twice" a database-level error, and callers are
//! expected to check [`skill_version_exists`] first so the refusal can name
//! the version instead of surfacing a constraint violation.
//!
//! ## Everything is scoped by owner
//!
//! Every read takes a `user_id`. The shipped skill library on disk is
//! everybody's baseline and is not stored here; merging the two is the
//! gateway's job, not this crate's.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A stored skill document.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillRow {
    /// The row id. Not the id clients use -- see the module docs on slugs.
    pub id: Uuid,
    /// The skill's name.
    pub name: String,
    /// Version string, e.g. `2.1`.
    pub version: String,
    /// Category: `liquidity`, `footprint`, ...
    pub category: String,
    /// The whole skill document.
    pub document: Value,
    /// When it was stored, unix nanos.
    pub created_at: i64,
}

/// Cap on how many of one user's skills a listing will read.
///
/// Skills are methodology documents, not market data: a user has tens of them,
/// not millions. The limit exists so a listing cannot grow without bound, not
/// because the query is expensive.
pub const MAX_SKILLS: i64 = 500;

fn skill_from_row(row: &sqlx::postgres::PgRow) -> Result<SkillRow, sqlx::Error> {
    Ok(SkillRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        version: row.try_get("version")?,
        category: row.try_get("category")?,
        document: row.try_get("document")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Whether this user already has a skill with this exact name and version.
///
/// Checked before an insert so the refusal can be a 409 that names the
/// version, rather than a constraint violation that names an index.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn skill_version_exists(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    version: &str,
) -> Result<bool, DbError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM skills WHERE user_id = $1 AND name = $2 AND version = $3)",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Store a new skill version.
///
/// Always an insert. Callers must have checked [`skill_version_exists`]:
/// publishing version `2.1` over an existing `2.1` would silently produce two
/// rows that differ only in `created_at`, and "the newest wins" would then
/// depend on clock precision.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn create_skill(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    version: &str,
    category: &str,
    document: &Value,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO skills (user_id, name, version, category, document) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .bind(category)
    .bind(document)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// This user's skills, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_skills(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<SkillRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, name, version, category, document, created_at \
         FROM skills WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(skill_from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Remove one skill row.
///
/// Not part of any request path -- `docs/12` has no delete for skills, and
/// deleting one would leave past theses pointing at nothing. It exists so an
/// integration test can create real rows and leave the database as it found it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_skill(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM skills WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
