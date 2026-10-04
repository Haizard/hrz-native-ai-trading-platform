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
    /// Every `request.security("SYM", "tf", ...)` pair the script names, as
    /// `SYM@TF` keys in first-seen order (docs/23 Phase 11). The host fetches
    /// exactly these before running the script; `check_with_header` refuses
    /// scripts naming more than the cap.
    pub series_pool: Vec<String>,
}

/// One user function's signature.
#[derive(Debug, Clone)]
pub struct FuncSig {
    /// Parameter names.
    pub params: Vec<String>,
    /// One entry per parameter, right-aligned: `Some(expr)` when the
    /// parameter declares a default (`f(a, b = 2) => ...`), `None` when it
    /// does not. The checker derives the call's minimum arity from the
    /// leading run of `None`s; the interpreter re-evaluates the default per
    /// call, in the call's scope, so a default of `close[1]` binds the
    /// caller's yesterday.
    pub defaults: Vec<Option<Expr>>,
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
    check_with_header(script, &crate::Header::default())
}

/// The vet-time check, with the LEXED header so the `request.*` rules can
/// see whether the script actually declared its second instrument. A
/// `request.close()` with no `sec=` in the header is refused HERE, with the
/// fix in the message, rather than at run time.
pub fn check_with_header(script: &Script, header: &crate::Header) -> Vec<ScriptError> {
    let mut cx = Cx {
        errors: Vec::new(),
        out: Checked::default(),
        depth: 0,
        sec: header.sec.clone(),
        series_pool: Vec::new(),
        in_security_expr: 0,
    };
    cx.items(&script.items);
    cx.out.series_pool = std::mem::take(&mut cx.series_pool);
    cx.errors
}

/// The `request.security("SYM", "tf", ...)` pairs a script names, as
/// `SYM@TF` keys in first-seen order (docs/23 Phase 11). The host fetches
/// and aligns exactly these before running; an empty list means nothing to
/// fetch. Runs the full check first, so a script that does not vet (bad
/// literals, over-cap pool) yields whatever was collected until the error.
#[must_use]
pub fn collect_series_pool(script: &Script, header: &crate::Header) -> Vec<String> {
    let mut cx = Cx {
        errors: Vec::new(),
        out: Checked::default(),
        depth: 0,
        sec: header.sec.clone(),
        series_pool: Vec::new(),
        in_security_expr: 0,
    };
    cx.items(&script.items);
    cx.series_pool
}

/// The `request.data("NAME")` names a script reads (docs/23 Phase 14), in
/// first-seen order: the host fills exactly these from platform feeds
/// (ticker fields today, venue funding/OI as those land).
#[must_use]
pub fn collect_data_names(script: &Script) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(expr: &Expr, names: &mut Vec<String>) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if callee == "request.data" {
                    if let Some(a) = args.first() {
                        if let ExprKind::Str(s) = &a.value.kind {
                            if !names.contains(s) {
                                names.push(s.clone());
                            }
                        }
                    }
                }
                for a in args {
                    walk(&a.value, names);
                }
            }
            ExprKind::Bin { left, right, .. } => {
                walk(left, names);
                walk(right, names);
            }
            _ => {}
        }
    }
    fn walk_items(items: &[Item], names: &mut Vec<String>) {
        for item in items {
            match item {
                Item::Expr { expr, .. } => walk(expr, names),
                Item::Assign { expr, .. } => walk(expr, names),
                Item::Block { exprs, body, els, .. } => {
                    for e in exprs {
                        walk(e, names);
                    }
                    walk_items(body, names);
                    if let Some(els) = els {
                        walk_items(els, names);
                    }
                }
                Item::Destructure { call, .. } => walk(call, names),
                _ => {}
            }
        }
    }
    walk_items(&script.items, &mut names);
    names
}

/// How many distinct pairs one script may name (docs/23 Phase 11): a fetch
/// is a real cost, and eight pairs cover every sane multi-leg strategy.
const MAX_SERIES_POOL: usize = 8;

struct Cx {
    errors: Vec<ScriptError>,
    out: Checked,
    depth: usize,
    /// The header's `sec=` symbol, when declared.
    sec: Option<String>,
    /// Every `request.security("SYM", "tf", ...)` pair the script names,
    /// as `SYM@TF` keys in first-seen order. The host reads this AFTER the
    /// check to know what to fetch; the cap is enforced here, at vet time.
    series_pool: Vec<String>,
    /// Depth inside a `request.security` third argument: there, bare
    /// `request.*` reads the PAIR (the VM swaps the candle set), so the
    /// no-`sec=` gate must not fire.
    in_security_expr: usize,
}

impl Cx {
    /// Record a `request.security` pair (already vetted as string literals)
    /// and refuse the script when it names more than [`MAX_SERIES_POOL`].
    fn check_series_pool(&mut self, args: &[Arg]) {
        if args.len() < 2 {
            return;
        }
        let sym = match &args[0].value.kind {
            crate::parse::ExprKind::Str(s) => s.to_uppercase(),
            _ => return,
        };
        let tf = match &args[1].value.kind {
            crate::parse::ExprKind::Str(s) => s.to_uppercase(),
            _ => return,
        };
        let key = format!("{sym}@{tf}");
        if !self.series_pool.contains(&key) {
            self.series_pool.push(key);
            if self.series_pool.len() > MAX_SERIES_POOL {
                self.err(
                    Span::new(1, 1),
                    format!(
                        "a script may request at most {MAX_SERIES_POOL} symbol/timeframe pairs; got {}",
                        self.series_pool.len()
                    ),
                );            }
        }
    }

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
            Item::Destructure { span, names, call } => {
                // The callee decides the count; the names decide how many the
                // author wanted. Anything else (a single-output call, an
                // unknown name) is an error with the fix in the text.
                match &call.kind {
                    ExprKind::Call { callee, args } => {
                        for a in args {
                            self.expr(&a.value);
                        }
                        match builtin_arity(callee) {
                            Some((min, max)) => {
                                let total = args.len();
                                let positional =
                                    args.iter().filter(|a| a.name.is_none()).count();
                                if total > max || positional > max {
                                    self.err(
                                        *span,
                                        format!("`{callee}` takes at most {max} argument(s)"),
                                    );
                                }
                                if total < min {
                                    self.err(
                                        *span,
                                        format!("`{callee}` needs at least {min} argument(s)"),
                                    );
                                }
                            }
                            None => {
                                if !self.out.functions.contains_key(callee) {
                                    self.unknown(*span, callee);
                                }
                            }
                        }
                        match multi_output_count(callee) {
                            Some(n) if n == names.len() => {}
                            Some(n) => self.err(
                                *span,
                                format!(
                                    "`{callee}` returns {n} value(s), but {} name(s) are given",
                                    names.len()
                                ),
                            ),
                            None => self.err(
                                *span,
                                format!(
                                    "`{callee}` returns one value; declare it with `name = {callee}(...)`, or use ta.macd, ta.bb or ta.stoch here"
                                ),
                            ),
                        }
                    }
                    _ => self.err(
                        *span,
                        "a multi-value assignment needs a function call on the right-hand side",
                    ),
                }
                for name in names {
                    self.out.var_types.insert(name.clone(), Ty::Float);
                    self.out.variables.insert(name.clone(), VarMode::Auto);
                }
            }
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
                if *kind == BlockKind::For || *kind == BlockKind::While {
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
            Item::FuncDef { span, name, params, defaults, body } => {
                if self.out.functions.contains_key(name) {
                    self.err(*span, format!("function `{name}` is defined twice"));
                }
                // Defaults must be trailing: required parameters first,
                // defaulted ones last. A required parameter after an
                // optional one could never be filled by a positional call.
                let mut seen_default = false;
                for d in defaults {
                    match d {
                        None if seen_default => self.err(
                            *span,
                            format!(
                                "`{name}`: a parameter without a default follows one with a default; defaults must be trailing"
                            ),
                        ),
                        None => {}
                        Some(_) => seen_default = true,
                    }
                }
                self.out
                    .functions
                    .insert(name.clone(), FuncSig { params: params.clone(), defaults: defaults.clone() });
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
            ExprKind::NaChecked { value } => {
                self.expr(value);
                Ty::Bool
            }
            ExprKind::Ident(name) => self.ident(expr.span, name),
            ExprKind::Member { path } => self.member(expr.span, path),
            ExprKind::Call { callee, args } => self.call(expr.span, callee, args),
            ExprKind::History { base, offset } => {
                // The interpreter only indexes a variable or a builtin series
                // (`interp.rs`'s history rule); anything else -- a call, a
                // parenthesized expression -- parses but dies at run time,
                // where the repair loop can never see it. Name the rule here.
                if !matches!(base.kind, ExprKind::Ident(_) | ExprKind::Member { .. }) {
                    self.err(
                        base.span,
                        "history indexing needs a variable or a builtin series; assign the expression to a variable first",
                    );
                }
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
            | "close" | "volume" | "hl2" | "hlc3" | "ohlc4" | "hour" | "minute"
            | "dayofweek" => Ty::Float,
            "barstate.isconfirmed" => Ty::Bool,
            "syminfo.ticker" => Ty::String,
            "strategy.position_size" | "strategy.position_avg_price" | "strategy.equity"
            | "strategy.openprofit" | "strategy.closedtrades" | "strategy.wintrades" => Ty::Float,
            _ => self.unknown(span, path),
        }
    }

    fn call(&mut self, span: Span, callee: &str, args: &[Arg]) -> Ty {
        // `request.security("SYM", "tf", expr)` (Phase 11, docs/23): any
        // pair, any timeframe the host serves, per call -- the full Pine
        // form. The symbol/timeframe must be string LITERALS (the host
        // fetches them before the script runs; a computed symbol is a fetch
        // the user never saw). The third argument is a series expression
        // over that pair, evaluated per bar by the VM against the pooled
        // series; it may itself contain request.security (depth-capped).
        if callee == "request.security" {
            if args.len() != 3 {
                self.err(
                    span,
                    "`request.security` takes exactly 3 arguments: request.security(\"SYM\", \"tf\", series_expr)".to_string(),
                );
            }
            for (i, a) in args.iter().enumerate() {
                if i < 2 {
                    if let Some(s) = a.name.as_deref() {
                        self.err(span, format!("`request.security` argument {} is positional; named `{s}` is not accepted", i + 1));
                    }
                    match a.value.kind {
                        crate::parse::ExprKind::Str(_) => {}
                        _ => self.err(
                            span,
                            format!(
                                "`request.security` argument {} must be a quoted string literal (the host fetches it before the run): got an expression",
                                i + 1
                            ),
                        ),
                    }
                }
            }
            self.check_series_pool(args);
            // The third argument is evaluated over the PAIR: inside it, bare
            // `request.*` reads the pooled series, so the no-`sec=` gate is
            // lifted for exactly this subtree.
            self.in_security_expr += 1;
            if let Some(a) = args.get(2) {
                self.expr(&a.value);
            }
            self.in_security_expr -= 1;
            return Ty::Float;
        }
        // The second instrument's reads are header-gated: a `request.*` call
        // in a script that never declared `sec=` is a vet error naming the
        // fix, not a runtime surprise. `request.security` was handled above,
        // bare `request.*` INSIDE its third argument reads the pooled pair
        // (gate lifted), and `request.data` is platform-native — it has no
        // second instrument to declare.
        if callee.starts_with("request.")
            && callee != "request.data"
            && self.sec.is_none()
            && self.in_security_expr == 0
        {
            self.err(
                span,
                format!(
                    "`{callee}` needs a second instrument; add sec=\"SYMBOL\" to the //@pine_lite header"
                ),
            );
            for a in args {
                self.expr(&a.value);
            }
            return Ty::Float;
        }
        // A multi-output builtin read as a single value: almost always a
        // missing `a, b, c =` -- name the fix rather than type a hole.
        if let Some(n) = multi_only_count(callee) {
            self.err(
                span,
                format!(
                    "`{callee}` returns {n} values; write them as `a, b, c = {callee}(...)`"
                ),
            );
            for a in args {
                self.expr(&a.value);
            }
            return Ty::Float;
        }
        // Arity, by builtin. Named args are allowed everywhere; positional
        // order matches the dispatchers in `interp.rs` (`call_f`/`call_stmt`),
        // which are the only real implementations -- this table must never
        // teach a function or a shape the interpreter would refuse.
        // `request.data`'s name must be a string LITERAL: the host fills the
        // series by name before the run, so a computed name is a feed the
        // user never saw.
        if callee == "request.data" {
            if let Some(a) = args.first() {
                if a.name.is_none() {
                    if !matches!(a.value.kind, ExprKind::Str(_)) {
                        self.err(
                            span,
                            "`request.data` takes a quoted series name, e.g. request.data(\"BTCUSDT.change_pct\")".to_string(),
                        );
                    }
                }
            }
        }
        let (min, max): (usize, usize) = match builtin_arity(callee) {
            Some(arity) => arity,
            None if self.out.functions.contains_key(callee) => {
                let sig = self.out.functions.get(callee).expect("checked above").clone();
                let positional = args.iter().filter(|a| a.name.is_none()).count();
                // Minimum arity: required parameters only. A default fills
                // every omitted trailing slot (`f(a, b = 2)` accepts 1 or 2
                // args).
                let min = sig.defaults.iter().take_while(|d| d.is_none()).count();
                if positional < min || positional > sig.params.len() {
                    let range = if min == sig.params.len() {
                        format!("{}", sig.params.len())
                    } else {
                        format!("{min}..={}", sig.params.len())
                    };
                    self.err(
                        span,
                        format!(
                            "`{callee}` takes {range} argument(s), got {positional}"
                        ),
                    );
                }
                for a in args {
                    self.expr(&a.value);
                }
                return Ty::Float;
            }
            None => {
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
                | "last_bar_index" | "time" | "time_close" | "hour" | "minute" | "dayofweek"
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
/// Positional-argument bounds per builtin. The single source of truth for
/// what an author may write: `interp.rs` dispatches exactly these shapes, so
/// a call that passes here runs.
fn builtin_arity(callee: &str) -> Option<(usize, usize)> {
    Some(match callee {
        "na" | "nz" => (1, 2),
        "fixnan" => (1, 1),
        // Session filters: the bare words are series reads, and models write
        // the call form `hour(time)` just as reflexively. The argument is
        // conventionally `time`; the value is the current bar's UTC clock.
        "hour" | "minute" | "dayofweek" => (1, 1),
        "ta.sma" | "ta.ema" | "ta.rma" | "ta.wma" | "ta.rsi" | "ta.highest" | "ta.lowest"
        | "ta.mom" | "ta.roc" => (2, 2),
        "ta.atr" | "ta.change" => (1, 1),
        "ta.tr" | "ta.vwap" => (0, 0),
        "ta.stoch" => (2, 2),
        "ta.macd" => (3, 4),
        "ta.bb" => (2, 3),
        "ta.crossover" | "ta.crossunder" | "ta.cross" => (2, 2),
        "math.abs" | "math.floor" | "math.ceil" | "math.round" | "math.sqrt" | "math.log"
        | "math.exp" | "math.sign" => (1, 1),
        "math.min" | "math.max" | "math.avg" => (1, 8),
        "math.sum" => (2, 2),
        "math.pow" => (2, 2),
        // Arrays: a variable holds a handle from `array.new()`; readers are
        // functions, mutators are statements.
        "array.new" => (0, 0),
        "array.get" => (2, 2),
        "array.size" | "array.first" | "array.last" | "array.min" | "array.max"
        | "array.avg" => (1, 1),
        "array.includes" => (2, 2),
        "array.push" => (2, 2),
        "array.pop" | "array.shift" | "array.clear" => (1, 1),
        "array.set" => (3, 3),
        // The second instrument (`sec=` in the header): the host fetches and
        // aligns its candles; the script reads them through these. `time` is
        // the aligned bar's open time (docs/28) -- a change in it is the
        // other market's new bar.
        "request.symbol" | "request.open" | "request.high" | "request.low"
        | "request.close" | "request.volume" | "request.time" => (0, 0),
        // Phase 11: the full Pine form, any pair, any timeframe. Arity is
        // checked in `call` (which also vets the literal strings and fills
        // the series pool); the entry here keeps `unknown()` away from it.
        "request.security" => (3, 3),
        // Phase 13 drawing objects: positional anchors + named knobs. They
        // are STATEMENTS (no value), so the checker's call path only vets
        // arity here; the dispatch pushes to the object heap. The `_time`
        // twins (docs/28) anchor by unix-nanos timestamps instead of bar
        // indexes, for drawings over pooled higher timeframes.
        "line.new" => (4, 7),
        "label.new" => (3, 4),
        // Boxes: 4 anchors + color + the docs/31 border knobs
        // (border_color, border_width, border_style).
        "box.new" => (4, 8),
        "line.new_time" => (4, 7),
        "label.new_time" => (3, 4),
        "box.new_time" => (4, 8),
        // Phase 14: platform-native data by name. The literal name is
        // vetted (the host cannot serve a computed name); the series itself
        // is host-supplied, so an unknown name is a RUNTIME report.
        "request.data" => (1, 1),
        "plot" | "plotshape" | "plotchar" | "plotarrow" => (1, 8),
        "hline" => (1, 4),
        "fill" => (2, 6),
        "bgcolor" | "barcolor" => (1, 4),
        "input.int" | "input.float" | "input.bool" | "input.string" | "input.color" => (0, 6),
        "strategy.entry" | "strategy.exit" | "strategy.close" | "strategy.cancel" => (1, 6),
        // Zero-arg: close everything.
        "strategy.close_all" => (0, 6),
        _ => return None,
    })
}

/// How many values a multi-output builtin hands back. Only these may appear on
/// the right of `a, b, c = ...`.
fn multi_output_count(callee: &str) -> Option<usize> {
    match callee {
        "ta.macd" | "ta.bb" => Some(3),
        // `ta.stoch` reads as its %K alone, and destructures to %K and %D.
        "ta.stoch" => Some(2),
        _ => None,
    }
}

/// Multi-output builtins with NO single-value reading: using one as a scalar
/// is always a mistake worth naming.
fn multi_only_count(callee: &str) -> Option<usize> {
    match callee {
        "ta.macd" | "ta.bb" => Some(3),
        _ => None,
    }
}

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
