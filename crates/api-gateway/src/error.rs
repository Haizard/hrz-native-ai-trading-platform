//! Errors returned to HTTP clients (`docs/12-API-GATEWAY.md`).
//!
//! ## The envelope
//!
//! `docs/12` fixes the shape:
//!
//! ```json
//! { "error": { "code": "STRATEGY_VALIDATION_FAILED", "message": "...", "details": {...} } }
//! ```
//!
//! The `code` is the part a client branches on; `message` is for a human. This
//! used to be a flat `{"error": "...", "status": 422}`, which forced the
//! frontend to match on prose.
//!
//! ## Validation detail is carried, not flattened
//!
//! `docs/12` is specific about this: "Validation errors from `strategy-dsl` are
//! surfaced with their specific field-level detail, not flattened to a generic
//! message — the frontend strategy editor and the AI agent's retry loop both
//! depend on this detail." So a [`DslError::Validation`] becomes a `details`
//! array of `{path, message}` pairs: what the editor underlines, and what the
//! agent's repair loop feeds back to the model.
//!
//! ## Status codes are chosen, not defaulted
//!
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
use strategy_dsl::DslError;

/// An error rendered as a problem response.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
    details: Option<serde_json::Value>,
}

impl ApiError {
    /// Build from an explicit status and message.
    ///
    /// The code is derived from the status. Prefer a constructor that names the
    /// failure -- a client can only branch on a code it can predict.
    #[must_use]
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let code = match status {
            StatusCode::BAD_REQUEST => "BAD_REQUEST",
            StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
            StatusCode::FORBIDDEN => "FORBIDDEN",
            StatusCode::NOT_FOUND => "NOT_FOUND",
            StatusCode::CONFLICT => "CONFLICT",
            StatusCode::UNPROCESSABLE_ENTITY => "UNPROCESSABLE_ENTITY",
            StatusCode::NOT_IMPLEMENTED => "NOT_IMPLEMENTED",
            StatusCode::BAD_GATEWAY => "UPSTREAM_FAILED",
            StatusCode::SERVICE_UNAVAILABLE => "UNAVAILABLE",
            _ => "INTERNAL_ERROR",
        };
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }

    /// Build with an explicit code.
    #[must_use]
    pub fn coded(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured detail.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// 400 with a code.
    #[must_use]
    pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::coded(StatusCode::BAD_REQUEST, code, message)
    }

    /// 401: the caller is not authenticated.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::coded(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", message)
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

    /// 500: this server is broken.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    /// The status, for tests and for the log line.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The machine-readable code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }
}

/// Turn a strategy-document failure into a response the editor can act on.
///
/// A parse failure and a validation failure are different codes because they
/// need different fixes: one is "this is not YAML", the other is "this field is
/// wrong". Both carry detail.
impl From<DslError> for ApiError {
    fn from(err: DslError) -> Self {
        match err {
            DslError::Validation { issues } => {
                let detail: Vec<_> = issues
                    .iter()
                    .map(|issue| json!({ "path": issue.path, "message": issue.message }))
                    .collect();
                Self::coded(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "STRATEGY_VALIDATION_FAILED",
                    format!("the strategy document has {} problem(s)", detail.len()),
                )
                .with_details(json!({ "issues": detail }))
            }
            DslError::Parse(message) => Self::coded(
                StatusCode::BAD_REQUEST,
                "STRATEGY_PARSE_FAILED",
                format!("the strategy document could not be parsed: {message}"),
            ),
            DslError::TooLarge(message) => {
                Self::coded(StatusCode::PAYLOAD_TOO_LARGE, "STRATEGY_TOO_LARGE", message)
            }
        }
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
        let code = match &err {
            AgentError::NoData { .. } => "NO_MARKET_DATA",
            AgentError::NoMatchingSkill(_) => "NO_MATCHING_SKILL",
            AgentError::NotConfigured(_) => "AGENT_NOT_CONFIGURED",
            AgentError::NoThesis { .. } => "AGENT_NO_THESIS",
            AgentError::Ungrounded(_) => "AGENT_UNGROUNDED_THESIS",
            AgentError::InvalidToolArgs { .. } => "AGENT_BAD_TOOL_ARGS",
            AgentError::InvalidSkill(_) => "AGENT_INVALID_SKILL",
            AgentError::InvalidStrategyDocument(_) => "STRATEGY_VALIDATION_FAILED",
            _ => "AGENT_FAILED",
        };
        Self::coded(status, code, err.to_string())
    }
}

impl From<db::DbError> for ApiError {
    fn from(err: db::DbError) -> Self {
        // A database failure is ours, not the caller's. The message names the
        // problem for the operator; the client gets a code it can show as
        // "try again" without learning the schema.
        Self::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DATABASE_ERROR",
            err.to_string(),
        )
    }
}

impl From<backtester::BacktestError> for ApiError {
    fn from(err: backtester::BacktestError) -> Self {
        // A backtest that cannot run is almost always a data problem: the window
        // has no candles, or a declared timeframe was not loaded. That is the
        // caller's to fix by choosing another window.
        Self::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BACKTEST_FAILED",
            err.to_string(),
        )
    }
}

impl From<trading_engine::ExecutionError> for ApiError {
    fn from(err: trading_engine::ExecutionError) -> Self {
        Self::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "EXECUTION_FAILED",
            err.to_string(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 5xx is our fault and is logged at warn; a 4xx is the caller's and is
        // logged at debug, so a noisy client cannot fill the log.
        if self.status.is_server_error() {
            tracing::warn!(status = %self.status, code = %self.code, "request failed: {}", self.message);
        } else {
            tracing::debug!(status = %self.status, code = %self.code, "request rejected: {}", self.message);
        }

        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(details) = self.details {
            error["details"] = details;
        }

        (self.status, Json(json!({ "error": error }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strategy_dsl::ValidationIssue;

    /// Render an error the way a client would see it.
    fn body(err: ApiError) -> serde_json::Value {
        let response = err.into_response();
        let status = response.status();
        let bytes = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(axum::body::to_bytes(response.into_body(), 64 * 1024))
            .expect("the body must be readable");
        let payload: serde_json::Value =
            serde_json::from_slice(&bytes).expect("the error body must be json");
        assert!(
            payload["error"]["code"].is_string(),
            "every error carries a code; status was {status}"
        );
        payload
    }

    #[test]
    fn the_envelope_matches_the_documented_contract() {
        let payload = body(ApiError::not_found("no such bot"));
        assert_eq!(payload["error"]["code"], "NOT_FOUND");
        assert_eq!(payload["error"]["message"], "no such bot");
        // The old flat shape had `error` as a string; a client written against
        // the contract reads `error.code`.
        assert!(payload["error"].is_object());
        assert!(payload.get("status").is_none());
    }

    #[test]
    fn validation_detail_survives_as_field_paths() {
        let err = ApiError::from(DslError::Validation {
            issues: vec![
                ValidationIssue::new("entry.all_of[2]", "unknown field `value`"),
                ValidationIssue::new("timeframes.entry", "must be 5m"),
            ],
        });
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code(), "STRATEGY_VALIDATION_FAILED");

        let payload = body(err);
        let issues = payload["error"]["details"]["issues"]
            .as_array()
            .expect("the issues array must survive");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0]["path"], "entry.all_of[2]");
        assert_eq!(issues[0]["message"], "unknown field `value`");
        assert_eq!(issues[1]["path"], "timeframes.entry");
    }

    #[test]
    fn a_parse_failure_is_a_different_code_from_a_validation_failure() {
        // They need different fixes, so they must be distinguishable.
        let parse = ApiError::from(DslError::Parse("expected a mapping".into()));
        assert_eq!(parse.status(), StatusCode::BAD_REQUEST);
        assert_eq!(parse.code(), "STRATEGY_PARSE_FAILED");
    }

    #[test]
    fn an_unconfigured_agent_is_503_not_500() {
        let err = ApiError::from(AgentError::NotConfigured("no credentials".into()));
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "AGENT_NOT_CONFIGURED");
    }

    #[test]
    fn a_missing_skill_is_404_and_an_ungrounded_thesis_is_502() {
        let missing = ApiError::from(AgentError::NoMatchingSkill("nope".into()));
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let ungrounded = ApiError::from(AgentError::Ungrounded("invented a price".into()));
        assert_eq!(ungrounded.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(ungrounded.code(), "AGENT_UNGROUNDED_THESIS");
    }

    #[test]
    fn a_database_failure_does_not_leak_the_schema_to_the_client() {
        let err = ApiError::from(db::DbError::MissingEnv("DATABASE_URL".into()));
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code(), "DATABASE_ERROR");
    }
}
