//! The strategy interpreter (`docs/06-STRATEGY-DSL.md`).
//!
//! A [`ValidatedStrategy`] becomes a [`StrategyEngine`], which implements the
//! spec's `Strategy` trait:
//!
//! ```text
//! fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal>;
//! ```
//!
//! ## Interpreted, not code-generated
//!
//! The spec is explicit that Phase 3 interprets the document rather than
//! compiling it to Rust. That is the right call for now: the document is
//! already parsed and type-checked, so interpretation costs one tree walk per
//! condition, and it keeps the execution path identical to the one the sandbox
//! will use later. A compiled path is an optimization to be justified by
//! profiling, not a thing to build speculatively.
//!
//! ## Two states, one clock
//!
//! The engine is either flat or in a position, and it does exactly one of two
//! things per candle:
//!
//! * **Flat** -- evaluate the entry block. If it fires, resolve the stop and
//!   target *now*, against the decision candle's close, and emit an
//!   [`EnterSignal`] carrying the resolved prices.
//! * **In a position** -- evaluate `invalidation` first, then `exit`. Stop and
//!   target are **not** checked here: they are price events, and the simulator
//!   is the component that watches price within a bar. The engine only decides
//!   from closed candles.
//!
//! That split is what keeps the engine free of any notion of intra-bar price
//! paths, which is what makes the no-look-ahead guarantee hold.
//!
//! ## Skips are recorded, never swallowed
//!
//! A setup can be perfectly valid and still not become a trade: `below_swing_low`
//! has no swing to reference yet, or the resolved stop lands on the wrong side
//! of price. The engine refuses to invent a level and records a [`SkipRecord`]
//! instead, so the report can say "fourteen entries were skipped because the
//! stop had no level to reference" rather than silently showing fewer trades.

use std::collections::BTreeSet;

use analytics_core::indicators::atr;
use serde::{Deserialize, Serialize};
use strategy_dsl::expr::{CompareOp, Expr, Func, Value};
use strategy_dsl::schema::{
    Conditional, DocumentKind, RiskBlock, StopSpec, TakeProfit, TakeProfitKind,
};
use strategy_dsl::{indexed_path, Direction, StrategyDocument, ValidatedStrategy};

use crate::context::{swept_level_price, FieldValue, MarketContext, TimeframeView};
use crate::error::RuntimeError;
use crate::signal::{EnterSignal, ExitSignal, ExitTrigger, Signal};

/// Tuning the document does not (and must not) control.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// How far beyond a referenced level a stop is placed, in basis points.
    ///
    /// "Just beyond the swept low" needs a number. Five basis points is wide
    /// enough to survive the level being touched exactly, and narrow enough
    /// that the stop is still *at* the level rather than somewhere else.
    pub stop_buffer_bps: f64,
    /// ATR period for take-profits that are ATR multiples but do not say which.
    ///
    /// The schema's `take_profit` carries only a kind and a value, so an
    /// `atr_multiple` target has no period of its own. Rather than invent one
    /// per call site, it is pinned here and documented.
    pub default_atr_period: usize,
    /// Lookback for `new_low()` / `new_high()` called without an argument.
    pub default_lookback: usize,
    /// How many closed candles each timeframe view retains.
    ///
    /// The replay driver must honour this, because `new_low(n)` is defined in
    /// terms of the retained window.
    pub max_history: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            stop_buffer_bps: 5.0,
            default_atr_period: 14,
            default_lookback: 20,
            max_history: 500,
        }
    }
}

impl RuntimeConfig {
    /// The stop buffer as a fraction, e.g. `0.0005`.
    #[must_use]
    pub fn stop_buffer(&self) -> f64 {
        self.stop_buffer_bps / 10_000.0
    }
}

/// The execution contract from the spec.
///
/// Implemented by [`StrategyEngine`] for documents, and by hand for tests --
/// including the deliberately cheating strategy the no-look-ahead test uses.
pub trait Strategy {
    /// Decide, given everything visible at the close of one candle.
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal>;
}

/// A condition that failed to become a trade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipRecord {
    /// Close time of the candle the decision was made on.
    pub at: i64,
    /// The declared timeframe the entry was evaluated on.
    pub timeframe: String,
    /// Why no signal was produced.
    pub reason: String,
}

/// A condition, parsed once and tagged with where it came from.
#[derive(Debug, Clone)]
struct Compiled {
    /// JSON path in the document, e.g. `entry.all_of[2]`.
    path: String,
    /// What to call it in a trade log: the author's label, or the source text.
    label: String,
    /// Declared timeframe name the condition is evaluated against.
    timeframe: String,
    /// The parsed expression.
    expr: Expr,
}

impl Compiled {
    fn compile(list: &[Conditional], prefix: &str) -> Result<Vec<Self>, RuntimeError> {
        list.iter()
            .enumerate()
            .map(|(index, conditional)| {
                let path = indexed_path(prefix, index);
                let expr = Expr::parse(&conditional.condition)
                    .map_err(|e| RuntimeError::ConditionEvaluation(path.clone(), e.to_string()))?;
                Ok(Self {
                    path,
                    label: conditional
                        .label
                        .clone()
                        .unwrap_or_else(|| conditional.condition.clone()),
                    timeframe: conditional.timeframe.clone(),
                    expr,
                })
            })
            .collect()
    }
}

/// What one candle's evaluation produced.
enum Attempt {
    /// A tradeable signal.
    Signal(Box<Signal>),
    /// Nothing to do -- the conditions did not fire. The ordinary case.
    Nothing,
    /// The conditions fired but no trade could be constructed.
    Skipped(String),
}

/// Executes a [`ValidatedStrategy`] against a [`MarketContext`].
#[derive(Debug)]
pub struct StrategyEngine {
    document: StrategyDocument,
    decision_timeframe: String,
    direction: Direction,
    risk: RiskBlock,
    /// Every timeframe the document declared. Used to tell a genuinely
    /// undeclared timeframe (a bug) from a declared one that has not produced
    /// its first candle yet (a warm-up).
    declared: BTreeSet<String>,
    entry_all: Vec<Compiled>,
    entry_any: Vec<Compiled>,
    invalidation: Vec<Compiled>,
    exit_all: Vec<Compiled>,
    exit_any: Vec<Compiled>,
    config: RuntimeConfig,
    skips: Vec<SkipRecord>,
    candles_seen: u64,
    entries_emitted: u64,
    exits_emitted: u64,
}

impl StrategyEngine {
    /// Build an engine from a strategy that has already passed validation.
    ///
    /// Taking [`ValidatedStrategy`] rather than a [`StrategyDocument`] is the
    /// point: an unvalidated document does not typecheck here, so "remember to
    /// validate" is not a rule anyone can forget.
    pub fn new(validated: &ValidatedStrategy, config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let document = validated.document().clone();

        if document.kind == DocumentKind::Indicator {
            return Err(RuntimeError::NotTradable(
                "an `indicator` has no entry or risk block; there is nothing to trade".into(),
            ));
        }

        let (decision_timeframe, _) = document
            .decision_timeframe()
            .ok_or_else(|| RuntimeError::NotTradable("no timeframes declared".into()))?;
        let decision_timeframe = decision_timeframe.to_string();

        let direction = document.direction().ok_or_else(|| {
            RuntimeError::NotTradable(
                "no direction: the document declares neither `entry.direction` nor a stop \
                 rule that implies one"
                    .into(),
            )
        })?;

        let risk = document.risk.clone().ok_or_else(|| {
            RuntimeError::NotTradable(
                "no `risk` block; there is nothing to size a trade with".into(),
            )
        })?;

        let (entry_all, entry_any) = match &document.entry {
            Some(entry) => (
                Compiled::compile(&entry.all_of, "entry.all_of")?,
                Compiled::compile(&entry.any_of, "entry.any_of")?,
            ),
            None => (Vec::new(), Vec::new()),
        };

        let (exit_all, exit_any) = match &document.exit {
            Some(exit) => (
                Compiled::compile(&exit.all_of, "exit.all_of")?,
                Compiled::compile(&exit.any_of, "exit.any_of")?,
            ),
            None => (Vec::new(), Vec::new()),
        };

        Ok(Self {
            invalidation: Compiled::compile(&document.invalidation, "invalidation")?,
            declared: document.timeframes.keys().cloned().collect(),
            document,
            decision_timeframe,
            direction,
            risk,
            entry_all,
            entry_any,
            exit_all,
            exit_any,
            config,
            skips: Vec::new(),
            candles_seen: 0,
            entries_emitted: 0,
            exits_emitted: 0,
        })
    }

    /// The document being executed.
    #[must_use]
    pub fn document(&self) -> &StrategyDocument {
        &self.document
    }

    /// The timeframe decisions are made on.
    #[must_use]
    pub fn decision_timeframe(&self) -> &str {
        &self.decision_timeframe
    }

    /// The runtime configuration this engine was built with.
    ///
    /// Exposed so a caller that also buffers candles -- the backtester's replay,
    /// for instance -- can size its buffer from the *same* configuration the
    /// engine reads. Two independently configured `max_history` values would
    /// silently break lookback conditions: the engine would ask for a window
    /// wider than the caller retained and get `None` back.
    #[must_use]
    pub const fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// The direction the document trades.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Setups that fired but could not become trades.
    #[must_use]
    pub fn skips(&self) -> &[SkipRecord] {
        &self.skips
    }

    /// Every condition the engine compiled, as `(path, label)` pairs.
    ///
    /// The path is the document location (`entry.all_of[3]`); the label is what
    /// the author called it, or the condition source when they did not name it.
    /// The strategy editor highlights a condition by path, and the agent's
    /// explainability panel refers to one by label, so both need naming the
    /// same way the document does.
    #[must_use]
    pub fn condition_paths(&self) -> Vec<(&str, &str)> {
        self.entry_all
            .iter()
            .chain(&self.entry_any)
            .chain(&self.invalidation)
            .chain(&self.exit_all)
            .chain(&self.exit_any)
            .map(|condition| (condition.path.as_str(), condition.label.as_str()))
            .collect()
    }

    /// How many candles have been fed in.
    #[must_use]
    pub const fn candles_seen(&self) -> u64 {
        self.candles_seen
    }

    /// How many entry signals have been emitted.
    #[must_use]
    pub const fn entries_emitted(&self) -> u64 {
        self.entries_emitted
    }

    /// How many exit signals have been emitted.
    #[must_use]
    pub const fn exits_emitted(&self) -> u64 {
        self.exits_emitted
    }

    /// Evaluate one compiled condition.
    ///
    /// Three outcomes, and the distinction between them is the whole reason
    /// this is a method rather than a bare call:
    ///
    /// * the timeframe is **not declared** -- a bug the validator should have
    ///   caught, so it is an error, not a `false`;
    /// * the timeframe is declared but has **no candle yet** -- the coarser
    ///   context timeframes warm up after the decision timeframe does, and a
    ///   condition that cannot be evaluated yet simply does not fire;
    /// * otherwise, evaluate.
    fn eval_condition(
        &self,
        condition: &Compiled,
        ctx: &MarketContext,
    ) -> Result<bool, RuntimeError> {
        if !self.declared.contains(&condition.timeframe) {
            return Err(RuntimeError::UndeclaredTimeframe(
                condition.timeframe.clone(),
            ));
        }
        match ctx.view(&condition.timeframe) {
            None => Ok(false),
            Some(view) => Ok(eval(&condition.expr, ctx, view, &self.config)?.truthy()),
        }
    }

    /// Evaluate the entry block. `&self`, so the caller owns the bookkeeping.
    fn enter_signal(&self, ctx: &MarketContext) -> Result<Attempt, RuntimeError> {
        if self.entry_all.is_empty() && self.entry_any.is_empty() {
            return Ok(Attempt::Nothing);
        }

        let mut reasons = Vec::new();

        for condition in &self.entry_all {
            if self.eval_condition(condition, ctx)? {
                reasons.push(condition.label.clone());
            } else {
                return Ok(Attempt::Nothing);
            }
        }

        if !self.entry_any.is_empty() {
            let mut fired = Vec::new();
            for condition in &self.entry_any {
                if self.eval_condition(condition, ctx)? {
                    fired.push(condition.label.clone());
                }
            }
            if fired.is_empty() {
                return Ok(Attempt::Nothing);
            }
            reasons.extend(fired);
        }

        let view = ctx
            .decision()
            .ok_or_else(|| RuntimeError::UndeclaredTimeframe(self.decision_timeframe.clone()))?;
        let reference = view.candle.close;

        let Some(stop_price) = self.resolve_stop(self.risk.stop, reference, ctx)? else {
            return Ok(Attempt::Skipped(format!(
                "stop rule `{}` has no level to reference yet",
                self.risk.stop.kind_name()
            )));
        };

        let take_profit_price = match self.risk.take_profit {
            Some(take_profit) => self.resolve_target(take_profit, reference, stop_price, ctx)?,
            None => None,
        };

        let signal = EnterSignal {
            direction: self.direction,
            reference_price: reference,
            stop_price,
            take_profit_price,
            max_risk_pct: self.risk.max_risk_pct,
            reasons,
        };

        // A stop or target on the wrong side of price is not a bad trade, it is
        // a malformed one. Refuse it loudly rather than let the simulator open
        // a position it will close on the very next bar.
        if !signal.stop_is_valid() {
            return Ok(Attempt::Skipped(format!(
                "resolved stop {} is on the wrong side of the reference price {} for a {} trade",
                signal.stop_price, signal.reference_price, self.direction
            )));
        }
        if !signal.target_is_valid() {
            return Ok(Attempt::Skipped(format!(
                "resolved target {:?} is on the wrong side of the reference price {} for a {} trade",
                signal.take_profit_price, signal.reference_price, self.direction
            )));
        }

        Ok(Attempt::Signal(Box::new(Signal::Enter(signal))))
    }

    /// Evaluate `invalidation`, then the `exit` block. `&self`.
    fn exit_signal(&self, ctx: &MarketContext) -> Result<Attempt, RuntimeError> {
        let mut fired = Vec::new();
        for condition in &self.invalidation {
            if self.eval_condition(condition, ctx)? {
                fired.push(condition.label.clone());
            }
        }
        if !fired.is_empty() {
            return Ok(Attempt::Signal(Box::new(Signal::Exit(
                ExitSignal::from_conditions(ExitTrigger::Invalidation, fired),
            ))));
        }

        if self.exit_all.is_empty() && self.exit_any.is_empty() {
            return Ok(Attempt::Nothing);
        }

        // Same shape as entry: `all_of` must all hold, and `any_of` -- when
        // present -- must contribute at least one. Both together are ANDed.
        let mut reasons = Vec::new();
        for condition in &self.exit_all {
            if self.eval_condition(condition, ctx)? {
                reasons.push(condition.label.clone());
            } else {
                return Ok(Attempt::Nothing);
            }
        }

        if self.exit_any.is_empty() {
            if reasons.is_empty() {
                return Ok(Attempt::Nothing);
            }
        } else {
            let mut any_fired = Vec::new();
            for condition in &self.exit_any {
                if self.eval_condition(condition, ctx)? {
                    any_fired.push(condition.label.clone());
                }
            }
            if any_fired.is_empty() {
                return Ok(Attempt::Nothing);
            }
            reasons.extend(any_fired);
        }

        Ok(Attempt::Signal(Box::new(Signal::Exit(
            ExitSignal::from_conditions(ExitTrigger::ExitCondition, reasons),
        ))))
    }

    /// Resolve a stop rule to a concrete price against the decision timeframe.
    ///
    /// `Ok(None)` means the rule references something the market has not
    /// produced yet -- no swept low, no confirmed swing, not enough bars for
    /// ATR. That is a warm-up, not an error, and the caller records it as a
    /// skip rather than substituting a guess.
    fn resolve_stop(
        &self,
        spec: StopSpec,
        reference: f64,
        ctx: &MarketContext,
    ) -> Result<Option<f64>, RuntimeError> {
        let view = ctx
            .decision()
            .ok_or_else(|| RuntimeError::UndeclaredTimeframe(self.decision_timeframe.clone()))?;
        let buffer = self.config.stop_buffer();

        let price = match spec {
            StopSpec::BelowSweepLow => {
                swept_level_price(view, false).map(|level| level * (1.0 - buffer))
            }
            StopSpec::AboveSweepHigh => {
                swept_level_price(view, true).map(|level| level * (1.0 + buffer))
            }
            StopSpec::BelowSwingLow => view
                .state
                .swing_lows
                .last()
                .map(|price| price * (1.0 - buffer)),
            StopSpec::AboveSwingHigh => view
                .state
                .swing_highs
                .last()
                .map(|price| price * (1.0 + buffer)),
            StopSpec::BelowRecentLow { bars } => view
                .last_n(bars)
                .map(|window| window.iter().map(|c| c.low).fold(f64::INFINITY, f64::min))
                .map(|low| low * (1.0 - buffer)),
            StopSpec::AboveRecentHigh { bars } => view
                .last_n(bars)
                .map(|window| {
                    window
                        .iter()
                        .map(|c| c.high)
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .map(|high| high * (1.0 + buffer)),
            StopSpec::Atr { multiple, period } => {
                last_atr(view, period).map(|atr| match self.direction {
                    Direction::Long => reference - multiple * atr,
                    Direction::Short => reference + multiple * atr,
                })
            }
            StopSpec::Fixed { price } => Some(price),
        };

        Ok(price)
    }

    /// Resolve a take-profit rule to a concrete price.
    fn resolve_target(
        &self,
        take_profit: TakeProfit,
        reference: f64,
        stop_price: f64,
        ctx: &MarketContext,
    ) -> Result<Option<f64>, RuntimeError> {
        let view = ctx
            .decision()
            .ok_or_else(|| RuntimeError::UndeclaredTimeframe(self.decision_timeframe.clone()))?;

        let distance = match take_profit.kind {
            TakeProfitKind::RiskMultiple => {
                let risk = (reference - stop_price).abs();
                if risk <= 0.0 {
                    return Ok(None);
                }
                take_profit.value * risk
            }
            TakeProfitKind::AtrMultiple => {
                let Some(atr) = last_atr(view, self.config.default_atr_period) else {
                    return Ok(None);
                };
                take_profit.value * atr
            }
            TakeProfitKind::FixedPrice => return Ok(Some(take_profit.value)),
        };

        Ok(Some(match self.direction {
            Direction::Long => reference + distance,
            Direction::Short => reference - distance,
        }))
    }
}

impl Strategy for StrategyEngine {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
        self.candles_seen += 1;

        let attempt = if ctx.in_position() {
            self.exit_signal(ctx)
        } else {
            self.enter_signal(ctx)
        };

        match attempt {
            Ok(Attempt::Signal(signal)) => {
                match signal.as_ref() {
                    Signal::Enter(_) => self.entries_emitted += 1,
                    Signal::Exit(_) => self.exits_emitted += 1,
                }
                Some(*signal)
            }
            Ok(Attempt::Nothing) => None,
            Ok(Attempt::Skipped(reason)) => {
                self.skips.push(SkipRecord {
                    at: ctx.now,
                    timeframe: self.decision_timeframe.clone(),
                    reason,
                });
                None
            }
            Err(error) => {
                self.skips.push(SkipRecord {
                    at: ctx.now,
                    timeframe: self.decision_timeframe.clone(),
                    reason: error.to_string(),
                });
                None
            }
        }
    }
}

/// The newest ATR value over the retained window.
fn last_atr(view: &TimeframeView, period: usize) -> Option<f64> {
    if period == 0 {
        return None;
    }
    atr(&view.history, period).last().copied().flatten()
}

/// Relative tolerance for `==` on floats.
///
/// Two floats that agree to nine significant digits are the same number as far
/// as a trading condition is concerned, and exact equality would make
/// `poc == vah` fire or not fire depending on rounding in the profile builder.
fn approx_eq(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() <= 1e-9 * scale
}

/// The lookback for `new_low()` / `new_high()`, defaulting when not given.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn lookback(args: &[Expr], default: usize) -> usize {
    match args.first() {
        Some(Expr::Literal(Value::Num(n))) if n.is_finite() && *n >= 1.0 => *n as usize,
        _ => default,
    }
}

/// Evaluate a condition tree against one timeframe view.
fn eval<'a>(
    expr: &'a Expr,
    ctx: &'a MarketContext,
    view: &'a TimeframeView,
    config: &RuntimeConfig,
) -> Result<FieldValue<'a>, RuntimeError> {
    match expr {
        Expr::Literal(Value::Bool(value)) => Ok(FieldValue::Bool(*value)),
        Expr::Literal(Value::Num(value)) => Ok(FieldValue::Num(*value)),
        Expr::Literal(Value::Str(value)) => Ok(FieldValue::Str(value.as_str())),

        Expr::Field(field) => Ok(ctx.read(&view.name, *field)),

        Expr::Not(inner) => Ok(FieldValue::Bool(!eval(inner, ctx, view, config)?.truthy())),

        Expr::And(a, b) => {
            let left = eval(a, ctx, view, config)?.truthy();
            let right = eval(b, ctx, view, config)?.truthy();
            Ok(FieldValue::Bool(left && right))
        }
        Expr::Or(a, b) => {
            let left = eval(a, ctx, view, config)?.truthy();
            let right = eval(b, ctx, view, config)?.truthy();
            Ok(FieldValue::Bool(left || right))
        }

        Expr::Compare { op, lhs, rhs } => {
            let left = eval(lhs, ctx, view, config)?;
            let right = eval(rhs, ctx, view, config)?;
            Ok(compare(*op, left, right))
        }

        Expr::Call { func, args } => eval_call(*func, args, ctx, view, config),
    }
}

/// Apply a comparison. An absent operand makes the comparison false.
fn compare(op: CompareOp, left: FieldValue<'_>, right: FieldValue<'_>) -> FieldValue<'static> {
    if left.is_absent() || right.is_absent() {
        return FieldValue::Bool(false);
    }

    match op {
        CompareOp::Eq | CompareOp::Ne => {
            let equal = match (left, right) {
                (FieldValue::Num(a), FieldValue::Num(b)) => approx_eq(a, b),
                (FieldValue::Str(a), FieldValue::Str(b)) => a == b,
                (FieldValue::Bool(a), FieldValue::Bool(b)) => a == b,
                // The validator rejects mixed-type equality, so this is
                // unreachable for a validated document. False is the safe
                // answer if it ever is reached.
                _ => false,
            };
            FieldValue::Bool(if op == CompareOp::Eq { equal } else { !equal })
        }
        _ => {
            let (Some(a), Some(b)) = (left.number_or_absent(), right.number_or_absent()) else {
                return FieldValue::Bool(false);
            };
            FieldValue::Bool(match op {
                CompareOp::Gt => a > b,
                CompareOp::Ge => a >= b,
                CompareOp::Lt => a < b,
                CompareOp::Le => a <= b,
                CompareOp::Eq | CompareOp::Ne => unreachable!("handled above"),
            })
        }
    }
}

/// Evaluate a function call.
fn eval_call<'a>(
    func: Func,
    args: &'a [Expr],
    ctx: &'a MarketContext,
    view: &'a TimeframeView,
    config: &RuntimeConfig,
) -> Result<FieldValue<'a>, RuntimeError> {
    match func {
        // `threshold` marks a tunable number; it evaluates to its argument.
        Func::Threshold => match args.first() {
            Some(arg) => eval(arg, ctx, view, config),
            None => Ok(FieldValue::Absent),
        },

        Func::Above | Func::Below => {
            let Some(a) = num_arg(args, 0, ctx, view, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            let Some(b) = num_arg(args, 1, ctx, view, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            Ok(FieldValue::Bool(if func == Func::Above {
                a > b
            } else {
                a < b
            }))
        }

        Func::CrossesAbove => {
            let Some(a) = num_arg(args, 0, ctx, view, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            let Some(b) = num_arg(args, 1, ctx, view, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            // Without a previous bar there is no cross, only a state.
            let Some(previous) = view.prev() else {
                return Ok(FieldValue::Bool(false));
            };
            let Some(prev_a) = num_arg(args, 0, ctx, previous, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            let Some(prev_b) = num_arg(args, 1, ctx, previous, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            Ok(FieldValue::Bool(a > b && prev_a <= prev_b))
        }

        Func::NewLow | Func::NewHigh => {
            let n = lookback(args, config.default_lookback);
            // An insufficient window is a warm-up, not a new extreme.
            let Some(window) = view.last_n(n) else {
                return Ok(FieldValue::Bool(false));
            };
            let (current, extreme) = if func == Func::NewLow {
                (
                    view.candle.low,
                    window.iter().map(|c| c.low).fold(f64::INFINITY, f64::min),
                )
            } else {
                (
                    view.candle.high,
                    window
                        .iter()
                        .map(|c| c.high)
                        .fold(f64::NEG_INFINITY, f64::max),
                )
            };
            Ok(FieldValue::Bool(approx_eq(current, extreme)))
        }

        Func::CloseBelow | Func::CloseAbove => {
            let Some(x) = num_arg(args, 0, ctx, view, config)? else {
                return Ok(FieldValue::Bool(false));
            };
            let close = view.candle.close;
            Ok(FieldValue::Bool(if func == Func::CloseBelow {
                close < x
            } else {
                close > x
            }))
        }
    }
}

/// Evaluate argument `index` as a number, or `None` when absent.
fn num_arg<'a>(
    args: &'a [Expr],
    index: usize,
    ctx: &'a MarketContext,
    view: &'a TimeframeView,
    config: &RuntimeConfig,
) -> Result<Option<f64>, RuntimeError> {
    match args.get(index) {
        Some(arg) => Ok(eval(arg, ctx, view, config)?.number_or_absent()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PositionView;
    use analytics_core::state::{build_market_state, MarketStateConfig};
    use analytics_core::types::{Candle, Timeframe};
    use std::collections::BTreeMap;

    const SAMPLE: &str = r#"
name: "Liquidity Sweep + Absorption"
version: "2.1"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  all_of:
    - timeframe: entry
      condition: liquidity.swept == "sell_side"
      label: sweep
    - timeframe: entry
      condition: delta > threshold(1500)
      label: delta
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.5
invalidation:
  - timeframe: entry
    condition: "close_below(stop_price)"
    label: lost-stop
"#;

    /// An entry with no liquidity condition, for tests about stop resolution
    /// rather than about the sweep.
    const DELTA_ONLY: &str = r#"
name: "delta only"
version: "1"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: delta > threshold(1500)
      label: delta
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
invalidation:
  - timeframe: entry
    condition: "close_below(stop_price)"
    label: lost-stop
"#;

    fn candle(open_time: i64, high: f64, low: f64, close: f64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time,
            open: close,
            high,
            low,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn config() -> MarketStateConfig {
        MarketStateConfig {
            bucket_size: 1.0,
            // One-bar lookbacks so a five-candle fixture can actually confirm
            // structure and sweep a level, instead of falling under the
            // production confirmation thresholds and testing nothing.
            structure: analytics_core::market_structure::StructureConfig { lookback: 1 },
            liquidity: analytics_core::liquidity::LiquidityConfig {
                lookback: 1,
                ..analytics_core::liquidity::LiquidityConfig::default()
            },
            ..MarketStateConfig::default()
        }
    }

    /// A series that sweeps a low, then pushes delta up hard.
    ///
    /// Three things have to hold for `liquidity.swept == "sell_side"` to be true
    /// on the entry bar, and a fixture that misses any of them tests nothing:
    ///
    /// 1. A swing low must **form** -- bar 1's low of 8, confirmed by bars 0 and
    ///    2 sitting above it. A level cannot be swept before it exists.
    /// 2. A **later** bar must trade through it (bar 3's low of 7).
    /// 3. The entry bar must not sweep anything itself, or it would report the
    ///    side of *its own* sweep instead.
    ///
    /// Every high is 10.0, so no swing high ever forms and there is no
    /// competing buy-side level. Volumes are in the thousands because the entry
    /// condition is `delta > threshold(1500)`.
    fn series() -> Vec<Candle> {
        vec![
            candle(0, 10.0, 9.0, 9.5, 1000.0, 1000.0),
            // Swing low at 8.
            candle(300, 10.0, 8.0, 9.0, 1000.0, 1000.0),
            candle(600, 10.0, 9.0, 9.5, 1000.0, 1000.0),
            // Trades below 8, sweeping the level.
            candle(900, 10.0, 7.0, 8.0, 1000.0, 3000.0),
            // Closes at 10 with delta +4000, clearing threshold(1500).
            candle(1200, 10.0, 8.5, 10.0, 5000.0, 1000.0),
        ]
    }

    /// The mirror image: a swing high at 12 is swept upward, and the entry bar
    /// closes at 10 -- *below* the high -- so an `above_swing_high` stop is
    /// valid for a short.
    ///
    /// Every low is 10.0, so no swing low forms to compete on the sell side.
    fn short_series() -> Vec<Candle> {
        vec![
            candle(0, 11.0, 10.0, 10.5, 1000.0, 1000.0),
            // Swing high at 12.
            candle(300, 12.0, 10.0, 11.0, 1000.0, 1000.0),
            candle(600, 11.0, 10.0, 10.5, 1000.0, 1000.0),
            // Trades above 12, sweeping the level.
            candle(900, 13.0, 10.0, 12.0, 1000.0, 1000.0),
            // Closes at 10 with sell volume 5000.
            candle(1200, 11.0, 10.0, 10.0, 1000.0, 5000.0),
        ]
    }

    fn engine(yaml: &str) -> StrategyEngine {
        let validated = strategy_dsl::parse_and_validate(yaml).expect("fixture must validate");
        StrategyEngine::new(&validated, RuntimeConfig::default()).expect("fixture must be tradable")
    }

    fn ctx_from(candles: Vec<Candle>, position: Option<PositionView>) -> MarketContext {
        let state = build_market_state(&candles, &[], &config()).unwrap();
        let previous = if candles.len() > 1 {
            let earlier = candles[..candles.len() - 1].to_vec();
            build_market_state(&earlier, &[], &config()).map(|state| {
                Box::new(TimeframeView {
                    name: "entry".into(),
                    timeframe: Timeframe::M5,
                    candle: earlier.last().unwrap().clone(),
                    state,
                    previous: None,
                    history: earlier,
                })
            })
        } else {
            None
        };

        let view = TimeframeView {
            name: "entry".into(),
            timeframe: Timeframe::M5,
            candle: candles.last().unwrap().clone(),
            state,
            previous,
            history: candles,
        };

        let mut timeframes = BTreeMap::new();
        timeframes.insert("entry".to_string(), view);

        MarketContext {
            symbol: "BTCUSDT".into(),
            now: timeframes["entry"].candle.open_time + Timeframe::M5.nanos(),
            decision_timeframe: "entry".into(),
            timeframes,
            position,
            equity: 10_000.0,
        }
    }

    #[test]
    fn an_indicator_is_refused_rather_than_silently_not_trading() {
        let yaml = r#"
name: "overlay"
version: "1"
kind: indicator
market: BTCUSDT
timeframes:
  entry: 5m
"#;
        let validated = strategy_dsl::parse_and_validate(yaml).unwrap();
        let err = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap_err();
        assert!(matches!(err, RuntimeError::NotTradable(_)), "{err:?}");
    }

    #[test]
    fn a_side_less_stop_without_a_direction_is_refused() {
        // Validation catches this, so the engine's own guard is a backstop --
        // but a backstop that returns an error beats one that guesses "long".
        let yaml = SAMPLE.replace("\"below_sweep_low\"", "{kind: fixed, price: 5.0}");
        let err = strategy_dsl::parse_and_validate(&yaml).unwrap_err();
        assert!(matches!(err, strategy_dsl::DslError::Validation { .. }));
    }

    #[test]
    fn the_engine_reports_the_decision_timeframe_and_direction() {
        let engine = engine(SAMPLE);
        assert_eq!(engine.decision_timeframe(), "entry");
        assert_eq!(engine.direction(), Direction::Long);
    }

    #[test]
    fn compiled_conditions_keep_their_document_paths() {
        let engine = engine(SAMPLE);
        let paths = engine.condition_paths();
        assert_eq!(
            paths,
            vec![
                ("entry.all_of[0]", "sweep"),
                ("entry.all_of[1]", "delta"),
                ("invalidation[0]", "lost-stop"),
            ]
        );
    }

    #[test]
    fn a_fired_entry_carries_resolved_stop_target_and_reasons() {
        let mut engine = engine(SAMPLE);
        let ctx = ctx_from(series(), None);
        let signal = engine
            .on_candle(&ctx)
            .expect("the sweep series must trigger");

        let enter = signal.as_enter().expect("an entry");
        assert_eq!(enter.direction, Direction::Long);
        assert!((enter.reference_price - 10.0).abs() < 1e-9);
        // The stop references the swept *level* -- the swing low at 8 -- not the
        // sweep's extreme of 7. "Just beyond the swept low" means just beyond
        // the level that was taken out, buffered down by 5bps.
        assert!(enter.stop_price < 8.0, "stop was {}", enter.stop_price);
        assert!(enter.stop_price > 7.99, "stop was {}", enter.stop_price);
        // 2.5R off the resolved risk.
        let risk = enter.reference_price - enter.stop_price;
        let reward = enter.take_profit_price.unwrap() - enter.reference_price;
        assert!((reward / risk - 2.5).abs() < 1e-9);
        assert_eq!(
            enter.reasons,
            vec!["sweep".to_string(), "delta".to_string()]
        );
    }

    #[test]
    fn no_signal_when_the_conditions_do_not_hold() {
        let mut engine = engine(SAMPLE);
        // Quiet series: no sweep, no delta spike.
        let quiet = vec![
            candle(0, 10.0, 9.0, 9.5, 1.0, 1.0),
            candle(300, 10.0, 9.0, 9.5, 1.0, 1.0),
            candle(600, 10.0, 9.0, 9.5, 1.0, 1.0),
        ];
        assert!(engine.on_candle(&ctx_from(quiet, None)).is_none());
    }

    #[test]
    fn an_unresolvable_stop_becomes_a_recorded_skip_not_a_guess() {
        // `below_swing_low` on a series that never confirms a swing: the entry
        // conditions fire, but there is no level to hang a stop on.
        let yaml = DELTA_ONLY.replace("\"below_sweep_low\"", "\"below_swing_low\"");
        let mut engine = engine(&yaml);

        let flat = vec![
            candle(0, 10.0, 10.0, 10.0, 5000.0, 1000.0),
            candle(300, 10.0, 10.0, 10.0, 5000.0, 1000.0),
        ];
        let ctx = ctx_from(flat, None);
        assert!(engine.on_candle(&ctx).is_none(), "nothing to trade");

        assert_eq!(engine.entries_emitted(), 0);
        assert_eq!(engine.skips().len(), 1, "the refusal must be recorded");
        assert!(
            engine.skips()[0].reason.contains("below_swing_low"),
            "{:?}",
            engine.skips()[0]
        );
    }

    #[test]
    fn invalidation_fires_before_the_exit_block() {
        let yaml = SAMPLE.replace(
            "invalidation:\n  - timeframe: entry\n    condition: \"close_below(stop_price)\"\n    label: lost-stop\n",
            "invalidation:\n  - timeframe: entry\n    condition: \"close_below(stop_price)\"\n    label: lost-stop\nexit:\n  all_of:\n    - timeframe: entry\n      condition: delta > 0\n      label: always\n",
        );
        let mut engine = engine(&yaml);

        let position = PositionView {
            direction: Direction::Long,
            entry_price: 10.0,
            entry_time: 1200,
            stop_price: 9.0,
            take_profit_price: Some(12.5),
            size: 1.0,
            bars_in_trade: 1,
            unrealized_r: 0.0,
        };

        // The close is 10.0 and the stop is 9.0, so `close_below(stop_price)` is
        // false and the exit block's `delta > 0` should be what fires.
        let ctx = ctx_from(series(), Some(position));
        let signal = engine.on_candle(&ctx).expect("exit block must fire");
        let exit = signal.as_exit().expect("an exit");
        assert_eq!(exit.trigger, ExitTrigger::ExitCondition);
        assert_eq!(exit.reasons, vec!["always".to_string()]);
    }

    #[test]
    fn invalidation_wins_when_both_blocks_would_fire() {
        let yaml = SAMPLE.replace(
            "invalidation:\n  - timeframe: entry\n    condition: \"close_below(stop_price)\"\n    label: lost-stop\n",
            "invalidation:\n  - timeframe: entry\n    condition: \"close_below(stop_price)\"\n    label: lost-stop\nexit:\n  all_of:\n    - timeframe: entry\n      condition: delta > 0\n      label: always\n",
        );
        let mut engine = engine(&yaml);

        // Stop above the close, so the invalidation is true.
        let position = PositionView {
            direction: Direction::Long,
            entry_price: 12.0,
            entry_time: 1200,
            stop_price: 12.5,
            take_profit_price: None,
            size: 1.0,
            bars_in_trade: 1,
            unrealized_r: 0.0,
        };

        let ctx = ctx_from(series(), Some(position));
        let signal = engine.on_candle(&ctx).expect("invalidation must fire");
        let exit = signal.as_exit().expect("an exit");
        assert_eq!(exit.trigger, ExitTrigger::Invalidation);
        assert_eq!(exit.reasons, vec!["lost-stop".to_string()]);
    }

    #[test]
    fn position_scoped_conditions_do_not_fire_while_flat() {
        // `close_below(stop_price)` with no position: stop_price is absent, so
        // the condition is false rather than true-against-zero.
        let mut engine = engine(SAMPLE);
        let ctx = ctx_from(series(), None);
        // Flat, so the entry path runs -- but the point is the invalidation
        // expression itself, which we evaluate directly.
        let view = ctx.decision().unwrap();
        let expr = Expr::parse("close_below(stop_price)").unwrap();
        let value = eval(&expr, &ctx, view, &RuntimeConfig::default()).unwrap();
        assert!(
            !value.truthy(),
            "an absent stop must not fire an invalidation"
        );
        let _ = engine.on_candle(&ctx);
    }

    #[test]
    fn crosses_above_needs_a_previous_bar() {
        let ctx = ctx_from(series(), None);
        let view = ctx.decision().unwrap();
        let expr = Expr::parse("crosses_above(close, vwap)").unwrap();
        let config = RuntimeConfig::default();
        // Either it evaluates against the previous bar or it is false; what it
        // must never do is panic or read the future.
        let _ = eval(&expr, &ctx, view, &config).unwrap();

        let single = ctx_from(vec![candle(0, 10.0, 10.0, 10.0, 1.0, 1.0)], None);
        let view = single.decision().unwrap();
        assert!(!eval(&expr, &single, view, &config).unwrap().truthy());
    }

    #[test]
    fn new_low_is_false_during_warm_up() {
        let ctx = ctx_from(series(), None);
        let view = ctx.decision().unwrap();
        let config = RuntimeConfig::default();
        // Only five bars retained, so a 20-bar low cannot be established.
        let expr = Expr::parse("new_low(20)").unwrap();
        assert!(!eval(&expr, &ctx, view, &config).unwrap().truthy());
    }

    #[test]
    fn threshold_evaluates_to_its_argument() {
        let ctx = ctx_from(series(), None);
        let view = ctx.decision().unwrap();
        let expr = Expr::parse("threshold(1500)").unwrap();
        let value = eval(&expr, &ctx, view, &RuntimeConfig::default()).unwrap();
        assert_eq!(value.as_num(), Some(1500.0));
    }

    #[test]
    fn float_equality_is_tolerant_but_not_sloppy() {
        assert!(approx_eq(0.1 + 0.2, 0.3));
        assert!(approx_eq(1.0, 1.0));
        assert!(!approx_eq(1.0, 1.01));
        assert!(!approx_eq(0.0, 1e-6));
    }

    #[test]
    fn an_atr_stop_lands_the_declared_multiple_away() {
        let yaml = DELTA_ONLY.replace(
            "\"below_sweep_low\"",
            "{kind: atr, multiple: 2.0, period: 3}",
        );
        let mut engine = engine(&yaml);

        let candles: Vec<Candle> = (0..12)
            .map(|i| {
                let base = 100.0 + f64::from(i);
                candle(
                    i64::from(i) * 300,
                    base + 2.0,
                    base - 2.0,
                    base,
                    5000.0,
                    1000.0,
                )
            })
            .collect();

        let ctx = ctx_from(candles, None);
        let reference = ctx.decision().unwrap().candle.close;
        let view = ctx.decision().unwrap();
        let expected_atr = last_atr(view, 3).expect("12 bars is plenty for ATR(3)");

        let signal = engine.on_candle(&ctx).expect("entry must fire");
        let enter = signal.as_enter().unwrap();
        assert!((enter.reference_price - reference).abs() < 1e-9);
        assert!(
            (enter.stop_price - (reference - 2.0 * expected_atr)).abs() < 1e-9,
            "stop {} vs expected {}",
            enter.stop_price,
            reference - 2.0 * expected_atr
        );
    }

    #[test]
    fn a_fixed_stop_is_used_verbatim_once_a_direction_is_given() {
        let yaml = SAMPLE
            .replace("\"below_sweep_low\"", "{kind: fixed, price: 5.0}")
            .replace("entry:\n  all_of:", "entry:\n  direction: long\n  all_of:");
        let mut engine = engine(&yaml);
        let signal = engine.on_candle(&ctx_from(series(), None)).expect("entry");
        let enter = signal.as_enter().unwrap();
        assert!((enter.stop_price - 5.0).abs() < 1e-9);
        // A fixed stop below the reference is valid, and the 2.5R target is
        // measured off the *resolved* stop distance, not off the stop rule.
        let risk = enter.reference_price - enter.stop_price;
        assert!((risk - 5.0).abs() < 1e-9);
        assert!(
            (enter.take_profit_price.unwrap() - (enter.reference_price + 2.5 * risk)).abs() < 1e-9
        );
    }

    #[test]
    fn a_short_document_resolves_its_stop_above_price() {
        let yaml = r#"
name: "short"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  all_of:
    - timeframe: entry
      condition: liquidity.swept == "buy_side"
    - timeframe: entry
      condition: sell_volume > threshold(1500)
risk:
  max_risk_pct: 1.0
  stop: above_swing_high
invalidation:
  - timeframe: entry
    condition: close_above(entry_price)
"#;
        let mut engine = engine(yaml);
        let ctx = ctx_from(short_series(), None);
        let signal = engine.on_candle(&ctx).expect("entry");
        let enter = signal.as_enter().unwrap();
        assert_eq!(enter.direction, Direction::Short);
        // The stop hangs above the swept swing high at 12 (the last confirmed
        // swing high is 13), while price closed at 10 -- a valid short stop.
        assert!(enter.stop_price > 12.0, "stop was {}", enter.stop_price);
        assert!(enter.stop_price > enter.reference_price);
        assert!(enter.stop_is_valid());
    }

    #[test]
    fn counters_track_what_the_engine_did() {
        let mut engine = engine(SAMPLE);
        let ctx = ctx_from(series(), None);
        assert_eq!(engine.candles_seen(), 0);
        let _ = engine.on_candle(&ctx);
        assert_eq!(engine.candles_seen(), 1);
        assert_eq!(engine.entries_emitted(), 1);
        assert_eq!(engine.exits_emitted(), 0);
    }
}
