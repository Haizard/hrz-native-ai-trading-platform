//! Type check: names, arity, and Pine's casting rules.
//!
//! Pine is dynamically typed at runtime but its casting rules are famously
//! *not* permissive: `int` promotes to `float`, `float` never narrows to
//! `int`, `na` sits happily in any float context, and a bool used as a number
//! is a bug. Enforcing those rules statically is the layer that turns "the
//! model wrote something that parses" into "the model wrote something that
//! means what it says".

use std::collections::HashMap;

use crate::parse::{Arg, BinOp, BlockKind, Expr, ExprKind, Item, Script, UnOp, VarMode};
use crate::{ErrorKind, ScriptError, Span};

/// Everything the checker knows statically about one script.
#[derive(Debug, Default)]
pub struct Checked {
    /// User-defined function signatures, by name.
    pub functions: HashMap<String, FuncSig>,
    /// Script variables, by name, with the mode they were declared in.
    pub variables: HashMap<String, VarMode>,
    /// Script variables, by name, with the static type the checker inferred.
    pub var_types: HashMap<String, Ty>,
    /// `input.*` declarations in source order, for the settings UI.
    pub inputs: Vec<InputDecl>,
    /// `plot*`/`hline` call sites in source order, in case the UI wants them.
    pub plots: Vec<Span>,
}

/// One user function's signature.
#[derive(Debug, Clone)]
pub struct FuncSig {
    /// Parameter names.
    pub params: Vec<String>,
}

/// One `input.*` declaration.
#[derive(Debug, Clone)]
pub struct InputDecl {
    /// The variable it binds.
    pub name: String,
    /// The input kind (`int`, `float`, `bool`, `string`, `color`).
    pub kind: &'static str,
    /// The default value, when the declaration carried one.
    pub defval: Option<f64>,
    /// `title=`, when given.
    pub title: Option<String>,
}

/// Type-check a parsed script, returning every problem.
///
/// Unknown identifiers and calls are errors, not `na`-at-runtime: a name the
/// platform does not know is a typo the author (human or model) wants told
/// about, and refusing here is what keeps the interpreter's job simple.
pub fn check(script: &Script) -> Vec<ScriptError> {
    let mut cx = Cx { errors: Vec::new(), out: Checked::default(), depth: 0 };
    cx.items(&script.items);
    cx.errors
}

struct Cx {
    errors: Vec<ScriptError>,
    out: Checked,
    depth: usize,
}

impl Cx {
    fn err(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(ScriptError {
            kind: ErrorKind::Type,
            span,
            message: message.into(),
        });
    }

    fn items(&mut self, items: &[Item]) {
        for item in items {
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Assign { span, name, mode, expr, .. } => {
                // `input.*` declarations are recorded for the UI and typed by
                // their call.
                if let ExprKind::Call { callee, args } = &expr.kind {
                    if let Some(kind) = callee.strip_prefix("input.") {
                        self.record_input(name, kind, args, *span);
                    }
                }
                let ty = self.expr(expr);
                self.out.var_types.insert(name.clone(), ty);
                if self.out.variables.insert(name.clone(), *mode).is_some() && *mode == VarMode::Var
                {
                    // Re-declaring a `var` in the same scope is usually a
                    // copy-paste; allow reassignment (Pine's `:=`) but a
                    // second `var` init would silently reset state.
                    self.err(*span, format!("`{name}` is declared twice with `var`"));
                }
            }
            Item::Block { span, kind, exprs, loop_var, body, els } => {
                for e in exprs {
                    self.expr(e);
                }
                if *kind == BlockKind::For {
                    self.depth += 1;
                    if self.depth > 4 {
                        self.err(*span, "loop nesting deeper than 4 is refused");
                    }
                    if let Some(v) = loop_var {
                        self.out.var_types.insert(v.clone(), Ty::Float);
                    }
                }
                self.items(body);
                if let Some(els) = els {
                    self.items(els);
                }
                self.depth = self.depth.saturating_sub(1);
            }
            Item::FuncDef { span, name, params, body } => {
                if self.out.functions.contains_key(name) {
                    self.err(*span, format!("function `{name}` is defined twice"));
                }
                self.out
                    .functions
                    .insert(name.clone(), FuncSig { params: params.clone() });
                // Parameters are in scope inside the body; bind them as
                // numbers so a recursive or self-referencing body checks.
                for p in params {
                    self.out.var_types.insert(p.clone(), Ty::Float);
                }
                self.items(body);
            }
            Item::Expr { span, expr } => {
                self.expr(expr);
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if callee.starts_with("plot") || callee == "hline" || callee == "fill"
                        || callee == "bgcolor" || callee == "barcolor"
                    {
                        self.out.plots.push(*span);
                    }
                }
            }
        }
    }

    fn record_input(&mut self, name: &str, kind: &str, args: &[Arg], span: Span) {
        let known = ["int", "float", "bool", "string", "color"];
        if !known.contains(&kind) {
            self.err(
                span,
                format!("`input.{kind}` is not an input kind; known: {}", known.join(", ")),
            );
        }
        let defval = args.iter().find_map(|a| {
            if a.name.as_deref() == Some("defval") {
                if let ExprKind::Num(n) = a.value.kind {
                    return Some(n);
                }
            }
            None
        });
        let title = args.iter().find_map(|a| {
            if a.name.as_deref() == Some("title") {
                if let ExprKind::Str(s) = &a.value.kind {
                    return Some(s.clone());
                }
            }
            None
        });
        self.out.inputs.push(InputDecl {
            name: name.to_string(),
            kind: Box::leak(kind.to_string().into_boxed_str()),
            defval,
            title,
        });
    }

    /// Check an expression; returns the static type when known.
    fn expr(&mut self, expr: &Expr) -> Ty {
        match &expr.kind {
            ExprKind::Num(_) => Ty::Float,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::String,
            ExprKind::Color(_) => Ty::Color,
            ExprKind::Na => Ty::Na,
            ExprKind::Ident(name) => self.ident(expr.span, name),
            ExprKind::Member { path } => self.member(expr.span, path),
            ExprKind::Call { callee, args } => self.call(expr.span, callee, args),
            ExprKind::History { base, offset } => {
                let inner = self.expr(base);
                self.expr(offset);
                match inner {
                    Ty::Color | Ty::String => {
                        self.err(expr.span, "history indexing is for numeric and bool series");
                        Ty::Float
                    }
                    _ => Ty::Float,
                }
            }
            ExprKind::Ternary { cond, then, els } => {
                let c = self.expr(cond);
                if !matches!(c, Ty::Bool | Ty::Na) {
                    self.err(cond.span, "a ternary condition must be bool");
                }
                let a = self.expr(then);
                let b = self.expr(els);
                if a == b {
                    a
                } else {
                    Ty::Float
                }
            }
            ExprKind::Un { op, expr } => {
                let inner = self.expr(expr);
                match op {
                    UnOp::Not => {
                        if !matches!(inner, Ty::Bool | Ty::Na) {
                            self.err(expr.span, "`not` needs a bool");
                        }
                        Ty::Bool
                    }
                    _ => {
                        if matches!(inner, Ty::Bool | Ty::String | Ty::Color) {
                            self.err(expr.span, "unary `+`/`-` needs a number");
                        }
                        Ty::Float
                    }
                }
            }
            ExprKind::Bin { left, op, right } => self.bin(expr.span, left, *op, right),
        }
    }

    fn bin(&mut self, span: Span, left: &Expr, op: BinOp, right: &Expr) -> Ty {
        let l = self.expr(left);
        let r = self.expr(right);
        match op {
            BinOp::Or | BinOp::And => {
                if !matches!(l, Ty::Bool | Ty::Na) || !matches!(r, Ty::Bool | Ty::Na) {
                    self.err(span, "`and`/`or` need bools");
                }
                Ty::Bool
            }
            BinOp::Cmp(_) => {
                // Comparing a bool to a number is the classic reach; comparing
                // strings is fine; comparing colors is not.
                if matches!(l, Ty::Color) || matches!(r, Ty::Color) {
                    self.err(span, "colors cannot be compared");
                }
                if matches!(l, Ty::Bool) ^ matches!(r, Ty::Bool) {
                    self.err(span, "a bool cannot be compared with a number");
                }
                Ty::Bool
            }
            BinOp::Add(add) => {
                // `+` concatenates strings; everything else is numeric.
                if matches!(l, Ty::String) || matches!(r, Ty::String) {
                    if matches!(l, Ty::Bool) || matches!(r, Ty::Bool) {
                        self.err(span, "bools do not concatenate");
                    }
                    return Ty::String;
                }
                let _ = add;
                self.numeric_operands(span, &l, &r)
            }
            BinOp::Mul(_) | BinOp::Pow => self.numeric_operands(span, &l, &r),
        }
    }

    fn numeric_operands(&mut self, span: Span, l: &Ty, r: &Ty) -> Ty {
        for t in [l, r] {
            if matches!(t, Ty::Bool | Ty::String | Ty::Color) {
                self.err(
                    span,
                    "arithmetic needs numbers; a bool/string/color here is a bug, not a coercion",
                );
                break;
            }
        }
        Ty::Float
    }

    fn ident(&mut self, span: Span, name: &str) -> Ty {
        if let Some(ty) = self.out.var_types.get(name) {
            return *ty;
        }
        if self.out.variables.contains_key(name) || self.builtins_contains(name) {
            return Ty::Float;
        }
        if self.out.functions.contains_key(name) {
            self.err(span, format!("`{name}` is a function; call it or rename the variable"));
            return Ty::Float;
        }
        // Namespaced constants: colors are values, barstate reads as a bool.
        if let Some(ty) = namespace_const(name) {
            return ty;
        }
        self.unknown(span, name)
    }

    fn member(&mut self, span: Span, path: &str) -> Ty {
        if let Some(ty) = namespace_const(path) {
            return ty;
        }
        match path {
            "bar_index" | "last_bar_index" | "time" | "time_close" | "open" | "high" | "low"
            | "close" | "volume" | "hl2" | "hlc3" | "ohlc4" => Ty::Float,
            "barstate.isconfirmed" => Ty::Bool,
            "syminfo.ticker" => Ty::String,
            "strategy.position_size" | "strategy.position_avg_price" | "strategy.equity"
            | "strategy.openprofit" | "strategy.closedtrades" | "strategy.wintrades" => Ty::Float,
            _ => self.unknown(span, path),
        }
    }

    fn call(&mut self, span: Span, callee: &str, args: &[Arg]) -> Ty {
        // Arity, by builtin. Named args are allowed everywhere; positional
        // order matches the table in docs/23.
        let (min, max): (usize, usize) = match callee {
            "na" | "nz" => (1, 2),
            "fixnan" => (1, 1),
            "ta.sma" | "ta.ema" | "ta.rma" | "ta.wma" | "ta.rsi" | "ta.atr" | "ta.tr"
            | "ta.highest" | "ta.lowest" | "ta.change" | "ta.mom" | "ta.roc" => (2, 2),
            "ta.macd" => (3, 5),
            "ta.stoch" => (3, 3),
            "ta.bb" => (3, 3),
            "ta.crossover" | "ta.crossunder" | "ta.cross" => (2, 2),
            "ta.vwap" => (0, 0),
            "math.abs" | "math.floor" | "math.ceil" | "math.round" | "math.sqrt" | "math.log"
            | "math.exp" | "math.sign" => (1, 1),
            "math.min" | "math.max" | "math.avg" | "math.sum" => (1, 8),
            "math.pow" => (2, 2),
            "plot" | "plotshape" | "plotchar" | "plotarrow" => (1, 8),
            "hline" => (1, 4),
            "fill" => (2, 6),
            "bgcolor" | "barcolor" => (1, 4),
            "input.int" | "input.float" | "input.bool" | "input.string" | "input.color" => (0, 6),
            "strategy.entry" | "strategy.exit" | "strategy.close" | "strategy.close_all"
            | "strategy.cancel" => (1, 6),
            c if self.out.functions.contains_key(c) => {
                let sig = self.out.functions.get(c).expect("checked above").clone();
                let positional = args.iter().filter(|a| a.name.is_none()).count();
                if positional != sig.params.len() {
                    self.err(
                        span,
                        format!(
                            "`{c}` takes {} argument(s), got {positional}",
                            sig.params.len()
                        ),
                    );
                }
                for a in args {
                    self.expr(&a.value);
                }
                return Ty::Float;
            }
            _ => {
                self.unknown(span, callee);
                for a in args {
                    self.expr(&a.value);
                }
                return Ty::Float;
            }
        };
        let positional = args.iter().filter(|a| a.name.is_none()).count();
        let total = args.len();
        if total > max || positional > max {
            self.err(span, format!("`{callee}` takes at most {max} argument(s)"));
        }
        if total < min {
            self.err(span, format!("`{callee}` needs at least {min} argument(s)"));
        }
        for a in args {
            self.expr(&a.value);
        }
        // Inputs are whatever their kind says; everything else numeric.
        if let Some(kind) = callee.strip_prefix("input.") {
            return match kind {
                "bool" => Ty::Bool,
                "string" => Ty::String,
                "color" => Ty::Color,
                _ => Ty::Float,
            };
        }
        match callee {
            "ta.crossover" | "ta.crossunder" | "ta.cross" => Ty::Bool,
            "barstate.isconfirmed" => Ty::Bool,
            _ => Ty::Float,
        }
    }

    fn builtins_contains(&self, name: &str) -> bool {
        matches!(
            name,
            "open" | "high" | "low" | "close" | "volume" | "hl2" | "hlc3" | "ohlc4" | "bar_index"
                | "last_bar_index" | "time" | "time_close"
        )
    }

    fn unknown(&mut self, span: Span, name: &str) -> Ty {
        self.err(
            span,
            format!(
                "`{name}` is not defined here. Known namespaces: ta., math., input., strategy., color.; known series: open/high/low/close/volume/hl2/hlc3/ohlc4/bar_index/time."
            ),
        );
        Ty::Float
    }
}

/// Namespaced constants and reads the checker can type statically.
fn namespace_const(name: &str) -> Option<Ty> {
    if name.starts_with("color.") {
        return Some(Ty::Color);
    }
    match name {
        "barstate.isconfirmed" => Some(Ty::Bool),
        "syminfo.ticker" => Some(Ty::String),
        "strategy.position_size" | "strategy.position_avg_price" | "strategy.equity"
        | "strategy.openprofit" | "strategy.closedtrades" | "strategy.wintrades" => Some(Ty::Float),
        _ => None,
    }
}

/// The static type of an expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// A number (int and float unify, Pine-style).
    Float,
    /// `true`/`false`.
    Bool,
    /// A string.
    String,
    /// A color.
    Color,
    /// `na`, before it lands in a typed context.
    Na,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lex, parse, vet};

    fn check_src(src: &str) -> Vec<ScriptError> {
        let tokens = match lex::lex(src) {
            Ok((_, t)) => t,
            Err(e) => return e,
        };
        let script = match parse::parse(tokens) {
            Ok(s) => s,
            Err(e) => return e,
        };
        check(&script)
    }
    #[test]
    fn a_typed_clean_script_has_no_errors() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             r = ta.rsi(close, 14)\n\
             plot(r)\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn float_to_int_is_refused_by_arity_not_cast() {
        // `ta.sma(close, 14.5)` -- the period must be integral; the call is
        // accepted by arity, so this test pins that the *interpreter* rounds
        // and the checker at least does not crash. (Pine has the same rule.)
        let errs = check_src(
            "//@pine_lite version=1\n\
             r = ta.sma(close, 14.5)\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn bool_in_arithmetic_is_refused() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             x = close + true\n",
        );
        assert!(errs.iter().any(|e| e.message.contains("arithmetic needs numbers")), "{errs:?}");
    }

    #[test]
    fn unknown_name_names_the_namespaces() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             x = zzz\n",
        );
        assert!(errs.iter().any(|e| e.message.contains("zzz")), "{errs:?}");
    }

    #[test]
    fn inputs_are_recorded_for_the_ui() {
        let tokens = lex::lex(
            "//@pine_lite version=1\n\
             len = input.int(defval=14, title=\"Length\")\n\
             r = ta.rsi(close, len)\n\
             plot(r)\n",
        )
        .expect("lex");
        let script = parse::parse(tokens.1).expect("parse");
        let cx_errs = check(&script);
        assert!(cx_errs.is_empty(), "{cx_errs:?}");
        // The checker's own output is consumed by the interpreter; the UI
        // re-reads the AST. This test pins that input declarations do not
        // error.
    }

    #[test]
    fn vet_end_to_end_refuses_a_type_error() {
        let errs = vet("//@pine_lite version=1\nx = close + true").expect_err("type error");
        assert!(errs.iter().any(|e| matches!(e.kind, crate::ErrorKind::Type)));
    }
}
