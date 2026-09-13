//! # `strategy-dsl`
//!
//! **One** declarative representation of trading logic (principle #5),
//! producible by natural language via the AI agent, by the visual builder, or
//! by a developer -- and executable unchanged as a chart indicator, a backtest,
//! a paper bot or a live bot.
//!
//! ## The pipeline
//!
//! ```text
//!   YAML / JSON
//!        |  parser::parse           -- typed, deny_unknown_fields
//!        v
//!   StrategyDocument
//!        |  validator::validate     -- semantic checks, field-level issues
//!        v
//!   ValidatedStrategy                -- a *type*, not a flag
//!        |
//!        v
//!   strategy-runtime                 -- accepts nothing else
//! ```
//!
//! Validation is a hard gate: **nothing reaches the sandbox or the runtime
//! without passing it**, and a failure returns the specific field-level error
//! to whoever produced the document (including the AI agent's retry loop) --
//! it is never silently patched.
//!
//! That gate is enforced by the type system rather than by discipline.
//! [`ValidatedStrategy`] has no public constructor other than the validating
//! ones, and `strategy-runtime` takes `&ValidatedStrategy` instead of a
//! [`StrategyDocument`]. A caller who forgets to validate does not get a
//! runtime surprise; they get a compile error.
//!
//! ## Modules
//!
//! * [`schema`] -- serde structs with `deny_unknown_fields` so hallucinated
//!   fields fail fast.
//! * [`expr`] -- the deliberately tiny condition grammar: tokenizer,
//!   recursive-descent parser and static type checker.
//! * [`parser`] -- YAML/JSON to a typed [`StrategyDocument`].
//! * [`validator`] -- semantic checks (declared timeframes, known condition
//!   vocabulary, risk ceiling, non-empty invalidation).
//!
//! ## Quick start
//!
//! ```
//! use strategy_dsl::parse_and_validate;
//!
//! let yaml = r#"
//! name: "Simple breakout"
//! version: "1.0"
//! kind: strategy
//! market: BTCUSDT
//! timeframes:
//!   entry: 5m
//! entry:
//!   all_of:
//!     - timeframe: entry
//!       condition: close_above(vah)
//! risk:
//!   max_risk_pct: 1.0
//!   stop: below_swing_low
//! invalidation:
//!   - timeframe: entry
//!     condition: close_below(vwap)
//! "#;
//!
//! let strategy = parse_and_validate(yaml).expect("valid");
//! assert_eq!(strategy.document().market, "BTCUSDT");
//! ```

#![deny(missing_docs)]

pub mod error;
pub mod expr;
pub mod parser;
pub mod schema;
pub mod validator;

pub use error::{DslError, ValidationIssue};

pub use schema::{
    indexed_path, Conditional, CreatedBy, Direction, DocumentKind, EntryBlock, ExitBlock, Metadata,
    RiskBlock, StopSpec, StrategyDocument, TakeProfit, TakeProfitKind,
};

pub use expr::{CompareOp, Expr, ExprError, Field, Func, Type, Value, ALL_FIELDS, ALL_FUNCS};

#[cfg(feature = "yaml")]
pub use parser::from_yaml;
pub use parser::{
    from_json, parse, parse_and_validate, parse_and_validate_with, MAX_DOCUMENT_BYTES,
};

pub use validator::{
    resolved_direction, validate, validate_with, Limits, ValidatedStrategy, MAX_RISK_PCT_CEILING,
};

/// The types most callers need.
///
/// `use strategy_dsl::prelude::*;` is the intended way to consume this crate
/// from `strategy-runtime`, `backtester` and `api-gateway`.
pub mod prelude {
    pub use crate::error::{DslError, ValidationIssue};
    pub use crate::expr::{Expr, Field, Func, Type, Value};
    pub use crate::parser::{parse, parse_and_validate, parse_and_validate_with};
    pub use crate::schema::{Conditional, Direction, DocumentKind, StopSpec, StrategyDocument};
    pub use crate::validator::{
        resolved_direction, validate, validate_with, Limits, ValidatedStrategy,
    };
}

// These exercise the document end to end through its YAML form, which is how a
// human writes one. The guest build has no YAML parser, so they are gated with
// the feature rather than rewritten into JSON -- the JSON path is covered by
// `parser::tests` and by `sandbox-guest`.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name: "Liquidity Sweep + Absorption"
version: "2.1"
kind: strategy
market: "BTCUSDT"
timeframes:
  trend: "4h"
  setup: "1h"
  entry: "5m"
entry:
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: setup
      condition: absorption.detected == true
    - timeframe: entry
      condition: liquidity.swept == "sell_side"
    - timeframe: entry
      condition: delta > threshold(1500)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.5
invalidation:
  - timeframe: entry
    condition: "close_below(stop_price)"
metadata:
  created_by: "ai_agent"
  skill_ref: "liquidity-sweep-absorption-v2"
"#;

    #[test]
    fn the_public_pipeline_works_end_to_end() {
        let strategy = parse_and_validate(SAMPLE).expect("the spec's sample must be valid");
        let doc = strategy.document();

        assert_eq!(doc.name, "Liquidity Sweep + Absorption");
        assert_eq!(doc.kind, DocumentKind::Strategy);
        assert_eq!(doc.direction(), Some(Direction::Long));
        assert_eq!(doc.decision_timeframe().map(|(n, _)| n), Some("entry"));
        assert_eq!(
            doc.metadata.skill_ref.as_deref(),
            Some("liquidity-sweep-absorption-v2")
        );
    }

    #[test]
    fn a_validated_document_round_trips_byte_stable() {
        // `timeframes` is a BTreeMap precisely so this holds; a HashMap would
        // reorder keys and make the document non-deterministic to hash or diff.
        let strategy = parse_and_validate(SAMPLE).unwrap();
        let first = serde_yaml::to_string(strategy.document()).unwrap();
        let second = serde_yaml::to_string(strategy.document()).unwrap();
        assert_eq!(first, second);

        let reparsed = parse_and_validate(&first).expect("re-serialized document must re-validate");
        assert_eq!(reparsed.document(), strategy.document());
    }

    #[test]
    fn an_invalid_document_is_refused_with_field_level_detail() {
        let yaml = SAMPLE.replace("max_risk_pct: 1.0", "max_risk_pct: 80.0");
        let err = parse_and_validate(&yaml).unwrap_err();
        match err {
            DslError::Validation { issues } => {
                assert!(issues.iter().any(|i| i.path == "risk.max_risk_pct"));
            }
            other => panic!("expected a validation error, got {other}"),
        }
    }

    #[test]
    fn the_prelude_is_self_sufficient() {
        use crate::prelude::*;

        let doc = crate::parser::from_yaml(SAMPLE).unwrap();
        let validated: ValidatedStrategy = ValidatedStrategy::with_defaults(doc).unwrap();
        assert_eq!(
            resolved_direction(validated.document()),
            Some(Direction::Long)
        );
        assert!(Limits::default().max_risk_pct <= crate::MAX_RISK_PCT_CEILING);
    }
}
