//! `/chats` -- the agent panel's conversation history.
//!
//! The transcript used to live in one browser array: a reload ended it, and
//! the machine that asked was the only one that remembered. These routes make
//! it a per-user record (`docs/13` shape: the payload is a document, the
//! ordering columns are real columns). The shell saves a turn when it
//! *finishes* -- an answer or a refusal -- so a reload mid-run simply loses
//! the in-flight question, which is the honest outcome.
//!
//! Everything is owner-scoped: a conversation that is not the caller's is
//! "not found", never "forbidden", the same answer the other routes give.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// One row of the history list.
#[derive(Debug, Serialize)]
pub struct ConversationSummaryResponse {
    /// The conversation id.
    pub id: Uuid,
    /// Derived from the first question; empty until that first turn lands.
    pub title: String,
    /// When the last turn was appended, unix nanos.
    pub updated_at: i64,
    /// How many turns the transcript holds.
    pub turn_count: i64,
}

impl From<db::chats::ConversationSummary> for ConversationSummaryResponse {
    fn from(row: db::chats::ConversationSummary) -> Self {
        Self {
            id: row.id,
            title: row.title,
            updated_at: row.updated_at,
            turn_count: row.turn_count,
        }
    }
}

/// A stored turn, as the shell renders one.
#[derive(Debug, Serialize)]
pub struct TurnResponse {
    /// The row id.
    pub id: Uuid,
    /// Transcript order within the conversation.
    pub position: i32,
    /// What the user asked.
    pub question: String,
    /// Attachment names, when the question carried any.
    pub attachments: Option<serde_json::Value>,
    /// The thesis payload, when the run answered.
    pub answer: Option<serde_json::Value>,
    /// The refusal/failure text, when the run errored.
    pub error: Option<String>,
    /// The chart symbol the question was asked on.
    pub symbol: Option<String>,
    /// When the turn was appended, unix nanos.
    pub created_at: i64,
}

impl From<db::chats::TurnRow> for TurnResponse {
    fn from(row: db::chats::TurnRow) -> Self {
        Self {
            id: row.id,
            position: row.position,
            question: row.question,
            attachments: row.attachments,
            answer: row.answer,
            error: row.error,
            symbol: row.symbol,
            created_at: row.created_at,
        }
    }
}

/// `GET /chats/{id}` answer: the conversation and its whole transcript.
#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    /// The conversation id.
    pub id: Uuid,
    /// The derived title.
    pub title: String,
    /// The turns, in transcript order.
    pub turns: Vec<TurnResponse>,
}

/// What the shell reports about a finished turn.
#[derive(Debug, Deserialize)]
pub struct AppendTurnBody {
    /// What the user asked. Required -- a turn with no question is not one.
    pub question: String,
    /// Attachment names (`{images: [...], document: ...}`), never bytes.
    pub attachments: Option<serde_json::Value>,
    /// The answer payload, when the run produced one.
    pub answer: Option<serde_json::Value>,
    /// The refusal/failure text, when the run ended in one.
    pub error: Option<String>,
    /// The chart symbol the question was asked on.
    pub symbol: Option<String>,
}

/// The database, or a clear 503.
fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; chats cannot be stored"))
}

/// Ownership, or 404. A conversation that is not the caller's is absent --
/// the same "not found" every owner-scoped route gives.
async fn owned(state: &AppState, user: &UserContext, id: Uuid) -> Result<(), ApiError> {
    if !db::chats::conversation_owned(database(state)?.pool(), user.user_id, id).await? {
        return Err(ApiError::not_found("no such chat"));
    }
    Ok(())
}

/// `GET /chats` -- the caller's conversations, most recently touched first.
///
/// # Errors
/// 401 without a token, 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<ConversationSummaryResponse>>, ApiError> {
    let rows = db::chats::list_conversations(
        database(&state)?.pool(),
        user.user_id,
        db::chats::MAX_CONVERSATIONS,
    )
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `POST /chats` -- open a new, empty conversation.
///
/// The title arrives with the first turn, derived from its question; nothing
/// here asks the user to name a chat before it has content.
///
/// # Errors
/// 401 without a token, 503 without a database.
pub async fn create(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = db::chats::create_conversation(database(&state)?.pool(), user.user_id).await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

/// `GET /chats/{id}` -- one conversation and its transcript, in order.
///
/// # Errors
/// 404 when the conversation is not the caller's, 401 without a token,
/// 503 without a database.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationResponse>, ApiError> {
    owned(&state, &user, id).await?;
    let conversation = db::chats::get_conversation(database(&state)?.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such chat"))?;
    let turns = db::chats::list_turns(database(&state)?.pool(), id).await?;
    Ok(Json(ConversationResponse {
        id,
        title: conversation.title,
        turns: turns.into_iter().map(Into::into).collect(),
    }))
}

/// `POST /chats/{id}/turns` -- record a finished turn.
///
/// A finished turn has exactly one ending: an `answer` or an `error`. Both
/// would be two transcripts in one row; neither is a run still in flight,
/// which the shell does not save.
///
/// # Errors
/// 400 on an empty question or an ambiguous/unfinished ending, 404 when the
/// conversation is not the caller's, 401 without a token, 503 without a
/// database.
pub async fn append(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<AppendTurnBody>,
) -> Result<(StatusCode, Json<TurnResponse>), ApiError> {
    if body.question.trim().is_empty() {
        return Err(ApiError::bad_request(
            "CHAT_QUESTION_REQUIRED",
            "a turn needs a `question`: it is what the transcript shows",
        ));
    }
    match (body.answer.is_some(), body.error.is_some()) {
        (true, true) => {
            return Err(ApiError::bad_request(
                "CHAT_TURN_AMBIGUOUS",
                "a turn ends once: send `answer` or `error`, not both",
            ));
        }
        (false, false) => {
            return Err(ApiError::bad_request(
                "CHAT_TURN_UNFINISHED",
                "a turn with neither `answer` nor `error` is still running; save it when it ends",
            ));
        }
        _ => {}
    }
    owned(&state, &user, id).await?;
    let turn = db::chats::NewTurn {
        question: body.question,
        attachments: body.attachments,
        answer: body.answer,
        error: body.error,
        symbol: body.symbol,
    };
    let stored = db::chats::append_turn(database(&state)?.pool(), id, &turn).await?;
    Ok((StatusCode::CREATED, Json(stored.into())))
}

/// `DELETE /chats/{id}` -- remove a conversation and its turns.
///
/// # Errors
/// 404 when the conversation is not the caller's, 401 without a token,
/// 503 without a database.
pub async fn remove(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if !db::chats::delete_conversation(database(&state)?.pool(), user.user_id, id).await? {
        return Err(ApiError::not_found("no such chat"));
    }
    Ok(StatusCode::NO_CONTENT)
}
