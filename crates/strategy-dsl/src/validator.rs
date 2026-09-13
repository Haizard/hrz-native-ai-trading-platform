//! Semantic validation -- the gate between a document and execution.
//!
//! Schema shape is enforced by serde; this module enforces *meaning*. The spec
//! is unambiguous about what happens next:
//!
//! > Any document that fails validation is rejected before it ever reaches the
//! > sandbox or runtime -- the AI agent must be told the specific validation
//! > error and asked to correct it, never silently patched.
//!
//! Two mechanisms enforce that. [`ValidatedStrategy`] can only be built by
//! validating, and `strategy-runtime` accepts nothing else -- so the gate is a
//! type, not a convention someone can forget. And every failure carries a
//! field-level [`ValidationIssue`], so the agent gets `entry.all_of[2].condition:
//! unknown field \`deltas\`` rather than "invalid document".
//!
//! ## What is checked
//!
//! * Identity fields are non-empty.
//! * `timeframes` is non-empty and within the declared limit.
//! * The presence of `entry`/`risk`/`invalidation` matches `kind` -- an
//!   indicator has no trade logic, a strategy must have all three.
//! * Every condition names a declared timeframe, parses, and is boolean-valued.
//! * Every field and function is in the vocabulary from [`crate::expr`].
//! * `max_risk_pct` is positive and under a hard ceiling that a document cannot
//!   raise, no matter what it asks for.
//! * Stop and take-profit parameters are sane.
//! * `entry.direction`, when given, does not contradict the stop rule.
//! * `threshold(...)` wraps a literal, so tunable parameters stay auditable.

use std::collections::BTreeSet;

use crate::error::{DslError, ValidationIssue};
use crate::expr::{Expr, Value};
use crate::schema::{Direction, DocumentKind, StopSpec, StrategyDocument};

/// Hard ceiling on `risk.max_risk_pct`, applied regardless of the document.
///
/// This is a safety rail, not a preference: an AI-generated or imported
/// document must not be able to risk more than this per trade.
pub const MAX_RISK_PCT_CEILING: f64 = 5.0;

/// Bounds applied during validation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Largest allowed `risk.max_risk_pct`.
    pub max_risk_pct: f64,
    /// Largest allowed number of conditions across the whole document.
    pub max_conditions: usize,
    /// Longest allowed condition source, in bytes.
    pub max_expression_len: usize,
    /// Largest allowed number of declared timeframes.
    pub max_timeframes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_risk_pct: MAX_RISK_PCT_CEILING,
            max_conditions: 64,
            max_expression_len: 512,
            max_timeframes: 8,
        }
    }
}

/// A [`StrategyDocument`] that has passed validation.
///
/// The only way to obtain one is [`ValidatedStrategy::new`] or
/// [`crate::parser::parse_and_validate`]. `strategy-runtime` takes this type
/// rather than a raw document, so an unvalidated strategy is not merely
/// discouraged -- it does not typecheck.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedStrategy {
    document: StrategyDocument,
}

impl ValidatedStrategy {
    /// Validate `document` and take ownership if it passes.
    pub fn new(document: StrategyDocument, limits: &Limits) -> Result<Self, DslError> {
        validate_with(&document, limits)?;
        Ok(Self { document })
    }

    /// Validate with the default limits.
    pub fn with_defaults(document: StrategyDocument) -> Result<Self, DslError> {
        Self::new(document, &Limits::default())
    }

    /// The validated document.
    #[must_use]
    pub fn document(&self) -> &StrategyDocument {
        &self.document
    }

    /// Unwrap the document.
    #[must_use]
    pub fn into_document(self) -> StrategyDocument {
        self.document
    }
}

/// Validate with the default limits.
pub fn validate(document: &StrategyDocument) -> Result<(), DslError> {
    validate_with(document, &Limits::default())
}

/// Validate, collecting every problem rather than stopping at the first.
///
/// Reporting all issues at once matters for the agent's retry loop: fixing one
/// error per round trip turns a two-line correction into five.
pub fn validate_with(document: &StrategyDocument, limits: &Limits) -> Result<(), DslError> {
    let mut issues = Issues::default();

    check_identity(document, &mut issues);
    check_timeframes(document, limits, &mut issues);
    check_blocks_for_kind(document, &mut issues);
    check_conditions(document, limits, &mut issues);
    check_risk(document, limits, &mut issues);
    check_direction_consistency(document, &mut issues);

    issues.into_result()
}

/// Issue collector.
#[derive(Default)]
struct Issues {
    items: Vec<ValidationIssue>,
}

impl Issues {
    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.items.push(ValidationIssue::new(path, message));
    }

    fn into_result(self) -> Result<(), DslError> {
        if self.items.is_empty() {
            Ok(())
        } else {
            Err(DslError::Validation { issues: self.items })
        }
    }
}

fn check_identity(document: &StrategyDocument, issues: &mut Issues) {
    if document.name.trim().is_empty() {
        issues.push("name", "must not be empty");
    }
    if document.version.trim().is_empty() {
        issues.push("version", "must not be empty");
    }
    if document.market.trim().is_empty() {
        issues.push("market", "must not be empty");
    }
}

fn check_timeframes(document: &StrategyDocument, limits: &Limits, issues: &mut Issues) {
    if document.timeframes.is_empty() {
        issues.push("timeframes", "must declare at least one timeframe");
        return;
    }

    if document.timeframes.len() > limits.max_timeframes {
        issues.push(
            "timeframes",
            format!(
                "declares {} timeframes, the limit is {}",
                document.timeframes.len(),
                limits.max_timeframes
            ),
        );
    }

    for name in document.timeframes.keys() {
        if name.trim().is_empty() {
            issues.push("timeframes", "timeframe names must not be empty");
        } else if name.chars().any(char::is_whitespace) {
            issues.push(
                format!("timeframes.{name}"),
                "timeframe names must not contain whitespace",
            );
        }
    }
}

fn check_blocks_for_kind(document: &StrategyDocument, issues: &mut Issues) {
    let indicator = document.kind == DocumentKind::Indicator;

    if indicator {
        if document.entry.is_some() {
            issues.push("entry", "an indicator must not declare entry rules");
        }
        if document.risk.is_some() {
            issues.push("risk", "an indicator must not declare a risk block");
        }
        if !document.invalidation.is_empty() {
            issues.push(
                "invalidation",
                "an indicator must not declare invalidation rules",
            );
        }
        if document.exit.as_ref().is_some_and(|e| !e.is_empty()) {
            issues.push("exit", "an indicator must not declare exit rules");
        }
        return;
    }

    match &document.entry {
        None => issues.push("entry", "is required for a strategy or bot"),
        Some(entry) if entry.all_of.is_empty() && entry.any_of.is_empty() => issues.push(
            "entry",
            "must declare at least one condition in `all_of` or `any_of`",
        ),
        Some(_) => {}
    }

    if document.risk.is_none() {
        issues.push("risk", "is required for a strategy or bot");
    }

    if document.invalidation.is_empty() {
        issues.push(
            "invalidation",
            "must contain at least one condition -- a strategy with no invalidation can only \
             exit on its stop or target",
        );
    }
}

fn check_conditions(document: &StrategyDocument, limits: &Limits, issues: &mut Issues) {
    let conditions = document.all_conditions();

    if conditions.len() > limits.max_conditions {
        issues.push(
            "entry",
            format!(
                "document declares {} conditions, the limit is {}",
                conditions.len(),
                limits.max_conditions
            ),
        );
    }

    let declared: BTreeSet<&str> = document.timeframes.keys().map(String::as_str).collect();

    for (path, conditional) in conditions {
        let condition_path = format!("{path}.condition");

        if !declared.contains(conditional.timeframe.as_str()) {
            issues.push(
                format!("{path}.timeframe"),
                format!(
                    "undeclared timeframe `{}`; declared: {}",
                    conditional.timeframe,
                    declared.iter().copied().collect::<Vec<_>>().join(", ")
                ),
            );
        }

        if conditional.condition.trim().is_empty() {
            issues.push(&condition_path, "must not be empty");
            continue;
        }

        if conditional.condition.len() > limits.max_expression_len {
            issues.push(
                &condition_path,
                format!(
                    "{} bytes exceeds the {}-byte limit",
                    conditional.condition.len(),
                    limits.max_expression_len
                ),
            );
            continue;
        }

        match Expr::parse_checked(&conditional.condition) {
            Err(e) => issues.push(&condition_path, e.to_string()),
            Ok(expr) => check_threshold_literals(&expr, &condition_path, issues),
        }
    }
}

/// `threshold(x)` must wrap a literal.
///
/// The function exists so tunable numbers are greppable and sweepable. If it
/// wrapped an arbitrary expression, a parameter sweep would have nothing
/// concrete to vary.
fn check_threshold_literals(expr: &Expr, path: &str, issues: &mut Issues) {
    walk(expr, &mut |node| {
        if let Expr::Call { func, args } = node {
            if func.name() == "threshold" {
                match args.first() {
                    Some(Expr::Literal(Value::Num(_))) => {}
                    _ => issues.push(
                        path,
                        "`threshold(...)` must wrap a numeric literal so the parameter can be \
                         identified and swept",
                    ),
                }
            }
        }
    });
}

/// Visit every node in an expression tree.
fn walk(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    match expr {
        Expr::Call { args, .. } => {
            for arg in args {
                walk(arg, visit);
            }
        }
        Expr::Not(inner) => walk(inner, visit),
        Expr::And(a, b) | Expr::Or(a, b) => {
            walk(a, visit);
            walk(b, visit);
        }
        Expr::Compare { lhs, rhs, .. } => {
            walk(lhs, visit);
            walk(rhs, visit);
        }
        Expr::Literal(_) | Expr::Field(_) => {}
    }
}

fn check_risk(document: &StrategyDocument, limits: &Limits, issues: &mut Issues) {
    let Some(risk) = &document.risk else {
        return;
    };

    if !risk.max_risk_pct.is_finite() || risk.max_risk_pct <= 0.0 {
        issues.push("risk.max_risk_pct", "must be a positive number");
    } else if risk.max_risk_pct > limits.max_risk_pct {
        issues.push(
            "risk.max_risk_pct",
            format!(
                "{} exceeds the hard ceiling of {}%",
                risk.max_risk_pct, limits.max_risk_pct
            ),
        );
    }

    if let Some(take_profit) = &risk.take_profit {
        if !take_profit.value.is_finite() || take_profit.value <= 0.0 {
            issues.push("risk.take_profit.value", "must be a positive number");
        }
    }

    match risk.stop {
        StopSpec::BelowRecentLow { bars } | StopSpec::AboveRecentHigh { bars } => {
            if bars == 0 {
                issues.push("risk.stop.bars", "must be greater than zero");
            }
        }
        StopSpec::Atr { multiple, period } => {
            if !multiple.is_finite() || multiple <= 0.0 {
                issues.push("risk.stop.multiple", "must be a positive number");
            }
            if period == 0 {
                issues.push("risk.stop.period", "must be greater than zero");
            }
        }
        StopSpec::Fixed { price } => {
            if !price.is_finite() || price <= 0.0 {
                issues.push("risk.stop.price", "must be a positive number");
            }
        }
        StopSpec::BelowSweepLow
        | StopSpec::AboveSweepHigh
        | StopSpec::BelowSwingLow
        | StopSpec::AboveSwingHigh => {}
    }
}

fn check_direction_consistency(document: &StrategyDocument, issues: &mut Issues) {
    let (Some(entry), Some(risk)) = (&document.entry, &document.risk) else {
        return;
    };

    match entry.direction {
        Some(explicit) => {
            if risk.stop.implies_direction() && explicit != risk.stop.direction() {
                issues.push(
                    "entry.direction",
                    format!(
                        "`{explicit}` contradicts the `{}` stop, which implies `{}`",
                        risk.stop.kind_name(),
                        risk.stop.direction()
                    ),
                );
            }
        }
        None => {
            if !risk.stop.implies_direction() {
                issues.push(
                    "entry.direction",
                    format!(
                        "is required when the stop is `{}`, because that rule does not imply a \
                         side",
                        risk.stop.kind_name()
                    ),
                );
            }
        }
    }
}

/// Convenience: the direction a validated document trades.
#[must_use]
pub fn resolved_direction(document: &StrategyDocument) -> Option<Direction> {
    document.direction()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::from_yaml;

    /// The documented sample, exactly as `docs/06-STRATEGY-DSL.md` writes it.
    /// The example from `docs/06-STRATEGY-DSL.md`.
    ///
    /// Read from the file we actually ship in `strategies/` rather than copied
    /// in here, so "the sample document validates" is a claim about the artifact
    /// a user gets, not about a fixture that can quietly drift from it.
    const SAMPLE: &str = include_str!("../../../strategies/liquidity-sweep.yaml");

    fn validate_str(yaml: &str) -> Result<(), DslError> {
        validate(&from_yaml(yaml).expect("fixture must parse"))
    }

    fn issues(yaml: &str) -> Vec<ValidationIssue> {
        match validate_str(yaml) {
            Ok(()) => Vec::new(),
            Err(DslError::Validation { issues }) => issues,
            Err(other) => panic!("expected a validation error, got {other}"),
        }
    }

    fn paths(yaml: &str) -> Vec<String> {
        issues(yaml).into_iter().map(|i| i.path).collect()
    }

    #[test]
    fn the_documented_sample_validates() {
        validate_str(SAMPLE).expect("the spec's own example must be valid");
    }

    #[test]
    fn the_sample_document_is_tradable() {
        let doc = from_yaml(SAMPLE).unwrap();
        assert_eq!(doc.kind, DocumentKind::Strategy);
        assert_eq!(doc.direction(), Some(Direction::Long));
        assert_eq!(doc.decision_timeframe().unwrap().0, "entry");
    }

    #[test]
    fn validated_strategy_cannot_be_built_from_a_bad_document() {
        let mut doc = from_yaml(SAMPLE).unwrap();
        doc.risk = None;
        assert!(ValidatedStrategy::with_defaults(doc).is_err());
    }

    #[test]
    fn empty_identity_fields_are_rejected() {
        // Replace whole lines rather than bare words: `market: "BTCUSDT"` with
        // only `BTCUSDT` swapped for `""` yields `market: """"`, which YAML
        // reads as a single quote character -- not an empty string.
        let cases = [
            (
                "name",
                SAMPLE.replace("name: \"Liquidity Sweep + Absorption\"", "name: \"  \""),
            ),
            (
                "market",
                SAMPLE.replace("market: \"BTCUSDT\"", "market: \"\""),
            ),
            (
                "version",
                SAMPLE.replace("version: \"2.1\"", "version: \"\""),
            ),
        ];
        for (field, yaml) in cases {
            assert!(
                paths(&yaml).contains(&field.to_string()),
                "expected an issue on `{field}`, got {:?}",
                paths(&yaml)
            );
        }
    }

    #[test]
    fn an_undeclared_timeframe_is_rejected() {
        let yaml = SAMPLE.replace("timeframe: setup", "timeframe: h4");
        let found = issues(&yaml);
        let issue = found
            .iter()
            .find(|i| i.path.ends_with(".timeframe"))
            .expect("expected a timeframe issue");
        assert!(
            issue.message.contains("undeclared timeframe `h4`"),
            "{issue:?}"
        );
        assert!(
            issue.message.contains("trend"),
            "should list what is declared"
        );
    }

    #[test]
    fn an_unknown_field_in_a_condition_is_rejected() {
        let yaml = SAMPLE.replace("delta > threshold(1500)", "deltas > 1500");
        let found = issues(&yaml);
        assert!(
            found
                .iter()
                .any(|i| i.message.contains("unknown field `deltas`")),
            "{found:?}"
        );
        // And the path must point at the exact condition.
        assert!(found.iter().any(|i| i.path == "entry.all_of[3].condition"));
    }

    #[test]
    fn an_unknown_function_is_rejected() {
        let yaml = SAMPLE.replace("delta > threshold(1500)", "delta > tunable(1500)");
        assert!(issues(&yaml)
            .iter()
            .any(|i| i.message.contains("unknown function `tunable`")));
    }

    #[test]
    fn a_non_boolean_condition_is_rejected() {
        let yaml = SAMPLE.replace("delta > threshold(1500)", "delta");
        assert!(issues(&yaml)
            .iter()
            .any(|i| i.message.contains("must be boolean")));
    }

    #[test]
    fn a_malformed_condition_reports_a_column() {
        let yaml = SAMPLE.replace("delta > threshold(1500)", "delta > ");
        let found = issues(&yaml);
        assert!(
            found.iter().any(|i| i.message.contains("unexpected end")),
            "{found:?}"
        );
    }

    #[test]
    fn risk_above_the_ceiling_is_rejected_however_it_is_asked_for() {
        let yaml = SAMPLE.replace("max_risk_pct: 1.0", "max_risk_pct: 50.0");
        let found = issues(&yaml);
        let issue = found
            .iter()
            .find(|i| i.path == "risk.max_risk_pct")
            .expect("expected a risk issue");
        assert!(issue.message.contains("hard ceiling"), "{issue:?}");
    }

    #[test]
    fn a_custom_ceiling_can_only_tighten() {
        let doc = from_yaml(&SAMPLE.replace("max_risk_pct: 1.0", "max_risk_pct: 2.0")).unwrap();
        let strict = Limits {
            max_risk_pct: 1.0,
            ..Limits::default()
        };
        assert!(validate_with(&doc, &strict).is_err());
    }

    #[test]
    fn non_positive_risk_is_rejected() {
        for value in ["0.0", "-1.0"] {
            let yaml = SAMPLE.replace("max_risk_pct: 1.0", &format!("max_risk_pct: {value}"));
            assert!(paths(&yaml).contains(&"risk.max_risk_pct".to_string()));
        }
    }

    #[test]
    fn missing_invalidation_is_rejected() {
        let yaml = SAMPLE.replace(
            "invalidation:\n  - timeframe: entry\n    condition: \"close_below(stop_price)\"\n",
            "",
        );
        assert!(paths(&yaml).contains(&"invalidation".to_string()));
    }

    #[test]
    fn missing_entry_is_rejected() {
        // Anchor on whole lines: a bare `find("entry:")` matches the *timeframe*
        // named `entry` inside `timeframes:` first, and cutting from there
        // leaves `risk:` indented under `timeframes`.
        let start = SAMPLE
            .find("\nentry:\n")
            .expect("sample has an entry block");
        let end = SAMPLE.find("\nrisk:\n").expect("sample has a risk block");
        let yaml = format!("{}{}", &SAMPLE[..start], &SAMPLE[end..]);
        assert!(
            !yaml.contains("\nentry:\n"),
            "the entry block should be gone:\n{yaml}"
        );
        assert!(paths(&yaml).contains(&"entry".to_string()));
    }

    #[test]
    fn an_empty_entry_block_is_rejected() {
        let yaml = SAMPLE.replace(
            "entry:\n  all_of:\n    - timeframe: trend\n      condition: market_structure.trend == \"bullish\"\n    - timeframe: setup\n      condition: absorption.detected == true\n    - timeframe: entry\n      condition: liquidity.swept == \"sell_side\"\n    - timeframe: entry\n      condition: delta > threshold(1500)",
            "entry:\n  all_of: []\n  any_of: []",
        );
        assert!(paths(&yaml).contains(&"entry".to_string()));
    }

    #[test]
    fn an_indicator_must_not_declare_trade_logic() {
        let yaml = SAMPLE.replace("kind: strategy", "kind: indicator");
        let found = paths(&yaml);
        for field in ["entry", "risk", "invalidation"] {
            assert!(
                found.contains(&field.to_string()),
                "missing {field} in {found:?}"
            );
        }
    }

    #[test]
    fn an_indicator_without_trade_logic_validates() {
        let yaml = r#"
name: "CVD overlay"
version: "1.0"
kind: indicator
market: BTCUSDT
timeframes:
  entry: 5m
"#;
        validate_str(yaml).expect("a bare indicator must validate");
    }

    #[test]
    fn direction_contradicting_the_stop_is_rejected() {
        let yaml = SAMPLE.replace("entry:\n  all_of:", "entry:\n  direction: short\n  all_of:");
        let found = issues(&yaml);
        let issue = found
            .iter()
            .find(|i| i.path == "entry.direction")
            .expect("expected a direction issue");
        assert!(issue.message.contains("contradicts"), "{issue:?}");
    }

    #[test]
    fn direction_matching_the_stop_is_accepted() {
        let yaml = SAMPLE.replace("entry:\n  all_of:", "entry:\n  direction: long\n  all_of:");
        validate_str(&yaml).expect("matching direction must be accepted");
    }

    #[test]
    fn a_side_less_stop_requires_an_explicit_direction() {
        let yaml = SAMPLE.replace(
            "\"below_sweep_low\"",
            "{kind: atr, multiple: 1.5, period: 14}",
        );
        let found = issues(&yaml);
        assert!(
            found
                .iter()
                .any(|i| i.path == "entry.direction" && i.message.contains("is required")),
            "{found:?}"
        );
    }

    #[test]
    fn a_side_less_stop_with_a_direction_validates() {
        let yaml = SAMPLE
            .replace(
                "\"below_sweep_low\"",
                "{kind: atr, multiple: 1.5, period: 14}",
            )
            .replace("entry:\n  all_of:", "entry:\n  direction: short\n  all_of:");
        validate_str(&yaml).expect("explicit direction makes an atr stop valid");
    }

    #[test]
    fn threshold_must_wrap_a_literal() {
        let yaml = SAMPLE.replace("threshold(1500)", "threshold(poc)");
        assert!(issues(&yaml)
            .iter()
            .any(|i| i.message.contains("numeric literal")));
    }

    #[test]
    fn non_positive_stop_and_target_parameters_are_rejected() {
        let cases = [
            (
                "{kind: atr, multiple: 0.0, period: 14}",
                "risk.stop.multiple",
            ),
            ("{kind: atr, multiple: 1.5, period: 0}", "risk.stop.period"),
            ("{kind: below_recent_low, bars: 0}", "risk.stop.bars"),
            ("{kind: fixed, price: 0.0}", "risk.stop.price"),
        ];
        for (stop, path) in cases {
            let yaml = SAMPLE.replace("\"below_sweep_low\"", stop);
            assert!(paths(&yaml).contains(&path.to_string()), "for {stop}");
        }
    }

    #[test]
    fn a_non_positive_take_profit_is_rejected() {
        let yaml = SAMPLE.replace("value: 2.5", "value: 0.0");
        assert!(paths(&yaml).contains(&"risk.take_profit.value".to_string()));
    }

    #[test]
    fn too_many_conditions_is_rejected() {
        let doc = from_yaml(SAMPLE).unwrap();
        let tight = Limits {
            max_conditions: 2,
            ..Limits::default()
        };
        assert!(validate_with(&doc, &tight).is_err());
    }

    #[test]
    fn too_many_timeframes_is_rejected() {
        let doc = from_yaml(SAMPLE).unwrap();
        let tight = Limits {
            max_timeframes: 2,
            ..Limits::default()
        };
        assert!(validate_with(&doc, &tight).is_err());
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        // Break four unrelated things and confirm all four come back.
        let yaml = SAMPLE
            .replace("market: \"BTCUSDT\"", "market: \"\"")
            .replace("max_risk_pct: 1.0", "max_risk_pct: 99.0")
            .replace("deltas > 1500", "delta > 1500")
            .replace("delta > threshold(1500)", "deltas > 1500");
        let found = issues(&yaml);
        assert!(found.len() >= 3, "expected several issues, got {found:?}");
        assert!(found.iter().any(|i| i.path == "market"));
        assert!(found.iter().any(|i| i.path == "risk.max_risk_pct"));
        assert!(found.iter().any(|i| i.path == "entry.all_of[3].condition"));
    }

    #[test]
    fn whitespace_only_timeframe_names_are_rejected() {
        let yaml = SAMPLE.replace("  trend: \"4h\"", "  \"  \": \"4h\"");
        assert!(paths(&yaml).iter().any(|p| p.contains("timeframes")));
    }
}
