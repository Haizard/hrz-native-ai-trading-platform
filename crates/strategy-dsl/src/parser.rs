//! YAML/JSON to a typed [`StrategyDocument`].
//!
//! YAML is what humans write; JSON is what the AI agent emits. Both go through
//! the same typed schema, so a document means exactly the same thing whichever
//! way it arrives.
//!
//! Parsing is deliberately separate from validation. A parse failure means "this
//! is not a strategy document"; a validation failure means "this is a strategy
//! document that cannot be traded". The two produce different advice for the
//! producer, so they stay distinct.

use crate::error::DslError;
use crate::schema::StrategyDocument;
use crate::validator::{Limits, ValidatedStrategy};

/// Largest document accepted, in bytes.
///
/// A strategy document is a few kilobytes at most. Anything larger is either a
/// mistake or an attempt to exhaust the validator, and the sandbox resource
/// limits in `docs/08-SANDBOX-WASM.md` apply to inputs too.
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;

/// Parse a YAML document.
pub fn from_yaml(source: &str) -> Result<StrategyDocument, DslError> {
    check_size(source)?;
    serde_yaml::from_str(source).map_err(|e| DslError::Parse(e.to_string()))
}

/// Parse a JSON document.
pub fn from_json(source: &str) -> Result<StrategyDocument, DslError> {
    check_size(source)?;
    serde_json::from_str(source).map_err(|e| DslError::Parse(e.to_string()))
}

/// Parse either format, choosing by the first non-whitespace character.
///
/// JSON is valid YAML, so a sniff is not strictly necessary -- but feeding JSON
/// to the YAML parser produces YAML-flavoured error messages, and the agent's
/// retry loop reads those messages.
pub fn parse(source: &str) -> Result<StrategyDocument, DslError> {
    check_size(source)?;
    if source.trim_start().starts_with('{') {
        from_json(source)
    } else {
        from_yaml(source)
    }
}

/// Parse and validate in one step, returning a document that is safe to execute.
pub fn parse_and_validate(source: &str) -> Result<ValidatedStrategy, DslError> {
    parse_and_validate_with(source, &Limits::default())
}

/// Parse and validate with explicit limits.
pub fn parse_and_validate_with(
    source: &str,
    limits: &Limits,
) -> Result<ValidatedStrategy, DslError> {
    let document = parse(source)?;
    ValidatedStrategy::new(document, limits)
}

fn check_size(source: &str) -> Result<(), DslError> {
    if source.len() > MAX_DOCUMENT_BYTES {
        return Err(DslError::TooLarge(format!(
            "{} bytes exceeds the {MAX_DOCUMENT_BYTES} byte limit",
            source.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
name: "Test"
version: "1.0"
kind: indicator
market: BTCUSDT
timeframes:
  entry: 5m
"#;

    #[test]
    fn parses_yaml() {
        let doc = from_yaml(MINIMAL).unwrap();
        assert_eq!(doc.name, "Test");
        assert_eq!(doc.market, "BTCUSDT");
    }

    #[test]
    fn parses_json() {
        let json = r#"{
            "name": "Test",
            "version": "1.0",
            "kind": "indicator",
            "market": "BTCUSDT",
            "timeframes": {"entry": "5m"}
        }"#;
        let doc = from_json(json).unwrap();
        assert_eq!(doc.name, "Test");
    }

    #[test]
    fn sniffing_picks_the_right_format() {
        assert!(parse(MINIMAL).is_ok());
        assert!(parse(
            r#"{"name":"x","version":"1","kind":"indicator","market":"B","timeframes":{"e":"5m"}}"#
        )
        .is_ok());
    }

    #[test]
    fn malformed_input_is_a_parse_error_not_a_panic() {
        assert!(matches!(
            from_yaml("name: [unclosed"),
            Err(DslError::Parse(_))
        ));
    }

    #[test]
    fn an_oversized_document_is_refused() {
        let huge = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(matches!(from_yaml(&huge), Err(DslError::TooLarge(_))));
    }

    #[test]
    fn parse_and_validate_rejects_an_incomplete_document() {
        let yaml = r#"
name: "Test"
version: "1.0"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
"#;
        assert!(matches!(
            parse_and_validate(yaml),
            Err(DslError::Validation { .. })
        ));
    }
}
