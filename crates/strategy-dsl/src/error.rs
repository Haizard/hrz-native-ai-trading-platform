//! Errors returned by `strategy-dsl`.
//!
//! Validation errors are surfaced with field-level detail on purpose: both the
//! frontend strategy editor and the AI agent's retry loop depend on being told
//! *which* field is wrong, not just "invalid document"
//! (`docs/06-STRATEGY-DSL.md`, `docs/12-API-GATEWAY.md`).

use thiserror::Error;

/// A single field-level validation problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// JSON/YAML path to the offending field, e.g. `entry.all_of[2].timeframe`.
    pub path: String,
    /// Human-readable description of the problem.
    pub message: String,
}

impl ValidationIssue {
    /// Convenience constructor.
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Errors produced while parsing or validating a strategy document.
#[derive(Debug, Error)]
pub enum DslError {
    /// The document is not valid YAML/JSON.
    #[error("parse error: {0}")]
    Parse(String),

    /// The document parsed but violates one or more semantic rules.
    #[error("strategy validation failed with {} issue(s): {}", .issues.len(), format_issues(.issues))]
    Validation {
        /// Every issue found, not just the first.
        issues: Vec<ValidationIssue>,
    },

    /// The document exceeds a configured size/complexity limit.
    #[error("strategy document too large: {0}")]
    TooLarge(String),
}

/// Render issues as `path: message; path: message`.
fn format_issues(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|i| format!("{}: {}", i.path, i.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_error_lists_every_issue() {
        let err = DslError::Validation {
            issues: vec![
                ValidationIssue::new("entry.all_of[0].timeframe", "undeclared timeframe `2h`"),
                ValidationIssue::new("risk.max_risk_pct", "must be <= 5.0"),
            ],
        };
        let msg = err.to_string();
        assert!(msg.contains("2h"));
        assert!(msg.contains("must be <= 5.0"));
    }
}
