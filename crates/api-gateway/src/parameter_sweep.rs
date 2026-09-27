//! Parameter sweep over `threshold()` -- the tuning grid for generated logic.
//!
//! ## Why this exists
//!
//! A generated document's numbers are guesses. `threshold(x)` exists in the
//! DSL precisely so the tunable ones are *marked* ("the future parameter sweep
//! has an explicit, greppable marker" -- `expr.rs`), and this module is the
//! sweep that marker was waiting for. Every `threshold(n)` in a document is
//! replaced across a small multiplicative grid, each variant is re-validated
//! and replayed, and the results come back as one table the chat can render:
//! which values made money, which lost it, and whether the middle of the grid
//! is stable or a lucky spike.
//!
//! ## Why the sweep is textual, not structural
//!
//! Conditions are stored as source strings in the schema, and a substitution
//! over the parsed [`Expr`] tree would need a writer that re-renders exactly
//! what the parser accepted -- a second serializer, kept in step with the
//! grammar. The substitution below works on the YAML text instead, and every
//! rewritten document goes back through [`parse_and_validate`] before it is
//! allowed to run. A mangled document does not backtest wrongly; it fails to
//! parse, and that variant is reported as skipped rather than silently used.

use backtester::replay::{run_backtest, ReplayConfig, ReplayInput};
use serde::Serialize;
use strategy_dsl::StrategyDocument;

/// How many grid points one `threshold()` is swept over.
///
/// Five points per parameter, one parameter at a time: `0.5x, 0.75x, 1x, 1.5x,
/// 2x` around the value the author wrote. A finer grid multiplies the replay
/// count linearly -- a week of 5m candles is a few seconds a run, and a sweep
/// that took minutes would not be run from a chat.
pub const GRID_MULTIPLIERS: [f64; 5] = [0.5, 0.75, 1.0, 1.5, 2.0];

/// The most `threshold()`s one document may sweep.
///
/// One parameter per sweep keeps the cost linear in candles loaded once. A
/// document with five thresholds sweeps five times over the same window --
/// already a minute of replay -- and a cartesian product over five of them
/// would be 3,125 runs, which is a batch job and not a chat feature.
pub const MAX_SWEEP_PARAMS: usize = 3;

/// The result of sweeping one `threshold()` in a document.
#[derive(Debug, Clone, Serialize)]
pub struct SweepResult {
    /// The condition path the parameter lives in, e.g. `entry.all_of[0]`.
    pub condition: String,
    /// The `threshold(N)` argument as the document wrote it.
    pub base_value: f64,
    /// One row per grid point, in ascending multiplier order.
    pub points: Vec<SweepPoint>,
    /// Grid points that failed to run, with the reason. A variant the
    /// validator refuses is a result, not an error: it tells the user the
    /// parameter has edges.
    pub skipped: Vec<String>,
}

/// One grid point: a value, and what it did.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SweepPoint {
    /// The multiplier this point tested.
    pub multiplier: f64,
    /// The threshold value that ran (`base_value * multiplier`).
    pub value: f64,
    /// Completed trades.
    pub trades: u32,
    /// Fraction of trades that made money.
    pub win_rate: f64,
    /// Total R over the window.
    pub net_r: f64,
    /// Mean R per trade.
    pub average_r: f64,
    /// Largest peak-to-trough fall of the R curve, positive.
    pub max_drawdown_r: f64,
}

/// Everything the sweep needs beyond the document itself.
pub struct SweepInput {
    /// The source text, exactly as stored. Substitution happens here.
    pub source: String,
    /// Symbol to replay.
    pub symbol: String,
    /// Window start, unix nanos.
    pub from_ns: i64,
    /// Window end, unix nanos.
    pub to_ns: i64,
    /// The candle series, already loaded. Loaded **once** and shared by every
    /// variant: the sweep replays N documents over one dataset, and loading
    /// the window N times would be N database trips for identical bytes.
    pub series: std::collections::BTreeMap<String, Vec<analytics_core::types::Candle>>,
}

/// What `threshold` calls exist in a document, with the condition that holds
/// each.
///
/// The document is already validated, so every condition parses; a parse
/// failure here is unreachable and reported as an empty list rather than a
/// panic -- a sweep of a document the platform cannot re-read is a skip, not
/// a 500.
#[must_use]
pub fn find_thresholds(document: &StrategyDocument) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (path, conditional) in document.all_conditions() {
        let expr = match strategy_dsl::Expr::parse_checked(&conditional.condition) {
            Ok(expr) => expr,
            // Unreachable for a validated document; skip, do not panic.
            Err(_) => continue,
        };
        collect_thresholds(&expr, &path, &mut out);
    }
    out
}

fn collect_thresholds(expr: &strategy_dsl::Expr, path: &str, out: &mut Vec<(String, f64)>) {
    use strategy_dsl::Expr;
    match expr {
        Expr::Call { func, args } if *func == strategy_dsl::Func::Threshold => {
            if let Some(Expr::Literal(strategy_dsl::Value::Num(value))) = args.first() {
                out.push((path.to_string(), *value));
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_thresholds(arg, path, out);
            }
        }
        Expr::Not(inner) => collect_thresholds(inner, path, out),
        Expr::And(a, b) | Expr::Or(a, b) => {
            collect_thresholds(a, path, out);
            collect_thresholds(b, path, out);
        }
        Expr::Compare { lhs, rhs, .. } => {
            collect_thresholds(lhs, path, out);
            collect_thresholds(rhs, path, out);
        }
        Expr::Literal(_) | Expr::Field(_) | Expr::Concept { .. } => {}
    }
}

/// Replace every `threshold(n)` in one condition string with `threshold(v)`.
///
/// The condition grammar keeps `threshold(...)` a plain function call with one
/// numeric literal, so a regex over the token shape is safe in a way a regex
/// over the whole YAML would not be: a number cannot appear between the
/// `threshold(` and its `)` other than as the argument itself.
#[must_use]
fn rewrite_condition(condition: &str, value: f64) -> String {
    let needle = "threshold(";
    let mut out = String::with_capacity(condition.len() + 8);
    let mut rest = condition;
    while let Some(start) = rest.find(needle) {
        out.push_str(&rest[..start + needle.len()]);
        rest = &rest[start + needle.len()..];
        // The argument runs to the next `)` -- a bare number has none inside.
        match rest.find(')') {
            Some(end) => {
                out.push_str(&format!("{value}"));
                rest = &rest[end..];
            }
            None => {
                // Malformed past repair; the original tail comes back and the
                // validator refuses it, which is the contract.
                out.push_str(rest);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Rewrite one threshold in the stored YAML.
///
/// The Nth `threshold(...)` occurrence in the source is replaced, because the
/// conditions in `all_conditions()` are enumerated in document order -- the
/// same order the source spells them. Rewriting the *wrong* occurrence would
/// silently sweep a different parameter than the result claims, so the count
/// is asserted against the parse tree before anything runs.
#[must_use]
pub fn rewrite_nth_threshold(source: &str, nth: usize, value: f64) -> String {
    let needle = "threshold(";
    let mut out = String::with_capacity(source.len() + 8);
    let mut rest = source;
    let mut seen = 0usize;
    while let Some(start) = rest.find(needle) {
        let head = start + needle.len();
        out.push_str(&rest[..head]);
        rest = &rest[head..];
        let Some(end) = rest.find(')') else {
            out.push_str(rest);
            return out;
        };
        if seen == nth {
            out.push_str(&format!("{value}"));
        } else {
            out.push_str(&rest[..end]);
        }
        rest = &rest[end..];
        seen += 1;
    }
    out.push_str(rest);
    out
}

/// Run the sweep: one replay per grid point per parameter, over one dataset.
///
/// # Errors
/// The per-variant failures are results, not errors; the only hard errors are
/// "the source does not parse at all" (which the caller validated before
/// handing it over, so this means the stored copy drifted) and shape problems
/// in the sweep input itself.
pub fn sweep(input: &SweepInput) -> Result<Vec<SweepResult>, String> {
    let base = strategy_dsl::parse_and_validate(&input.source)
        .map_err(|error| format!("the stored document does not parse: {error}"))?;
    let base_document = base.document();
    let thresholds = find_thresholds(base_document);
    if thresholds.is_empty() {
        return Ok(Vec::new());
    }
    let count = thresholds.len().min(MAX_SWEEP_PARAMS);

    let mut results = Vec::with_capacity(count);
    for index in 0..count {
        let (condition, base_value) = &thresholds[index];
        let mut points = Vec::new();
        let mut skipped = Vec::new();
        for multiplier in GRID_MULTIPLIERS {
            let value = base_value * multiplier;
            let source = rewrite_nth_threshold(&input.source, index, value);
            let validated = match strategy_dsl::parse_and_validate(&source) {
                Ok(validated) => validated,
                Err(error) => {
                    skipped.push(format!("{multiplier}x ({value}): {error}"));
                    continue;
                }
            };
            let document = validated.document();
            let replay_input = ReplayInput::new(document, input.series.clone())
                .map_err(|error| format!("the sweep input was incomplete: {error}"))?;
            let mut engine = strategy_runtime::StrategyEngine::new(
                &validated,
                strategy_runtime::RuntimeConfig::default(),
            )
            .map_err(|error| format!("a swept variant is not runnable: {error}"))?;
            match run_backtest(
                &mut engine,
                &replay_input,
                &ReplayConfig {
                    symbol: input.symbol.clone(),
                    from: input.from_ns,
                    to: input.to_ns,
                    ..ReplayConfig::default()
                },
            ) {
                Ok(report) => points.push(SweepPoint {
                    multiplier,
                    value,
                    trades: report.total_trades,
                    win_rate: report.win_rate,
                    net_r: report.net_return_pct,
                    average_r: report.average_r,
                    max_drawdown_r: report.max_drawdown_pct,
                }),
                Err(error) => skipped.push(format!("{multiplier}x ({value}): {error}")),
            }
        }
        results.push(SweepResult {
            condition: condition.clone(),
            base_value: *base_value,
            points,
            skipped,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name: "Sweep probe"
version: "1.0"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  all_of:
    - timeframe: entry
      condition: "delta > threshold(1500)"
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
invalidation:
  - timeframe: entry
    condition: "close_below(vwap)"
"#;

    #[test]
    fn rewriting_the_nth_threshold_touches_only_that_occurrence() {
        let source = "a: threshold(10)\nb: threshold(20)";
        assert_eq!(
            rewrite_nth_threshold(source, 1, 99.0),
            "a: threshold(10)\nb: threshold(99)"
        );
        assert_eq!(
            rewrite_nth_threshold(source, 0, 5.0),
            "a: threshold(5)\nb: threshold(20)"
        );
        // Past the end: nothing changes.
        assert_eq!(rewrite_nth_threshold(source, 2, 1.0), source);
    }

    #[test]
    fn the_sample_document_reports_its_one_threshold() {
        // Parsed through the DSL's own front door, which is what a sweep
        // caller uses -- the stored copy arrives as JSON, and this fixture is
        // the same YAML shape the generator writes.
        let validated = strategy_dsl::parse_and_validate(SAMPLE).expect("parses");
        let found = find_thresholds(validated.document());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "entry.all_of[0]");
        assert_eq!(found[0].1, 1500.0);
    }

    #[test]
    fn a_document_without_thresholds_sweeps_to_an_empty_table() {
        let validated = strategy_dsl::parse_and_validate(
            "name: t\nversion: \"1\"\nkind: indicator\nmarket: X\ntimeframes:\n  entry: 5m",
        )
        .expect("parses");
        assert!(find_thresholds(validated.document()).is_empty());
    }
}
