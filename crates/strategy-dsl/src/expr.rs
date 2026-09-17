//! The condition language (`docs/06-STRATEGY-DSL.md`).
//!
//! Conditions are small boolean expressions over named fields exposed by
//! `analytics-core`'s `MarketState`, plus a fixed set of helper functions.
//!
//! ## Why the grammar is deliberately tiny
//!
//! The spec is explicit: "Keep the condition grammar small and explicit rather
//! than a general-purpose expression language -- every operator supported must
//! be enumerable and individually testable by the validator." So there are no
//! loops, no variables, no user-defined functions, no arithmetic beyond
//! comparisons, and no way to reference anything that is not in [`Field`] or
//! [`Func`]. Anything not in those two enums is a validation error, which is
//! what stops a hallucinated field from silently evaluating to zero.
//!
//! ## Grammar
//!
//! ```text
//! expr        := or_expr
//! or_expr     := and_expr ("or" and_expr)*
//! and_expr    := not_expr ("and" not_expr)*
//! not_expr    := "not" not_expr | comparison
//! comparison  := primary (("==" | "!=" | ">" | ">=" | "<" | "<=") primary)?
//! primary     := number | string | bool
//!              | ident "." ident ("(" args? ")")?
//!              | ident ("(" args? ")")?
//!              | "(" expr ")"
//! args        := expr ("," expr)*
//! ```
//!
//! `not` binds looser than a comparison, so `not a == b` means `not (a == b)`.
//!
//! ## Static typing
//!
//! [`Expr::type_of`] resolves every field and function to a [`Type`] before the
//! strategy ever runs. A condition that is not boolean-valued, a comparison
//! between a number and a string, a call with the wrong number of arguments, an
//! unknown field name -- all are caught here rather than at trade time. That
//! matters because the producer is usually an LLM: it must be told precisely
//! what is wrong and asked to fix it, not have the mistake discovered three
//! hours into a backtest.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A literal value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
}

impl Value {
    /// The static type of this value.
    #[must_use]
    pub const fn type_of(&self) -> Type {
        match self {
            Self::Bool(_) => Type::Bool,
            Self::Num(_) => Type::Num,
            Self::Str(_) => Type::Str,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::Num(v) => write!(f, "{v}"),
            Self::Str(v) => write!(f, "\"{v}\""),
        }
    }
}

/// The static type of an expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// A boolean.
    Bool,
    /// A number.
    Num,
    /// A string.
    Str,
}

impl Type {
    /// Human-readable name, used in error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Num => "number",
            Self::Str => "string",
        }
    }
}

/// A named field the condition language can read.
///
/// This enum **is** the vocabulary. Adding a field here is the only way to make
/// it addressable from a document, which keeps the surface auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// Current candle open.
    Open,
    /// Current candle high.
    High,
    /// Current candle low.
    Low,
    /// Current candle close.
    Close,
    /// Latest traded price (equal to `close` on the decision timeframe).
    Price,
    /// Newest candle delta.
    Delta,
    /// Cumulative volume delta over the window.
    Cvd,
    /// Session VWAP.
    Vwap,
    /// Volume profile Point of Control.
    Poc,
    /// Volume profile Value Area High.
    Vah,
    /// Volume profile Value Area Low.
    Val,
    /// Newest candle total volume.
    Volume,
    /// Newest candle buy-aggressed volume.
    BuyVolume,
    /// Newest candle sell-aggressed volume.
    SellVolume,
    /// Structural trend: `bullish`, `bearish` or `ranging`.
    Trend,
    /// CVD/price divergence: `bullish`, `bearish` or `none`.
    Divergence,
    /// Alias of [`Field::Trend`] under the spec's `market_structure` namespace.
    MarketStructureTrend,
    /// Most recent confirmed swing high.
    MarketStructureSwingHigh,
    /// Most recent confirmed swing low.
    MarketStructureSwingLow,
    /// Kind of the most recent structural break: `bos`, `choch`, or `none`.
    ///
    /// The distinction `Field::Trend` cannot make. A close above the last swing
    /// high while the trend was up is a BOS -- continuation, and the usual place
    /// to add. The same close while the trend was down is a CHoCH -- reversal
    /// evidence, and the usual place to reverse. Both leave `trend` reading
    /// `bullish`, so a structure-based condition written against `trend` alone
    /// cannot tell continuation from reversal.
    MarketStructureBreak,
    /// Direction of the most recent break: `buy`, `sell`, or `none`.
    MarketStructureBreakDirection,
    /// The swing level the most recent break closed through. Absent when
    /// nothing has broken.
    MarketStructureBreakLevel,
    /// Signed distance from the newest close to that level.
    ///
    /// A separate field because the DSL has no arithmetic: `close - level`
    /// cannot be written in a condition, so anything a strategy wants to
    /// compare must be a field. Positive means price has held the break.
    MarketStructureBreakDistance,
    /// Bars between the newest candle and the most recent break. Absent when
    /// nothing has broken; `0` means it happened on the current bar.
    ///
    /// Recency is most of what makes a break actionable. Without it every
    /// condition over [`Field::MarketStructureBreak`] also matches a CHoCH from
    /// forty bars ago that price has long since left behind.
    MarketStructureBreakAge,
    /// Whether any absorption was detected recently.
    AbsorptionDetected,
    /// Whether the most recent absorption was bullish.
    AbsorptionBullish,
    /// Whether the most recent absorption was bearish.
    AbsorptionBearish,
    /// Volume ratio of the most recent absorption.
    AbsorptionStrength,
    /// Whether any imbalance was detected recently.
    ImbalanceDetected,
    /// Whether any buy-side imbalance was detected.
    ImbalanceBuy,
    /// Whether any sell-side imbalance was detected.
    ImbalanceSell,
    /// Whether a stacked (multi-level) imbalance was detected.
    ImbalanceStacked,
    /// Signed net imbalance volume.
    ImbalanceNetVolume,
    /// Direction of the most recent sweep: `buy_side`, `sell_side` or `none`.
    LiquiditySwept,
    /// Price of the level the newest candle swept, on the side
    /// [`Field::LiquiditySwept`] reports. Absent when it swept nothing.
    ///
    /// This exists because a liquidity-sweep setup needs to *reason* about the
    /// level, not only stop on it. The canonical entry is a sweep followed by a
    /// reclaim -- price trading back above the low it just took out -- which is
    /// exactly `close > liquidity.swept_level`. Without this field the stop rule
    /// could reference the swept level while no condition could compare against
    /// it, so the setup was expressible only as "a sweep happened", which fires
    /// on bars still closing below the level. Those bars have no valid long stop,
    /// and the engine refused thousands of them.
    LiquiditySweptLevel,
    /// Price of the nearest liquidity level above.
    LiquidityNearestAbove,
    /// Price of the nearest liquidity level below.
    LiquidityNearestBelow,
    /// Stop price of the open position. Absent when flat.
    StopPrice,
    /// Entry price of the open position. Absent when flat.
    EntryPrice,
    /// Size of the open position. Absent when flat.
    PositionSize,
    /// Current open profit/loss expressed in R multiples. Absent when flat.
    UnrealizedR,
    /// Bars the position has been open. Absent when flat.
    BarsInTrade,
    /// Whether a position is currently open.
    InPosition,
}

/// Every field, for validation messages and exhaustive tests.
pub const ALL_FIELDS: &[Field] = &[
    Field::Open,
    Field::High,
    Field::Low,
    Field::Close,
    Field::Price,
    Field::Delta,
    Field::Cvd,
    Field::Vwap,
    Field::Poc,
    Field::Vah,
    Field::Val,
    Field::Volume,
    Field::BuyVolume,
    Field::SellVolume,
    Field::Trend,
    Field::Divergence,
    Field::MarketStructureTrend,
    Field::MarketStructureSwingHigh,
    Field::MarketStructureSwingLow,
    Field::MarketStructureBreak,
    Field::MarketStructureBreakDirection,
    Field::MarketStructureBreakLevel,
    Field::MarketStructureBreakDistance,
    Field::MarketStructureBreakAge,
    Field::AbsorptionDetected,
    Field::AbsorptionBullish,
    Field::AbsorptionBearish,
    Field::AbsorptionStrength,
    Field::ImbalanceDetected,
    Field::ImbalanceBuy,
    Field::ImbalanceSell,
    Field::ImbalanceStacked,
    Field::ImbalanceNetVolume,
    Field::LiquiditySwept,
    Field::LiquiditySweptLevel,
    Field::LiquidityNearestAbove,
    Field::LiquidityNearestBelow,
    Field::StopPrice,
    Field::EntryPrice,
    Field::PositionSize,
    Field::UnrealizedR,
    Field::BarsInTrade,
    Field::InPosition,
];

impl Field {
    /// Canonical name, as written in a condition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::High => "high",
            Self::Low => "low",
            Self::Close => "close",
            Self::Price => "price",
            Self::Delta => "delta",
            Self::Cvd => "cvd",
            Self::Vwap => "vwap",
            Self::Poc => "poc",
            Self::Vah => "vah",
            Self::Val => "val",
            Self::Volume => "volume",
            Self::BuyVolume => "buy_volume",
            Self::SellVolume => "sell_volume",
            Self::Trend => "trend",
            Self::Divergence => "divergence",
            Self::MarketStructureTrend => "market_structure.trend",
            Self::MarketStructureSwingHigh => "market_structure.swing_high",
            Self::MarketStructureSwingLow => "market_structure.swing_low",
            Self::MarketStructureBreak => "market_structure.break",
            Self::MarketStructureBreakDirection => "market_structure.break_direction",
            Self::MarketStructureBreakLevel => "market_structure.break_level",
            Self::MarketStructureBreakDistance => "market_structure.break_distance",
            Self::MarketStructureBreakAge => "market_structure.break_age",
            Self::AbsorptionDetected => "absorption.detected",
            Self::AbsorptionBullish => "absorption.bullish",
            Self::AbsorptionBearish => "absorption.bearish",
            Self::AbsorptionStrength => "absorption.strength",
            Self::ImbalanceDetected => "imbalance.detected",
            Self::ImbalanceBuy => "imbalance.buy",
            Self::ImbalanceSell => "imbalance.sell",
            Self::ImbalanceStacked => "imbalance.stacked",
            Self::ImbalanceNetVolume => "imbalance.net_volume",
            Self::LiquiditySwept => "liquidity.swept",
            Self::LiquiditySweptLevel => "liquidity.swept_level",
            Self::LiquidityNearestAbove => "liquidity.nearest_above",
            Self::LiquidityNearestBelow => "liquidity.nearest_below",
            Self::StopPrice => "stop_price",
            Self::EntryPrice => "entry_price",
            Self::PositionSize => "position_size",
            Self::UnrealizedR => "unrealized_r",
            Self::BarsInTrade => "bars_in_trade",
            Self::InPosition => "in_position",
        }
    }

    /// The type this field evaluates to.
    #[must_use]
    pub const fn type_of(self) -> Type {
        match self {
            Self::Trend | Self::Divergence | Self::MarketStructureTrend | Self::LiquiditySwept => {
                Type::Str
            }
            // The break's kind and direction are names, not numbers -- `bos`
            // and `choch` are compared as strings the same way `trend` is.
            Self::MarketStructureBreak | Self::MarketStructureBreakDirection => Type::Str,
            Self::AbsorptionDetected
            | Self::AbsorptionBullish
            | Self::AbsorptionBearish
            | Self::ImbalanceDetected
            | Self::ImbalanceBuy
            | Self::ImbalanceSell
            | Self::ImbalanceStacked
            | Self::InPosition => Type::Bool,
            _ => Type::Num,
        }
    }

    /// Whether this field reads position state rather than market state.
    ///
    /// Position fields are absent while flat, and any comparison against an
    /// absent value is `false` -- so a condition like `close_below(stop_price)`
    /// is simply false when there is nothing open, which is what you want.
    #[must_use]
    pub const fn is_position_scoped(self) -> bool {
        matches!(
            self,
            Self::StopPrice
                | Self::EntryPrice
                | Self::PositionSize
                | Self::UnrealizedR
                | Self::BarsInTrade
                | Self::InPosition
        )
    }

    /// Look a field up by its canonical name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        ALL_FIELDS.iter().copied().find(|f| f.name() == name)
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A helper function available in conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Func {
    /// `threshold(x)` -- marks `x` as a tunable parameter.
    ///
    /// Evaluates to `x`; it exists so the future parameter sweep has an
    /// explicit, greppable marker for what may be varied, and so a reader can
    /// see which numbers are knobs rather than physics.
    Threshold,
    /// `above(a, b)` -- `a > b`.
    Above,
    /// `below(a, b)` -- `a < b`.
    Below,
    /// `crosses_above(a, b)` -- `a > b` now and `a <= b` on the previous bar.
    CrossesAbove,
    /// `new_low()` / `new_low(n)` -- the current low is the lowest of `n` bars.
    NewLow,
    /// `new_high()` / `new_high(n)` -- the current high is the highest of `n` bars.
    NewHigh,
    /// `close_below(x)` -- `close < x`.
    CloseBelow,
    /// `close_above(x)` -- `close > x`.
    CloseAbove,
}

/// Every function, for validation messages and exhaustive tests.
pub const ALL_FUNCS: &[Func] = &[
    Func::Threshold,
    Func::Above,
    Func::Below,
    Func::CrossesAbove,
    Func::NewLow,
    Func::NewHigh,
    Func::CloseBelow,
    Func::CloseAbove,
];

impl Func {
    /// Name as written in a condition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Above => "above",
            Self::Below => "below",
            Self::CrossesAbove => "crosses_above",
            Self::NewLow => "new_low",
            Self::NewHigh => "new_high",
            Self::CloseBelow => "close_below",
            Self::CloseAbove => "close_above",
        }
    }

    /// Inclusive `(min, max)` argument count.
    #[must_use]
    pub const fn arity(self) -> (usize, usize) {
        match self {
            Self::Threshold | Self::CloseBelow | Self::CloseAbove => (1, 1),
            Self::Above | Self::Below | Self::CrossesAbove => (2, 2),
            Self::NewLow | Self::NewHigh => (0, 1),
        }
    }

    /// What this function evaluates to.
    #[must_use]
    pub const fn return_type(self) -> Type {
        match self {
            Self::Threshold => Type::Num,
            _ => Type::Bool,
        }
    }

    /// The type each argument must have, ignoring optional trailing arguments.
    #[must_use]
    pub const fn param_types(self) -> &'static [Type] {
        match self {
            Self::Threshold | Self::CloseBelow | Self::CloseAbove => &[Type::Num],
            Self::Above | Self::Below | Self::CrossesAbove => &[Type::Num, Type::Num],
            // Optional lookback, if supplied, is a number.
            Self::NewLow | Self::NewHigh => &[Type::Num],
        }
    }

    /// Look a function up by name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        ALL_FUNCS.iter().copied().find(|f| f.name() == name)
    }
}

impl fmt::Display for Func {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

impl CompareOp {
    /// Every operator, for validation messages and exhaustive tests.
    pub const ALL: &'static [Self] = &[Self::Eq, Self::Ne, Self::Gt, Self::Ge, Self::Lt, Self::Le];

    /// The operator as written.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    /// Whether this is an ordering comparison, which only numbers support.
    #[must_use]
    pub const fn is_ordering(self) -> bool {
        !matches!(self, Self::Eq | Self::Ne)
    }
}

impl fmt::Display for CompareOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

/// A property of a concept the document declared.
///
/// This is the **whole** surface a condition can read off a concept: five names,
/// enumerable and individually testable, exactly as `docs/06` requires of every
/// operator. It is deliberately not "any field of any region" -- the vocabulary
/// stays closed even though the concepts themselves do not, which is what lets
/// the validator be exhaustive about it.
///
/// ## Which band these describe
///
/// The concept's **newest** one -- the most recently formed. A concept is a
/// pattern, not a detector with a notion of "the" gap: any three candles that
/// separate is a fair value gap, so one impulse can leave several bands and
/// "the band" has to be pinned down. The newest is deterministic, cheap, and
/// what a trader reading left to right is looking at.
///
/// This is a real limitation and not a hidden one: a setup that wants "any
/// fresh band anywhere" cannot yet say so. It says "the newest one is fresh",
/// which on a chart is the same thing most of the time and is never a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConceptPart {
    /// Whether the concept found any band at all.
    Exists,
    /// Whether the newest band is untouched -- price has not traded back into
    /// it. The distinction the whole concept rests on: a mitigated band has
    /// already been consumed, and trading one is trading a level that is gone.
    Fresh,
    /// How much of the newest band price has traded back through, `0.0..=1.0`.
    /// `0.0` is fresh, `1.0` means price has been through the whole band.
    Mitigated,
    /// The newest band's dearer edge. Absent when the concept found nothing.
    Top,
    /// The newest band's cheaper edge. Absent when the concept found nothing.
    Bottom,
}

impl ConceptPart {
    /// Every part, for validation messages and exhaustive tests.
    pub const ALL: [Self; 5] = [
        Self::Exists,
        Self::Fresh,
        Self::Mitigated,
        Self::Top,
        Self::Bottom,
    ];

    /// Canonical name, as written in a condition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::Fresh => "fresh",
            Self::Mitigated => "mitigated",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    /// The type this part evaluates to.
    #[must_use]
    pub const fn type_of(self) -> Type {
        match self {
            Self::Exists | Self::Fresh => Type::Bool,
            Self::Mitigated | Self::Top | Self::Bottom => Type::Num,
        }
    }

    /// One line saying what this part reads.
    ///
    /// Lives here rather than in either consumer -- the agent's prompt and the
    /// gateway's schema -- because those two must not be able to disagree about
    /// what `fresh` means. Neither is authoritative; this is. It is also what
    /// makes "every part is described exactly once" a testable claim instead of
    /// a hopeful one.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Exists => "the concept found a band at all",
            Self::Fresh => "the newest band has not been traded back into",
            Self::Mitigated => {
                "how much of the newest band price has traded back through, 0.0..=1.0"
            }
            Self::Top => "the newest band's dearer edge",
            Self::Bottom => "the newest band's cheaper edge",
        }
    }

    /// Look a part up by its canonical name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.name() == name)
    }
}

impl fmt::Display for ConceptPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A parsed condition expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A literal.
    Literal(Value),
    /// A field read.
    Field(Field),
    /// A read of a property of a concept the document declared.
    ///
    /// Its own node rather than a [`Field`] variant, because the name is
    /// whatever the document called it. A `String` inside `Field` would cost
    /// `Copy`, would stop [`ALL_FIELDS`] being a plain slice of a closed set,
    /// and would force the name to be resolved at parse time -- before the
    /// document that declares the concept has even been read. As a node, `Field`
    /// stays closed and the *validator* checks the name against the document's
    /// `concepts` block, which is the same shape as "a condition's timeframe
    /// must be declared".
    Concept {
        /// The concept's name, as declared in the document.
        name: String,
        /// Which property of it to read.
        part: ConceptPart,
    },
    /// A function call.
    Call {
        /// Which function.
        func: Func,
        /// Arguments, already type-checked by [`Expr::type_of`].
        args: Vec<Expr>,
    },
    /// Logical negation.
    Not(Box<Expr>),
    /// Logical conjunction.
    And(Box<Expr>, Box<Expr>),
    /// Logical disjunction.
    Or(Box<Expr>, Box<Expr>),
    /// A comparison.
    Compare {
        /// The operator.
        op: CompareOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
}

/// A parse or type error, with the position in the source where it occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    /// What went wrong.
    pub message: String,
    /// Zero-based character offset into the source.
    pub position: usize,
}

impl ExprError {
    /// Construct an error.
    #[must_use]
    pub fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }

    /// One-based column, for a message a human or an LLM can act on.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.position + 1
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at column {})", self.message, self.column())
    }
}

impl Expr {
    /// The static type of this expression, or the first problem found.
    ///
    /// Checks field/function existence, arity and operand types. Every check
    /// here corresponds to a rejection case the validator reports.
    pub fn type_of(&self) -> Result<Type, ExprError> {
        match self {
            Self::Literal(v) => Ok(v.type_of()),
            Self::Field(f) => Ok(f.type_of()),
            Self::Concept { part, .. } => Ok(part.type_of()),
            Self::Call { func, args } => {
                let (min, max) = func.arity();
                if args.len() < min || args.len() > max {
                    let expected = if min == max {
                        format!("{min}")
                    } else {
                        format!("{min} or {max}")
                    };
                    return Err(ExprError::new(
                        format!(
                            "`{}` expects {expected} argument(s), got {}",
                            func.name(),
                            args.len()
                        ),
                        0,
                    ));
                }

                let params = func.param_types();
                for (i, arg) in args.iter().enumerate() {
                    let got = arg.type_of()?;
                    // Optional trailing arguments share the last declared type.
                    let Some(expected) = params.get(i).or_else(|| params.last()) else {
                        continue;
                    };
                    if got != *expected {
                        return Err(ExprError::new(
                            format!(
                                "`{}` argument {} must be {}, got {}",
                                func.name(),
                                i + 1,
                                expected.name(),
                                got.name()
                            ),
                            0,
                        ));
                    }
                }

                Ok(func.return_type())
            }
            Self::Not(inner) => {
                let t = inner.type_of()?;
                if t != Type::Bool {
                    return Err(ExprError::new(
                        format!("`not` requires bool, got {}", t.name()),
                        0,
                    ));
                }
                Ok(Type::Bool)
            }
            Self::And(a, b) | Self::Or(a, b) => {
                for side in [a, b] {
                    let t = side.type_of()?;
                    if t != Type::Bool {
                        return Err(ExprError::new(
                            format!("logical operators require bool, got {}", t.name()),
                            0,
                        ));
                    }
                }
                Ok(Type::Bool)
            }
            Self::Compare { op, lhs, rhs } => {
                let l = lhs.type_of()?;
                let r = rhs.type_of()?;
                if op.is_ordering() {
                    if l != Type::Num || r != Type::Num {
                        return Err(ExprError::new(
                            format!(
                                "`{op}` compares numbers only, got {} and {}",
                                l.name(),
                                r.name()
                            ),
                            0,
                        ));
                    }
                } else if l != r {
                    return Err(ExprError::new(
                        format!("`{op}` compares {} with {}", l.name(), r.name()),
                        0,
                    ));
                }
                Ok(Type::Bool)
            }
        }
    }

    /// Parse a condition source into an expression tree.
    pub fn parse(source: &str) -> Result<Self, ExprError> {
        let tokens = tokenize(source)?;
        let mut parser = Parser {
            tokens: &tokens,
            position: 0,
            source_len: source.len(),
        };
        let expr = parser.parse_or()?;

        if let Some(token) = parser.peek() {
            return Err(ExprError::new(
                format!("unexpected `{}` after end of expression", token.describe()),
                token.position(),
            ));
        }

        Ok(expr)
    }

    /// Parse and type-check in one step.
    pub fn parse_checked(source: &str) -> Result<Self, ExprError> {
        let expr = Self::parse(source)?;
        let ty = expr.type_of()?;
        if ty != Type::Bool {
            return Err(ExprError::new(
                format!(
                    "a condition must be boolean, but this evaluates to {}",
                    ty.name()
                ),
                0,
            ));
        }
        Ok(expr)
    }
}

/// Split the `concepts.<name>.<part>` tail into the two halves the AST wants.
///
/// `rsplit_once` rather than `split_once` because the *part* is the last
/// segment, and it is the only one that could be confused with the name. In
/// practice the distinction does not arise: `analytics_core::concepts::validate`
/// allows only lowercase letters, digits and underscores in a name, so a name
/// containing a dot could never have been declared. Splitting on the last dot
/// means the error for `concepts.gap.top.extra` names `extra` as the bad
/// property instead of blaming the concept.
fn parse_concept_ref(rest: &str, whole: &str, start: usize) -> Result<Expr, ExprError> {
    let Some((name, part)) = rest.rsplit_once('.') else {
        return Err(ExprError::new(
            format!(
                "`{whole}` names a concept but reads nothing from it; write `{whole}.{}`",
                ConceptPart::Fresh
            ),
            start,
        ));
    };
    if name.is_empty() {
        return Err(ExprError::new(format!("`{whole}` names no concept"), start));
    }
    let Some(part) = ConceptPart::parse(part) else {
        let known: Vec<&str> = ConceptPart::ALL.iter().map(|p| p.name()).collect();
        return Err(ExprError::new(
            format!(
                "unknown concept property `{part}` in `{whole}`; expected one of {}",
                known.join(", ")
            ),
            start,
        ));
    };
    Ok(Expr::Concept {
        name: name.to_owned(),
        part,
    })
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(v) => write!(f, "{v}"),
            Self::Field(field) => write!(f, "{field}"),
            // Round-trips through the parser, so a condition read back out of the
            // AST is the condition that went in.
            Self::Concept { name, part } => write!(f, "concepts.{name}.{part}"),
            Self::Call { func, args } => {
                write!(f, "{func}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                f.write_str(")")
            }
            Self::Not(inner) => write!(f, "not {inner}"),
            Self::And(a, b) => write!(f, "({a} and {b})"),
            Self::Or(a, b) => write!(f, "({a} or {b})"),
            Self::Compare { op, lhs, rhs } => write!(f, "{lhs} {op} {rhs}"),
        }
    }
}

/// A lexical token.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Number(f64),
    Str(String),
    Bool(bool),
    Op(CompareOp),
    LParen,
    RParen,
    Comma,
    And,
    Or,
    Not,
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Ident(s) => s.clone(),
            Self::Number(n) => n.to_string(),
            Self::Str(s) => format!("\"{s}\""),
            Self::Bool(b) => b.to_string(),
            Self::Op(op) => op.symbol().to_string(),
            Self::LParen => "(".to_string(),
            Self::RParen => ")".to_string(),
            Self::Comma => ",".to_string(),
            Self::And => "and".to_string(),
            Self::Or => "or".to_string(),
            Self::Not => "not".to_string(),
        }
    }
}

/// A token with the offset it started at.
#[derive(Debug, Clone, PartialEq)]
struct Spanned {
    token: Token,
    start: usize,
}

impl Spanned {
    fn position(&self) -> usize {
        self.start
    }

    fn describe(&self) -> String {
        self.token.describe()
    }
}

/// Split source into tokens.
fn tokenize(source: &str) -> Result<Vec<Spanned>, ExprError> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        let start = i;

        // Numbers, including a leading sign when it is unambiguously a number
        // (i.e. not a binary minus, which this grammar does not have anyway).
        if c.is_ascii_digit()
            || (c == '-'
                && chars
                    .get(i + 1)
                    .is_some_and(|n| n.is_ascii_digit() || *n == '.'))
        {
            let mut text = String::new();
            if c == '-' {
                text.push('-');
                i += 1;
            }
            let mut seen_dot = false;
            while i < chars.len() {
                let d = chars[i];
                if d.is_ascii_digit() {
                    text.push(d);
                    i += 1;
                } else if d == '.' && !seen_dot {
                    seen_dot = true;
                    text.push(d);
                    i += 1;
                } else {
                    break;
                }
            }
            let value: f64 = text
                .parse()
                .map_err(|_| ExprError::new(format!("invalid number `{text}`"), start))?;
            tokens.push(Spanned {
                token: Token::Number(value),
                start,
            });
            continue;
        }

        // String literals, single or double quoted.
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let mut text = String::new();
            let mut closed = false;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' && i + 1 < chars.len() {
                    text.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if d == quote {
                    closed = true;
                    i += 1;
                    break;
                }
                text.push(d);
                i += 1;
            }
            if !closed {
                return Err(ExprError::new("unterminated string literal", start));
            }
            tokens.push(Spanned {
                token: Token::Str(text),
                start,
            });
            continue;
        }

        // Identifiers, keywords, and dotted field paths.
        if c.is_alphabetic() || c == '_' {
            let mut text = String::new();
            while i < chars.len() {
                let d = chars[i];
                if d.is_alphanumeric() || d == '_' || d == '.' {
                    text.push(d);
                    i += 1;
                } else {
                    break;
                }
            }
            let token = match text.as_str() {
                "and" => Token::And,
                "or" => Token::Or,
                "not" => Token::Not,
                "true" => Token::Bool(true),
                "false" => Token::Bool(false),
                _ => Token::Ident(text),
            };
            tokens.push(Spanned { token, start });
            continue;
        }

        // Operators and punctuation.
        let (token, width) = match c {
            '(' => (Token::LParen, 1),
            ')' => (Token::RParen, 1),
            ',' => (Token::Comma, 1),
            '=' => {
                if chars.get(i + 1) == Some(&'=') {
                    (Token::Op(CompareOp::Eq), 2)
                } else {
                    return Err(ExprError::new("use `==` for comparison, not `=`", start));
                }
            }
            '!' => {
                if chars.get(i + 1) == Some(&'=') {
                    (Token::Op(CompareOp::Ne), 2)
                } else {
                    return Err(ExprError::new("expected `!=`", start));
                }
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    (Token::Op(CompareOp::Ge), 2)
                } else {
                    (Token::Op(CompareOp::Gt), 1)
                }
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    (Token::Op(CompareOp::Le), 2)
                } else {
                    (Token::Op(CompareOp::Lt), 1)
                }
            }
            other => {
                return Err(ExprError::new(
                    format!("unexpected character `{other}`"),
                    start,
                ))
            }
        };
        tokens.push(Spanned { token, start });
        i += width;
    }

    Ok(tokens)
}

/// Recursive-descent parser over the token stream.
struct Parser<'a> {
    tokens: &'a [Spanned],
    position: usize,
    source_len: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.position)
    }

    fn advance(&mut self) -> Option<&Spanned> {
        let token = self.tokens.get(self.position);
        if token.is_some() {
            self.position += 1;
        }
        token
    }

    fn eat(&mut self, predicate: impl Fn(&Token) -> bool) -> bool {
        if self.peek().is_some_and(|t| predicate(&t.token)) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    /// Position to blame when the stream ends unexpectedly.
    fn end_position(&self) -> usize {
        self.tokens
            .last()
            .map_or(0, |t| t.start + t.describe().len())
            .min(self.source_len)
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_and()?;
        while self.eat(|t| matches!(t, Token::Or)) {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_not()?;
        while self.eat(|t| matches!(t, Token::And)) {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, ExprError> {
        if self.eat(|t| matches!(t, Token::Not)) {
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, ExprError> {
        let lhs = self.parse_primary()?;

        let op = match self.peek().map(|t| &t.token) {
            Some(Token::Op(op)) => *op,
            _ => return Ok(lhs),
        };
        self.position += 1;

        let rhs = self.parse_primary()?;
        Ok(Expr::Compare {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        let Some(spanned) = self.advance().cloned() else {
            return Err(ExprError::new(
                "unexpected end of expression",
                self.end_position(),
            ));
        };
        let start = spanned.start;

        match spanned.token {
            Token::Number(n) => Ok(Expr::Literal(Value::Num(n))),
            Token::Str(s) => Ok(Expr::Literal(Value::Str(s))),
            Token::Bool(b) => Ok(Expr::Literal(Value::Bool(b))),
            Token::Ident(name) => {
                if self.eat(|t| matches!(t, Token::LParen)) {
                    let args = self.parse_args()?;
                    let Some(func) = Func::parse(&name) else {
                        return Err(ExprError::new(format!("unknown function `{name}`"), start));
                    };
                    return Ok(Expr::Call { func, args });
                }
                // `concepts.<name>.<part>` is checked before the field lookup,
                // so a typo in a concept property is reported as a bad property
                // rather than as "unknown field `concepts.gap.frsh`" -- the
                // message a model gets back is what it corrects from.
                if let Some(rest) = name.strip_prefix("concepts.") {
                    return parse_concept_ref(rest, &name, start);
                }
                let Some(field) = Field::parse(&name) else {
                    return Err(ExprError::new(format!("unknown field `{name}`"), start));
                };
                Ok(Expr::Field(field))
            }
            Token::LParen => {
                let inner = self.parse_or()?;
                if !self.eat(|t| matches!(t, Token::RParen)) {
                    return Err(ExprError::new("expected `)`", self.end_position()));
                }
                Ok(inner)
            }
            other => Err(ExprError::new(
                format!("unexpected `{}`", other.describe()),
                start,
            )),
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, ExprError> {
        let mut args = Vec::new();
        if self.eat(|t| matches!(t, Token::RParen)) {
            return Ok(args);
        }
        loop {
            args.push(self.parse_or()?);
            if self.eat(|t| matches!(t, Token::Comma)) {
                continue;
            }
            if self.eat(|t| matches!(t, Token::RParen)) {
                return Ok(args);
            }
            return Err(ExprError::new("expected `,` or `)`", self.end_position()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Expr {
        Expr::parse(src).unwrap_or_else(|e| panic!("failed to parse `{src}`: {e}"))
    }

    #[test]
    fn parses_the_documented_examples() {
        for src in [
            "market_structure.trend == \"bullish\"",
            "absorption.detected == true",
            "liquidity.swept == \"sell_side\"",
            "delta > threshold(1500)",
            "close_below(stop_price)",
        ] {
            let expr = parse(src);
            assert_eq!(expr.type_of(), Ok(Type::Bool), "for `{src}`");
        }
    }

    #[test]
    fn the_specs_invalidation_condition_is_expressible() {
        // `docs/06-STRATEGY-DSL.md` writes `close_below(stop_price)` in its own
        // sample. `MarketState` alone cannot express that -- it knows nothing
        // about an open position -- so `stop_price` (and the other
        // position-scoped fields) were added to the vocabulary to make the
        // documented invalidation implementable. Recorded as a deviation.
        let expr = parse("close_below(stop_price)");
        assert_eq!(expr.type_of(), Ok(Type::Bool));
        assert!(Field::StopPrice.is_position_scoped());
    }

    #[test]
    fn an_unknown_field_names_the_bad_token() {
        // The other half of the same guarantee: a field that genuinely does not
        // exist must be refused, never silently evaluated as zero.
        let err = Expr::parse("close_below(swing_low)").unwrap_err();
        assert!(err.message.contains("swing_low"), "{err}");
    }

    #[test]
    fn operator_precedence_matches_the_documented_grammar() {
        // or is loosest, then and, then not, then comparison.
        let expr = parse("delta == 1 or not volume > 2 and cvd > 3");
        assert!(matches!(expr, Expr::Or(_, _)));
    }

    #[test]
    fn not_binds_looser_than_comparison() {
        // `not delta > 2` must parse as `not (delta > 2)`.
        let expr = parse("not delta > 2");
        match expr {
            Expr::Not(inner) => assert!(matches!(*inner, Expr::Compare { .. })),
            other => panic!("expected Not, got {other:?}"),
        }
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let expr = parse("delta > 1 or delta > 2 and delta > 3");
        match expr {
            Expr::Or(_, right) => assert!(matches!(*right, Expr::And(_, _))),
            other => panic!("expected Or at the root, got {other:?}"),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        let expr = parse("(delta > 1 or delta > 2) and delta > 3");
        assert!(matches!(expr, Expr::And(_, _)));
    }

    #[test]
    fn string_literals_accept_both_quote_styles() {
        assert_eq!(parse("trend == \"bullish\""), parse("trend == 'bullish'"));
    }

    #[test]
    fn negative_and_decimal_numbers_parse() {
        assert_eq!(parse("delta > -1.5"), {
            Expr::Compare {
                op: CompareOp::Gt,
                lhs: Box::new(Expr::Field(Field::Delta)),
                rhs: Box::new(Expr::Literal(Value::Num(-1.5))),
            }
        });
    }

    #[test]
    fn single_equals_is_rejected_with_a_helpful_message() {
        let err = Expr::parse("delta = 1").unwrap_err();
        assert!(err.message.contains("`==`"), "{err}");
        assert_eq!(err.column(), 7);
    }

    #[test]
    fn unterminated_string_is_rejected() {
        let err = Expr::parse("trend == \"bullish").unwrap_err();
        assert!(err.message.contains("unterminated"), "{err}");
    }

    #[test]
    fn trailing_junk_is_rejected() {
        let err = Expr::parse("delta > 1 volume > 2").unwrap_err();
        assert!(err.message.contains("unexpected"), "{err}");
    }

    #[test]
    fn unknown_function_lists_nothing_but_names_the_token() {
        let err = Expr::parse("crosses_under(delta, 1)").unwrap_err();
        assert!(err.message.contains("crosses_under"), "{err}");
    }

    #[test]
    fn wrong_arity_is_rejected() {
        let err = Expr::parse("threshold(1, 2)")
            .unwrap()
            .type_of()
            .unwrap_err();
        assert!(err.message.contains("expects 1 argument"), "{err}");
    }

    #[test]
    fn new_low_accepts_zero_or_one_argument() {
        assert_eq!(parse("new_low()").type_of(), Ok(Type::Bool));
        assert_eq!(parse("new_low(20)").type_of(), Ok(Type::Bool));
        assert!(parse("new_low(1, 2)").type_of().is_err());
    }

    #[test]
    fn ordering_comparison_rejects_strings() {
        let err = parse("trend > 1").type_of().unwrap_err();
        assert!(err.message.contains("numbers only"), "{err}");
    }

    #[test]
    fn equality_between_mismatched_types_is_rejected() {
        let err = parse("trend == 1").type_of().unwrap_err();
        assert!(err.message.contains("string with number"), "{err}");
    }

    #[test]
    fn logical_operators_reject_non_booleans() {
        let err = parse("delta and volume").type_of().unwrap_err();
        assert!(err.message.contains("bool"), "{err}");
    }

    #[test]
    fn not_rejects_non_booleans() {
        let err = parse("not delta").type_of().unwrap_err();
        assert!(err.message.contains("bool"), "{err}");
    }

    #[test]
    fn a_non_boolean_condition_is_rejected_at_the_top_level() {
        let err = Expr::parse_checked("delta + 1").unwrap_err();
        assert!(!err.message.is_empty());
        let err = Expr::parse_checked("delta").unwrap_err();
        assert!(err.message.contains("must be boolean"), "{err}");
    }

    #[test]
    fn parse_checked_accepts_a_boolean_expression() {
        assert!(Expr::parse_checked("delta > 100").is_ok());
        assert!(Expr::parse_checked("absorption.detected").is_ok());
    }

    #[test]
    fn every_field_name_parses_back_to_itself() {
        for field in ALL_FIELDS {
            assert_eq!(Field::parse(field.name()), Some(*field), "{}", field.name());
        }
    }

    #[test]
    fn every_function_name_parses_back_to_itself() {
        for func in ALL_FUNCS {
            assert_eq!(Func::parse(func.name()), Some(*func), "{}", func.name());
        }
    }

    #[test]
    fn every_field_has_a_stable_type() {
        // Guards against a field being added with a type that makes the
        // documented examples unrepresentable.
        for field in ALL_FIELDS {
            let ty = field.type_of();
            assert!(matches!(ty, Type::Bool | Type::Num | Type::Str));
        }
    }

    // --- concepts a document declared ---------------------------------------

    #[test]
    fn a_concept_reference_parses_into_its_own_node() {
        // The point of the node: the *name* is whatever the document called it,
        // so it cannot live in the closed `Field` enum. What stays closed is the
        // part -- five names, all of them known here.
        assert_eq!(
            parse("concepts.bullish_gap.fresh"),
            Expr::Concept {
                name: "bullish_gap".into(),
                part: ConceptPart::Fresh,
            }
        );
        assert_eq!(
            parse("close > concepts.gap.top"),
            Expr::Compare {
                op: CompareOp::Gt,
                lhs: Box::new(Expr::Field(Field::Close)),
                rhs: Box::new(Expr::Concept {
                    name: "gap".into(),
                    part: ConceptPart::Top,
                }),
            }
        );
    }

    #[test]
    fn every_concept_part_has_a_stable_name_and_type() {
        for part in ConceptPart::ALL {
            assert_eq!(
                ConceptPart::parse(part.name()),
                Some(part),
                "{}",
                part.name()
            );
            assert!(
                matches!(part.type_of(), Type::Bool | Type::Num),
                "{} is neither a predicate nor a number",
                part.name()
            );
        }
        // A part whose type a condition could not use would be a part that
        // exists only in this enum.
        assert_eq!(ConceptPart::Fresh.type_of(), Type::Bool);
        assert_eq!(ConceptPart::Exists.type_of(), Type::Bool);
        assert_eq!(ConceptPart::Top.type_of(), Type::Num);
    }

    #[test]
    fn a_concept_name_is_not_validated_by_the_parser() {
        // Deliberately: whether `no_such_concept` was declared is a fact about
        // the *document*, which the parser has never seen. Parsing must accept
        // it so the validator can report it against the condition that names it,
        // the same way an undeclared timeframe is handled.
        let expr = parse("concepts.no_such_concept.fresh");
        assert_eq!(expr.type_of(), Ok(Type::Bool));
    }

    #[test]
    fn a_concept_reference_without_a_property_says_what_to_write() {
        let err = Expr::parse("concepts.bullish_gap").unwrap_err();
        assert!(err.message.contains("names a concept"), "{err}");
        assert!(
            err.message.contains("fresh"),
            "it should name a valid part: {err}"
        );
    }

    #[test]
    fn an_unknown_concept_property_lists_the_known_ones() {
        // The message is what a model corrects from, so it has to be actionable
        // rather than merely accurate.
        let err = Expr::parse("concepts.bullish_gap.frsh").unwrap_err();
        assert!(
            err.message.contains("unknown concept property `frsh`"),
            "{err}"
        );
        for part in ConceptPart::ALL {
            assert!(
                err.message.contains(part.name()),
                "`{}` should be offered as a valid property: {err}",
                part.name()
            );
        }
    }

    #[test]
    fn a_dotted_concept_property_blames_the_property_not_the_concept() {
        // Splitting on the *last* dot is what makes this read correctly. Split on
        // the first and the message would blame a concept called `gap.top`.
        let err = Expr::parse("concepts.gap.top.extra").unwrap_err();
        assert!(err.message.contains("`extra`"), "{err}");
    }

    #[test]
    fn a_concept_property_is_not_mistaken_for_a_field() {
        // `concepts` is not in `ALL_FIELDS`, so without the branch in the parser
        // this would be "unknown field `concepts.bullish_gap.fresh`" -- accurate
        // and useless.
        let err = Expr::parse("concepts").unwrap_err();
        assert!(err.message.contains("unknown field"), "{err}");
        // `concepts.` names a concept that is empty *and* reads nothing from it;
        // the second is the more useful thing to say.
        let err = Expr::parse("concepts.").unwrap_err();
        assert!(err.message.contains("reads nothing from it"), "{err}");
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        let sources = [
            "delta > threshold(1500)",
            "not (trend == \"bullish\")",
            "absorption.detected and imbalance.stacked",
            "liquidity.swept == \"sell_side\" or new_low()",
            "close_below(vwap)",
            "concepts.bullish_gap.fresh",
            "close > concepts.bullish_gap.top",
        ];
        for src in sources {
            let expr = parse(src);
            let rendered = expr.to_string();
            let reparsed = parse(&rendered);
            assert_eq!(
                expr, reparsed,
                "round trip failed for `{src}` -> `{rendered}`"
            );
        }
    }
}
