//! Chat history: the agent transcript, persisted per user.
//!
//! See migration `0014_chat_history.sql` for the schema's reasoning. In short:
//! a conversation is a row the user owns; a turn is one question plus the
//! ending it reached (an `answer` document or an `error` text), appended in
//! `position` order. Everything takes a `user_id` -- a conversation that is
//! not the caller's reports as absent, never as forbidden, matching the rest
//! of this crate.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::dt_to_ns;

/// Cap on how many conversations one listing reads.
///
/// Chats are cheap rows, but a history list is a UI surface: nobody scrolls
/// ten thousand of them. The cap keeps the listing bounded, not because the
/// query is expensive.
pub const MAX_CONVERSATIONS: i64 = 200;

/// Longest a derived title may be. The first question fills it; a question
/// pasted as a whole paragraph would otherwise make the history list
/// unreadable.
pub const MAX_TITLE_CHARS: usize = 80;

/// One row of the history list.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSummary {
    /// The conversation id.
    pub id: Uuid,
    /// Derived from the first question; empty until that first turn lands.
    pub title: String,
    /// When the last turn was appended (or the shell was created), unix nanos.
    pub updated_at: i64,
    /// How many turns the transcript holds.
    pub turn_count: i64,
}

/// A stored turn: one question and the ending it reached.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    /// The row id.
    pub id: Uuid,
    /// Transcript order within the conversation.
    pub position: i32,
    /// What the user asked.
    pub question: String,
    /// Attachment names (`{images: [...], document: ...}`); names only, never
    /// bytes. NULL when the question carried none.
    pub attachments: Option<Value>,
    /// The thesis payload, when the run answered. NULL when it errored.
    pub answer: Option<Value>,
    /// The refusal/failure text, when the run errored. NULL on success.
    pub error: Option<String>,
    /// The chart symbol the question was asked on, when the client recorded it.
    pub symbol: Option<String>,
    /// When the turn was appended, unix nanos.
    pub created_at: i64,
}

/// What the client knows about a finished turn.
#[derive(Debug, Clone)]
pub struct NewTurn {
    /// What the user asked.
    pub question: String,
    /// Attachment names, if any rode the question.
    pub attachments: Option<Value>,
    /// The answer payload, if the run produced one.
    pub answer: Option<Value>,
    /// The refusal/failure text, if the run ended in one.
    pub error: Option<String>,
    /// The chart symbol the question was asked on.
    pub symbol: Option<String>,
}

fn summary_from_row(row: &sqlx::postgres::PgRow) -> Result<ConversationSummary, sqlx::Error> {
    Ok(ConversationSummary {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        updated_at: dt_to_ns(row.try_get("updated_at")?),
        turn_count: row.try_get("turn_count")?,
    })
}

fn turn_from_row(row: &sqlx::postgres::PgRow) -> Result<TurnRow, sqlx::Error> {
    Ok(TurnRow {
        id: row.try_get("id")?,
        position: row.try_get("position")?,
        question: row.try_get("question")?,
        attachments: row.try_get("attachments")?,
        answer: row.try_get("answer")?,
        error: row.try_get("error")?,
        symbol: row.try_get("symbol")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
    })
}

/// Derive the conversation title from its first question.
///
/// Whitespace collapses (a pasted multi-line question is one line in the
/// list), and the cut is on a char boundary so no grapheme is split.
fn derive_title(question: &str) -> String {
    let one_line = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_TITLE_CHARS {
        return one_line;
    }
    let mut title: String = one_line.chars().take(MAX_TITLE_CHARS - 1).collect();
    title.push('…');
    title
}

/// Open a new, empty conversation. The title arrives with the first turn.
///
/// # Errors
/// Returns [`DbError::Pool`] if the insert fails.
pub async fn create_conversation(pool: &PgPool, user_id: Uuid) -> Result<Uuid, DbError> {
    let row = sqlx::query(
        "INSERT INTO chat_conversations (user_id) VALUES ($1) RETURNING id",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get("id")?)
}

/// This user's conversations, most recently touched first, with turn counts.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_conversations(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<ConversationSummary>, DbError> {
    let rows = sqlx::query(
        "SELECT c.id, c.title, c.updated_at, COUNT(t.id) AS turn_count \
         FROM chat_conversations c \
         LEFT JOIN chat_turns t ON t.conversation_id = c.id \
         WHERE c.user_id = $1 \
         GROUP BY c.id \
         ORDER BY c.updated_at DESC \
         LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(summary_from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Whether this conversation belongs to the user -- the ownership check every
/// member route runs before reading or writing turns.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn conversation_owned(
    pool: &PgPool,
    user_id: Uuid,
    conversation_id: Uuid,
) -> Result<bool, DbError> {
    let row = sqlx::query(
        "SELECT 1 AS one FROM chat_conversations WHERE id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// One conversation by id, scoped to its owner. `None` covers both "does not
/// exist" and "is not yours" -- the route turns both into the same 404.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn get_conversation(
    pool: &PgPool,
    user_id: Uuid,
    conversation_id: Uuid,
) -> Result<Option<ConversationSummary>, DbError> {
    let row = sqlx::query(
        "SELECT c.id, c.title, c.updated_at, COUNT(t.id) AS turn_count \
         FROM chat_conversations c \
         LEFT JOIN chat_turns t ON t.conversation_id = c.id \
         WHERE c.id = $1 AND c.user_id = $2 \
         GROUP BY c.id",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| summary_from_row(&r))
        .transpose()
        .map_err(Into::into)
}

/// The conversation's turns in transcript order.
///
/// Ownership is the caller's job ([`conversation_owned`]): this reads by
/// conversation id alone, so the two compose into one scoped read.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_turns(pool: &PgPool, conversation_id: Uuid) -> Result<Vec<TurnRow>, DbError> {
    let rows = sqlx::query(
        "SELECT id, position, question, attachments, answer, error, symbol, created_at \
         FROM chat_turns WHERE conversation_id = $1 ORDER BY position ASC",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(turn_from_row)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Append a finished turn, and maintain the conversation around it.
///
/// The position is `max(position) + 1` inside one transaction, so two saves
/// cannot collide on the unique index. The first turn derives the title from
/// its question (only while the title is still empty), and every append
/// touches `updated_at`, which is what the history list sorts by.
///
/// Ownership must be checked first ([`conversation_owned`]); the transaction
/// alone does not scope by user.
///
/// # Errors
/// Returns [`DbError::Pool`] if any statement fails; the transaction rolls
/// back whole, so a failed insert never leaves a title-less, untouched
/// conversation with a half-written turn.
pub async fn append_turn(
    pool: &PgPool,
    conversation_id: Uuid,
    turn: &NewTurn,
) -> Result<TurnRow, DbError> {
    let mut tx = pool.begin().await?;

    let title = derive_title(&turn.question);
    let row = sqlx::query(
        "WITH next_pos AS ( \
           SELECT COALESCE(MAX(position), -1) + 1 AS pos \
           FROM chat_turns WHERE conversation_id = $1 \
         ) \
         INSERT INTO chat_turns (conversation_id, position, question, attachments, answer, error, symbol) \
         SELECT $1, pos, $2, $3, $4, $5, $6 FROM next_pos \
         RETURNING id, position, question, attachments, answer, error, symbol, created_at",
    )
    .bind(conversation_id)
    .bind(&turn.question)
    .bind(&turn.attachments)
    .bind(&turn.answer)
    .bind(&turn.error)
    .bind(&turn.symbol)
    .fetch_one(&mut *tx)
    .await?;
    let stored = turn_from_row(&row)?;

    // The title is filled exactly once: the first turn names the
    // conversation, and later questions do not rename it under the user.
    sqlx::query(
        "UPDATE chat_conversations \
         SET updated_at = now(), \
             title = CASE WHEN title = '' THEN $2 ELSE title END \
         WHERE id = $1",
    )
    .bind(conversation_id)
    .bind(&title)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(stored)
}

/// Delete a conversation and its turns (the schema cascades).
///
/// Returns whether a row was actually removed, so the route can tell 204
/// from 404. Scoped by owner: someone else's conversation is "not found".
///
/// # Errors
/// Returns [`DbError::Pool`] if the delete fails.
pub async fn delete_conversation(
    pool: &PgPool,
    user_id: Uuid,
    conversation_id: Uuid,
) -> Result<bool, DbError> {
    let result = sqlx::query(
        "DELETE FROM chat_conversations WHERE id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
