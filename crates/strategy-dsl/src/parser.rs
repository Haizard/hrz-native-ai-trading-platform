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
///
/// Absent when the `yaml` feature is off, which is how the sandbox guest is
/// built: the only encoding accepted inside the sandbox is JSON.
#[cfg(feature = "yaml")]
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
///
/// Without the `yaml` feature a document that is not JSON is refused with an
/// explicit reason rather than being reported as malformed JSON, which would
/// send whoever produced it looking in the wrong place.
pub fn parse(source: &str) -> Result<StrategyDocument, DslError> {
    check_size(source)?;
    if source.trim_start().starts_with('{') {
        return from_json(source);
    }
    #[cfg(feature = "yaml")]
    {
        from_yaml(source)
    }
    #[cfg(not(feature = "yaml"))]
    {
        Err(DslError::Parse(
            "this build has no YAML support: a strategy document must be JSON".into(),
        ))
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

    const MINIMAL_JSON: &str = r#"{
        "name": "Test",
        "version": "1.0",
        "kind": "indicator",
        "market": "BTCUSDT",
        "timeframes": {"entry": "5m"}
    }"#;

    #[cfg(feature = "yaml")]
    #[test]
    fn parses_yaml() {
        let doc = from_yaml(MINIMAL).unwrap();
        assert_eq!(doc.name, "Test");
        assert_eq!(doc.market, "BTCUSDT");
    }

    #[test]
    fn parses_json() {
        let doc = from_json(MINIMAL_JSON).unwrap();
        assert_eq!(doc.name, "Test");
    }

    #[test]
    fn sniffing_picks_the_right_format() {
        assert!(parse(MINIMAL_JSON).is_ok());
        #[cfg(feature = "yaml")]
        assert!(parse(MINIMAL).is_ok());
    }

    /// The guest build links no YAML parser, so a YAML document must be refused
    /// with a reason that names the real problem. Reporting it as malformed JSON
    /// would send the document's producer hunting for a syntax error that is not
    /// there.
    #[cfg(not(feature = "yaml"))]
    #[test]
    fn without_the_yaml_feature_yaml_is_refused_by_name() {
        let err = parse(MINIMAL).unwrap_err();
        match err {
            DslError::Parse(message) => assert!(message.contains("no YAML support")),
            other => panic!("expected a parse error, got {other}"),
        }
    }

    #[test]
    fn malformed_input_is_a_parse_error_not_a_panic() {
        assert!(matches!(
            from_json(r#"{"name": [unclosed"#),
            Err(DslError::Parse(_))
        ));
        #[cfg(feature = "yaml")]
        assert!(matches!(
            from_yaml("name: [unclosed"),
            Err(DslError::Parse(_))
        ));
    }

    #[test]
    fn an_oversized_document_is_refused() {
        let huge = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(matches!(from_json(&huge), Err(DslError::TooLarge(_))));
    }

    #[test]
    fn parse_and_validate_rejects_an_incomplete_document() {
        let json = r#"{
            "name": "Test",
            "version": "1.0",
            "kind": "strategy",
            "market": "BTCUSDT",
            "timeframes": {"entry": "5m"}
        }"#;
        assert!(matches!(
            parse_and_validate(json),
            Err(DslError::Validation { .. })
        ));
    }
}
