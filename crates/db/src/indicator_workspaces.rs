//! Durable AI-generated indicator workspaces and immutable revisions.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// A client-owned persistent AI indicator workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorWorkspaceRow {
    /// Workspace id.
    pub id: Uuid,
    /// Client-provided name.
    pub name: String,
    /// Market symbol.
    pub symbol: String,
    /// Primary chart timeframe.
    pub timeframe: String,
    /// Compact durable chat memory.
    pub memory: Value,
    /// Last valid revision attached to a chart, if any.
    pub active_revision_id: Option<Uuid>,
    /// Creation time in unix nanoseconds.
    pub created_at: i64,
    /// Last modification time in unix nanoseconds.
    pub updated_at: i64,
}

/// One immutable generated source revision.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorRevisionRow {
    /// Revision id.
    pub id: Uuid,
    /// Workspace that owns it.
    pub workspace_id: Uuid,
    /// Prior revision when this was an edit.
    pub parent_revision_id: Option<Uuid>,
    /// Monotonic workspace-local sequence number.
    pub revision_number: i32,
    /// Read-only generated source.
    pub source: String,
    /// Plain-language explanation of the rules.
    pub summary: String,
    /// What changed from its parent.
    pub change_summary: String,
    /// Compiler/sandbox validation report.
    pub validation: Value,
    /// Deterministic preview report and output.
    pub preview: Value,
    /// `validated` or `rejected`.
    pub status: String,
    /// Creation time in unix nanoseconds.
    pub created_at: i64,
}

fn workspace_from(row: &sqlx::postgres::PgRow) -> Result<IndicatorWorkspaceRow, sqlx::Error> {
    Ok(IndicatorWorkspaceRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        symbol: row.try_get("symbol")?,
        timeframe: row.try_get("timeframe")?,
        memory: row.try_get("memory")?,
        active_revision_id: row.try_get("active_revision_id")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        updated_at: dt_to_ns(row.try_get("updated_at")?),
    })
}

fn revision_from(row: &sqlx::postgres::PgRow) -> Result<IndicatorRevisionRow, sqlx::Error> {
    Ok(IndicatorRevisionRow {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        parent_revision_id: row.try_get("parent_revision_id")?,
        revision_number: row.try_get("revision_number")?,
        source: row.try_get("source")?,
        summary: row.try_get("summary")?,
        change_summary: row.try_get("change_summary")?,
        validation: row.try_get("validation")?,
        preview: row.try_get("preview")?,
        status: row.try_get("status")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Create a workspace with no attached indicator.
pub async fn create_indicator_workspace(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    symbol: &str,
    timeframe: &str,
    memory: &Value,
) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO indicator_workspaces (user_id, name, symbol, timeframe, memory) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(symbol)
    .bind(timeframe)
    .bind(memory)
    .fetch_one(pool)
    .await?)
}

/// Read an owned workspace.
pub async fn get_indicator_workspace(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<IndicatorWorkspaceRow>, DbError> {
    let row = sqlx::query(
        "SELECT id, name, symbol, timeframe, memory, active_revision_id, created_at, updated_at \
         FROM indicator_workspaces WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(workspace_from).transpose().map_err(Into::into)
}

/// List a client's workspaces, newest activity first.
pub async fn list_indicator_workspaces(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<IndicatorWorkspaceRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, name, symbol, timeframe, memory, active_revision_id, created_at, updated_at \
         FROM indicator_workspaces WHERE user_id = $1 ORDER BY updated_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(workspace_from).collect::<Result<_, _>>().map_err(Into::into)
}

/// Append a generated revision. Only a validated revision becomes active.
pub async fn create_indicator_revision(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
    parent_revision_id: Option<Uuid>,
    source: &str,
    summary: &str,
    change_summary: &str,
    validation: &Value,
    preview: &Value,
    status: &str,
) -> Result<Option<IndicatorRevisionRow>, DbError> {
    let mut tx = pool.begin().await?;
    let owned = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM indicator_workspaces WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if owned.is_none() {
        return Ok(None);
    }
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM indicator_revisions WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    let row = sqlx::query(
        "INSERT INTO indicator_revisions \
         (workspace_id, parent_revision_id, revision_number, source, summary, change_summary, validation, preview, status) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
         RETURNING id, workspace_id, parent_revision_id, revision_number, source, summary, change_summary, validation, preview, status, created_at",
    )
    .bind(workspace_id).bind(parent_revision_id).bind(next).bind(source).bind(summary)
    .bind(change_summary).bind(validation).bind(preview).bind(status)
    .fetch_one(&mut *tx).await?;
    let revision = revision_from(&row)?;
    sqlx::query(
        "UPDATE indicator_workspaces SET active_revision_id = CASE WHEN $3 = 'validated' THEN $2 ELSE active_revision_id END, updated_at = now() WHERE id = $1",
    )
    .bind(workspace_id).bind(revision.id).bind(status).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(revision))
}

/// List immutable revisions for an owned workspace.
pub async fn list_indicator_revisions(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
    limit: i64,
) -> Result<Vec<IndicatorRevisionRow>, DbError> {
    let rows = sqlx::query(
        "SELECT r.id, r.workspace_id, r.parent_revision_id, r.revision_number, r.source, r.summary, r.change_summary, r.validation, r.preview, r.status, r.created_at \
         FROM indicator_revisions r JOIN indicator_workspaces w ON w.id = r.workspace_id \
         WHERE r.workspace_id = $1 AND w.user_id = $2 ORDER BY r.revision_number DESC LIMIT $3",
    ).bind(workspace_id).bind(user_id).bind(limit).fetch_all(pool).await?;
    rows.iter().map(revision_from).collect::<Result<_, _>>().map_err(Into::into)
}
