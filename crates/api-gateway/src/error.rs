//! Errors returned to HTTP clients.
//!
//! The mapping from [`AgentError`] to a status code is the interesting part.
//! Most agent failures are *not* server errors: a model that never produced a
//! thesis, or produced one whose numbers no tool reported, is a 502 (the
//! upstream did not hold up its end), while missing market data is a 404 and a
//! missing skill is a 404 too -- the client asked for something that is not
//! there and can retry differently.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use ai_agent::AgentError;

/// An error rendered as a problem response.
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// Build from an explicit status and message.
    #[must_use]
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// 404 with a message.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// 503: the capability is not configured in this deployment.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// 501: designed but deliberately not built yet.
    #[must_use]
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_IMPLEMENTED, message)
    }
}

impl From<AgentError> for ApiError {
    fn from(err: AgentError) -> Self {
        let status = match &err {
            AgentError::NoData { .. } | AgentError::NoMatchingSkill(_) => StatusCode::NOT_FOUND,
            AgentError::NotConfigured(_) => StatusCode::SERVICE_UNAVAILABLE,
            // The conversation was healthy; the model just did not deliver a
            // thesis, or delivered one that contradicts the data it was shown.
            // Both are upstream failures, not bugs in this server.
            AgentError::NoThesis { .. } | AgentError::Ungrounded(_) => StatusCode::BAD_GATEWAY,
            AgentError::InvalidToolArgs { .. } | AgentError::InvalidSkill(_) => {
                StatusCode::BAD_GATEWAY
            }
            AgentError::InvalidStrategyDocument(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(status = %self.status, "request failed: {}", self.message);
        (
            self.status,
            Json(json!({ "error": self.message, "status": self.status.as_u16() })),
        )
            .into_response()
    }
}
