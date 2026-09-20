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

/// Per-event, per-revision alert preference. Alerts are opt-in by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorAlertPreference {
    /// Named event emitted by the generated indicator.
    pub event_name: String,
    /// Whether delivery is enabled.
    pub enabled: bool,
    /// Selected delivery channels.
    pub channels: Value,
}

/// One durable message in an owned workspace conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorWorkspaceMessageRow {
    /// Message id.
    pub id: Uuid,
    /// Workspace that owns the conversation.
    pub workspace_id: Uuid,
    /// `user`, `assistant`, or `system`.
    pub role: String,
    /// Machine-readable message category.
    pub kind: String,
    /// Human-readable content.
    pub content: String,
    /// Structured revision or validation detail.
    pub payload: Value,
    /// Creation time in unix nanoseconds.
    pub created_at: i64,
}

/// A bot candidate pinned to one immutable indicator revision.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorBotDraftRow {
    /// Draft id.
    pub id: Uuid,
    /// Workspace that owns the draft.
    pub workspace_id: Uuid,
    /// Immutable source revision the draft uses.
    pub revision_id: Uuid,
    /// Stored, validated strategy executed by the existing bot runtime.
    pub strategy_id: Uuid,
    /// Required historical backtest, once supplied.
    pub backtest_id: Option<Uuid>,
    /// Requested execution mode.
    pub mode: String,
    /// Venue for live execution.
    pub venue: Option<String>,
    /// Requested risk settings.
    pub risk: Value,
    /// `draft`, `approved`, or `promoted`.
    pub status: String,
    /// Approval time in unix nanoseconds.
    pub approved_at: Option<i64>,
    /// Bot created from this draft, if promoted.
    pub bot_id: Option<Uuid>,
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

fn message_from(row: &sqlx::postgres::PgRow) -> Result<IndicatorWorkspaceMessageRow, sqlx::Error> {
    Ok(IndicatorWorkspaceMessageRow {
        id: row.try_get("id")?, workspace_id: row.try_get("workspace_id")?,
        role: row.try_get("role")?, kind: row.try_get("kind")?, content: row.try_get("content")?,
        payload: row.try_get("payload")?, created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

fn draft_from(row: &sqlx::postgres::PgRow) -> Result<IndicatorBotDraftRow, sqlx::Error> {
    Ok(IndicatorBotDraftRow {
        id: row.try_get("id")?, workspace_id: row.try_get("workspace_id")?,
        revision_id: row.try_get("revision_id")?, strategy_id: row.try_get("strategy_id")?,
        backtest_id: row.try_get("backtest_id")?, mode: row.try_get("mode")?, venue: row.try_get("venue")?,
        risk: row.try_get("risk")?, status: row.try_get("status")?,
        approved_at: row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("approved_at")?.map(dt_to_ns),
        bot_id: row.try_get("bot_id")?, created_at: dt_to_ns(row.try_get("created_at")?),
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

/// Read one immutable revision when its workspace belongs to the user.
pub async fn get_indicator_revision(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, revision_id: Uuid,
) -> Result<Option<IndicatorRevisionRow>, DbError> {
    let row = sqlx::query(
        "SELECT r.id, r.workspace_id, r.parent_revision_id, r.revision_number, r.source, r.summary, r.change_summary, r.validation, r.preview, r.status, r.created_at \
         FROM indicator_revisions r JOIN indicator_workspaces w ON w.id = r.workspace_id \
         WHERE r.id = $1 AND r.workspace_id = $2 AND w.user_id = $3",
    ).bind(revision_id).bind(workspace_id).bind(user_id).fetch_optional(pool).await?;
    row.as_ref().map(revision_from).transpose().map_err(Into::into)
}

/// Restore an owned, validated revision as the chart's active revision.
pub async fn restore_indicator_revision(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, revision_id: Uuid,
) -> Result<bool, DbError> {
    let changed = sqlx::query(
        "UPDATE indicator_workspaces w SET active_revision_id = $3, updated_at = now() \
         WHERE w.id = $1 AND w.user_id = $2 AND EXISTS (SELECT 1 FROM indicator_revisions r WHERE r.id = $3 AND r.workspace_id = w.id AND r.status = 'validated')",
    ).bind(workspace_id).bind(user_id).bind(revision_id).execute(pool).await?;
    Ok(changed.rows_affected() > 0)
}

/// Append an owned workspace conversation message.
pub async fn create_indicator_workspace_message(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, role: &str, kind: &str, content: &str, payload: &Value,
) -> Result<Option<IndicatorWorkspaceMessageRow>, DbError> {
    let row = sqlx::query(
        "INSERT INTO indicator_workspace_messages (workspace_id, role, kind, content, payload) \
         SELECT id, $3, $4, $5, $6 FROM indicator_workspaces WHERE id = $1 AND user_id = $2 \
         RETURNING id, workspace_id, role, kind, content, payload, created_at",
    ).bind(workspace_id).bind(user_id).bind(role).bind(kind).bind(content).bind(payload)
        .fetch_optional(pool).await?;
    row.as_ref().map(message_from).transpose().map_err(Into::into)
}

/// List the newest owned messages in chronological order.
pub async fn list_indicator_workspace_messages(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, limit: i64,
) -> Result<Vec<IndicatorWorkspaceMessageRow>, DbError> {
    let rows = sqlx::query(
        "SELECT m.id, m.workspace_id, m.role, m.kind, m.content, m.payload, m.created_at \
         FROM indicator_workspace_messages m JOIN indicator_workspaces w ON w.id = m.workspace_id \
         WHERE m.workspace_id = $1 AND w.user_id = $2 ORDER BY m.created_at DESC LIMIT $3",
    ).bind(workspace_id).bind(user_id).bind(limit).fetch_all(pool).await?;
    let mut messages = rows.iter().map(message_from).collect::<Result<Vec<_>, _>>()?;
    messages.reverse();
    Ok(messages)
}

/// Update compact workspace memory after a successful generation turn.
pub async fn update_indicator_workspace_memory(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, memory: &Value,
) -> Result<bool, DbError> {
    let changed = sqlx::query("UPDATE indicator_workspaces SET memory = $3, updated_at = now() WHERE id = $1 AND user_id = $2")
        .bind(workspace_id).bind(user_id).bind(memory).execute(pool).await?;
    Ok(changed.rows_affected() > 0)
}

/// Create a revision-pinned bot draft after verifying all referenced records are owned.
pub async fn create_indicator_bot_draft(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, revision_id: Uuid, strategy_id: Uuid,
    backtest_id: Option<Uuid>, mode: &str, venue: Option<&str>, risk: &Value,
) -> Result<Option<IndicatorBotDraftRow>, DbError> {
    let row = sqlx::query(
        "INSERT INTO indicator_bot_drafts (workspace_id, revision_id, strategy_id, backtest_id, mode, venue, risk) \
         SELECT w.id, $3, $4, $5, $6, $7, $8 FROM indicator_workspaces w \
         JOIN indicator_revisions r ON r.id = $3 AND r.workspace_id = w.id AND r.status = 'validated' \
         JOIN strategies s ON s.id = $4 AND s.user_id = w.user_id \
         WHERE w.id = $1 AND w.user_id = $2 \
         RETURNING id, workspace_id, revision_id, strategy_id, backtest_id, mode, venue, risk, status, approved_at, bot_id, created_at",
    ).bind(workspace_id).bind(user_id).bind(revision_id).bind(strategy_id).bind(backtest_id).bind(mode).bind(venue).bind(risk)
        .fetch_optional(pool).await?;
    row.as_ref().map(draft_from).transpose().map_err(Into::into)
}

/// Read an owned bot draft.
pub async fn get_indicator_bot_draft(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, draft_id: Uuid,
) -> Result<Option<IndicatorBotDraftRow>, DbError> {
    let row = sqlx::query(
        "SELECT d.id, d.workspace_id, d.revision_id, d.strategy_id, d.backtest_id, d.mode, d.venue, d.risk, d.status, d.approved_at, d.bot_id, d.created_at \
         FROM indicator_bot_drafts d JOIN indicator_workspaces w ON w.id = d.workspace_id \
         WHERE d.id = $1 AND d.workspace_id = $2 AND w.user_id = $3",
    ).bind(draft_id).bind(workspace_id).bind(user_id).fetch_optional(pool).await?;
    row.as_ref().map(draft_from).transpose().map_err(Into::into)
}

/// Delete an owned workspace and everything under it.
///
/// Revisions, messages, drafts and alert preferences all cascade from the
/// workspace row, so this leaves nothing orphaned. Ownership is proved first:
/// a workspace belonging to somebody else is reported as absent rather than
/// deleted.
///
/// # Errors
/// Returns [`DbError::Pool`] on query failure.
pub async fn delete_indicator_workspace(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<bool, DbError> {
    let deleted = sqlx::query("DELETE FROM indicator_workspaces WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Store one opt-in alert preference after proving the user owns its workspace.
pub async fn set_indicator_alert_preference(
    pool: &PgPool, user_id: Uuid, workspace_id: Uuid, revision_id: Uuid,
    event_name: &str, enabled: bool, channels: &Value,
) -> Result<bool, DbError> {
    let owned: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM indicator_workspaces WHERE id = $1 AND user_id = $2",
    ).bind(workspace_id).bind(user_id).fetch_optional(pool).await?;
    if owned.is_none() { return Ok(false); }
    sqlx::query(
        "INSERT INTO indicator_alert_preferences (workspace_id, revision_id, event_name, enabled, channels) \
         VALUES ($1,$2,$3,$4,$5) ON CONFLICT (workspace_id, revision_id, event_name) \
         DO UPDATE SET enabled = EXCLUDED.enabled, channels = EXCLUDED.channels",
    ).bind(workspace_id).bind(revision_id).bind(event_name).bind(enabled).bind(channels)
        .execute(pool).await?;
    Ok(true)
}

/// List all alert preferences for an owned workspace.
pub async fn list_indicator_alert_preferences(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
    limit: i64,
) -> Result<Vec<IndicatorAlertPreference>, DbError> {
    let rows = sqlx::query(
        "SELECT ap.event_name, ap.enabled, ap.channels \
         FROM indicator_alert_preferences ap \
         JOIN indicator_workspaces w ON w.id = ap.workspace_id \
         WHERE ap.workspace_id = $1 AND w.user_id = $2 \
         ORDER BY ap.event_name ASC LIMIT $3",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(IndicatorAlertPreference {
                event_name: row.try_get("event_name")?,
                enabled: row.try_get("enabled")?,
                channels: row.try_get("channels")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// List all bot drafts for an owned workspace.
pub async fn list_indicator_bot_drafts(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
    limit: i64,
) -> Result<Vec<IndicatorBotDraftRow>, DbError> {
    let rows = sqlx::query(
        "SELECT d.id, d.workspace_id, d.revision_id, d.strategy_id, d.backtest_id, d.mode, d.venue, d.risk, d.status, d.approved_at, d.bot_id, d.created_at \
         FROM indicator_bot_drafts d \
         JOIN indicator_workspaces w ON w.id = d.workspace_id \
         WHERE d.workspace_id = $1 AND w.user_id = $2 \
         ORDER BY d.created_at DESC LIMIT $3",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(draft_from).collect::<Result<_, _>>().map_err(Into::into)
}
