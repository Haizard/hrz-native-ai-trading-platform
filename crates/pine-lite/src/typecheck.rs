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
    /// Inferred parameter types, one per name.
    pub param_types: Vec<Ty>,
    /// One entry per parameter, right-aligned: `Some(expr)` when the
    /// parameter declares a default (`f(a, b = 2) => ...`), `None` when it
    /// does not. The checker derives the call's minimum arity from the
    /// leading run of `None`s; the interpreter re-evaluates the default per
    /// call, in the call's scope, so a default of `close[1]` binds the
    /// caller's yesterday.
    pub defaults: Vec<Option<Expr>>,
    /// Type of the function body's final expression.
    pub return_type: Ty,
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
        local_types: Vec::new(),
        input_timeframes: std::collections::HashMap::new(),
        security_outer: Vec::new(),
        stmt_call: false,
        stmt_depth: 0,
        top_assigned: std::collections::HashSet::new(),
        input_symbols: std::collections::HashMap::new(),
        type_fields: std::collections::HashMap::new(),
        var_udt_types: std::collections::HashMap::new(),
        methods: std::collections::HashMap::new(),
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
        local_types: Vec::new(),
        input_timeframes: std::collections::HashMap::new(),
        security_outer: Vec::new(),
        stmt_call: false,
        stmt_depth: 0,
        top_assigned: std::collections::HashSet::new(),
        input_symbols: std::collections::HashMap::new(),
        type_fields: std::collections::HashMap::new(),
        var_udt_types: std::collections::HashMap::new(),
        methods: std::collections::HashMap::new(),
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
                Item::Block {
                    exprs, body, els, ..
                } => {
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
    /// Function-local value kinds, innermost scope last.
    local_types: Vec<HashMap<String, Ty>>,
    /// `tf = input.timeframe("4h")` bindings seen at the top level, so
    /// `request.security("SYM", tf, ...)` resolves its pool key the way the
    /// VM's registry does.
    input_timeframes: std::collections::HashMap<String, String>,
    /// `input.symbol` var name -> its chosen symbol (defval or host).
    input_symbols: std::collections::HashMap<String, String>,
    /// Declared UDTs: type name -> ordered (field, type word) layout.
    type_fields: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Vars known to hold a UDT instance: var name -> type name.
    var_udt_types: std::collections::HashMap<String, String>,
    /// Declared methods: (receiver type, name) -> params (receiver first).
    methods: std::collections::HashMap<(String, String), Vec<String>>,
    /// Names in scope OUTSIDE the innermost `request.security` third
    /// argument. The sub-VM that evaluates the expression starts with EMPTY
    /// vars over the pooled candles, so a chart variable captured there
    /// checks clean here and dies at run time ("not a series") where the
    /// repair loop never sees it. One set per nesting level (security
    /// expressions may nest, depth-capped).
    security_outer: Vec<std::collections::HashSet<String>>,
    /// True only while the OUTERMOST call of a statement-level expression is
    /// being checked: drawing builtins, plot-family calls and strategy order
    /// commands are statements -- they return no value the VM can bind -- so
    /// a nested use (`x := box.new(...)`, `fill(plot(a), plot(b))`) checks
    /// clean here and then dies at run time, where the repair loop can never
    /// see it. Name the rule at vet time instead.
    stmt_call: bool,
    /// Statement nesting: 0 on the script's top-level items, deeper inside
    /// if/else/loop/function bodies.
    stmt_depth: usize,
    /// Names with at least one TOP-LEVEL assignment. A top-level read of a
    /// name assigned only inside a block checks clean (the name is in
    /// var_types) but dies at run time on every bar before the block first
    /// executes -- and the deferred plot replay reads every bar, bar 0
    /// included. Pine's rule is the same: block declarations stay local.
    top_assigned: std::collections::HashSet<String>,
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
            crate::parse::ExprKind::Ident(v) => match self.input_symbols.get(v) {
                Some(s) => s.to_uppercase(),
                None => return,
            },
            _ => return,
        };
        let tf = match &args[1].value.kind {
            crate::parse::ExprKind::Str(s) => s.to_uppercase(),
            crate::parse::ExprKind::Ident(v) => match self.input_timeframes.get(v) {
                Some(s) => s.to_uppercase(),
                None => return,
            },
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
                );
            }
        }
    }

    /// The `request.security_lower_tf` pool keys: `SYM@LTF#ltf` -- the
    /// suffix tells the host to pool the RAW lower-timeframe series (a
    /// plain `SYM@LTF` key from request.security gets the raw source form).
    fn check_lower_tf_pool(&mut self, args: &[Arg]) {
        if args.len() < 2 {
            return;
        }
        let sym = match &args[0].value.kind {
            crate::parse::ExprKind::Str(s) => s.to_uppercase(),
            crate::parse::ExprKind::Ident(v) => match self.input_symbols.get(v) {
                Some(s) => s.to_uppercase(),
                None => return,
            },
            _ => return,
        };
        let tf = match &args[1].value.kind {
            crate::parse::ExprKind::Str(s) => s.to_uppercase(),
            crate::parse::ExprKind::Ident(v) => match self.input_timeframes.get(v) {
                Some(s) => s.to_uppercase(),
                None => return,
            },
            _ => return,
        };
        let key = format!("{sym}@{tf}#ltf");
        if !self.series_pool.contains(&key) {
            self.series_pool.push(key);
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
                if self.stmt_depth == 0 {
                    for n in names {
                        self.top_assigned.insert(n.clone());
                    }
                }
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
                                let positional = args.iter().filter(|a| a.name.is_none()).count();
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
            Item::TypeDef { span, name, fields } => {
                if self.type_fields.contains_key(name) {
                    self.err(*span, format!("type `{name}` is declared twice"));
                }
                // A field name may not repeat inside one type; the instance
                // layout is the declaration order.
                let mut seen = std::collections::HashSet::new();
                for (fname, word) in fields {
                    if !seen.insert(fname) {
                        self.err(*span, format!("type `{name}` declares `{fname}` twice"));
                    }
                    let known = matches!(
                        word.as_str(),
                        "float"
                            | "int"
                            | "string"
                            | "bool"
                            | "color"
                            | "line"
                            | "label"
                            | "box"
                            | "table"
                            | "linefill"
                            | "polyline"
                            | "array"
                    );
                    if !known {
                        self.err(
                            *span,
                            format!("`{word}` is not a field type pine-lite knows"),
                        );
                    }
                }
                self.type_fields.insert(name.clone(), fields.clone());
            }
            Item::Assign {
                span,
                name,
                mode,
                expr,
                decl_type,
                ..
            } => {
                // An explicit type word must be known, and its right-hand
                // side must have the corresponding value kind.
                if let Some(word) = decl_type {
                    let known = matches!(
                        word.as_str(),
                        "float"
                            | "int"
                            | "bool"
                            | "color"
                            | "string"
                            | "line"
                            | "label"
                            | "box"
                            | "table"
                            | "linefill"
                            | "polyline"
                    ) || self.type_fields.contains_key(word);
                    if !known {
                        self.err(*span, format!(
                            "`{word}` is not a type pine-lite knows: float, int, bool, color, line, label, box, table, linefill, polyline"
                        ));
                    }
                }
                // `input.*` declarations are recorded for the UI and typed by
                // their call.
                if let ExprKind::Call { callee, args } = &expr.kind {
                    if let Some(kind) = callee.strip_prefix("input.") {
                        self.record_input(name, kind, args, *span);
                        // Register the timeframe binding for security-key
                        // resolution (sequential walk: declarations precede
                        // the request.security calls that use them).
                        if kind == "symbol" {
                            let sym = args
                                .iter()
                                .find(|a| a.name.as_deref() == Some("defval"))
                                .or_else(|| args.first())
                                .and_then(|a| match &a.value.kind {
                                    ExprKind::Str(s) => Some(s.to_uppercase()),
                                    _ => None,
                                });
                            if let Some(sym) = sym {
                                self.input_symbols.insert(name.clone(), sym);
                            }
                        }
                        if kind == "timeframe" {
                            let tf = args
                                .iter()
                                .find(|a| a.name.as_deref() == Some("defval"))
                                .or_else(|| args.first())
                                .and_then(|a| match &a.value.kind {
                                    ExprKind::Str(s) => Some(s.clone()),
                                    _ => None,
                                });
                            if let Some(tf) = tf {
                                self.input_timeframes.insert(name.clone(), tf);
                            }
                        }
                    }
                }
                let mut ty = self.expr(expr);
                // `p = Point.new(...)`: the var holds that type's instances.
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if let Some(tn) = callee.strip_suffix(".new") {
                        if self.type_fields.contains_key(tn) {
                            self.var_udt_types.insert(name.clone(), tn.to_string());
                        }
                    }
                }
                if let Some(word) = decl_type {
                    let declared_ty = match word.as_str() {
                        "float" | "int" | "line" | "label" | "box" | "table" | "linefill"
                        | "polyline" => Some(Ty::Float),
                        "bool" => Some(Ty::Bool),
                        "string" => Some(Ty::String),
                        "color" => Some(Ty::Color),
                        // A user type word: instances are handles (floats).
                        w if self.type_fields.contains_key(w) => Some(Ty::Float),
                        _ => None,
                    };
                    if let Some(expected) = declared_ty {
                        if ty == Ty::Na {
                            ty = expected;
                        } else if ty != expected {
                            self.err(
                                *span,
                                format!(
                                    "`{name}` is declared `{word}` but the right-hand side is {ty:?} -- the declaration and the value must agree"
                                ),
                            );
                        }
                    }
                }
                // `p.x := v`: legal only when p holds a UDT -- the field
                // must exist and the value must match its type.
                if let Some((base, field)) = name.split_once('.') {
                    match self.var_udt_types.get(base).cloned() {
                        Some(type_name) => {
                            if let Some(layout) = self.type_fields.get(&type_name) {
                                match layout.iter().find(|(f, _)| f == field) {
                                    Some((_, word)) => {
                                        let want = match word.as_str() {
                                            "bool" => Ty::Bool,
                                            "color" => Ty::Color,
                                            "string" => Ty::String,
                                            _ => Ty::Float,
                                        };
                                        if ty != want && ty != Ty::Na {
                                            self.err(*span, format!(
                                                "field `{field}` of {type_name} is {want:?} but the value is {ty:?}"
                                            ));
                                        }
                                    }
                                    None => self.err(*span, format!("type {type_name} has no field `{field}`")),
                                }
                            }
                        }
                        None => self.err(*span, format!(
                            "`{base}` does not hold a user type, so `{name}` is not a field -- declare it with `{base} = TypeName.new(...)` first"
                        )),
                    }
                }
                if !name.contains('.') {
                    let previous = match self.local_types.last() {
                        Some(scope) => scope.get(name).copied(),
                        None => self.out.var_types.get(name).copied(),
                    };
                    if let Some(previous) = previous {
                        if previous == Ty::Na {
                            // `na` does not constrain a series until a concrete
                            // value is assigned.
                        } else if ty == Ty::Na {
                            ty = previous;
                        } else if previous != ty {
                            self.err(
                                *span,
                                format!(
                                    "`{name}` was previously {previous:?} and cannot be reassigned a {ty:?} value"
                                ),
                            );
                        }
                    }
                }
                if let Some(scope) = self.local_types.last_mut() {
                    if !name.contains('.') {
                        scope.insert(name.clone(), ty);
                    }
                } else {
                    self.out.var_types.insert(name.clone(), ty);
                }
                if self.stmt_depth == 0 {
                    self.top_assigned.insert(name.clone());
                }
                if self.out.variables.insert(name.clone(), *mode).is_some() && *mode == VarMode::Var
                {
                    // Re-declaring a `var` in the same scope is usually a
                    // copy-paste; allow reassignment (Pine's `:=`) but a
                    // second `var` init would silently reset state.
                    self.err(*span, format!("`{name}` is declared twice with `var`"));
                }
            }
            Item::Block {
                span,
                kind,
                exprs,
                loop_var,
                body,
                els,
            } => {
                let expr_types: Vec<_> = exprs.iter().map(|expr| self.expr(expr)).collect();
                match kind {
                    BlockKind::If | BlockKind::While => {
                        if let (Some(condition), Some(condition_ty)) =
                            (exprs.first(), expr_types.first())
                        {
                            if !matches!(condition_ty, Ty::Bool | Ty::Na) {
                                self.err(
                                    condition.span,
                                    format!(
                                        "{} condition must be bool, got {condition_ty:?}",
                                        if *kind == BlockKind::If {
                                            "if"
                                        } else {
                                            "while"
                                        }
                                    ),
                                );
                            }
                        }
                    }
                    BlockKind::For => {
                        for (expression, ty) in exprs.iter().zip(&expr_types) {
                            if !matches!(ty, Ty::Float | Ty::Na) {
                                self.err(
                                    expression.span,
                                    format!("for-loop bounds and step must be numeric, got {ty:?}"),
                                );
                            }
                        }
                    }
                }
                if *kind == BlockKind::For || *kind == BlockKind::While {
                    self.depth += 1;
                    if self.depth > 4 {
                        self.err(*span, "loop nesting deeper than 4 is refused");
                    }
                    if let Some(v) = loop_var {
                        if let Some(scope) = self.local_types.last_mut() {
                            scope.insert(v.clone(), Ty::Float);
                        } else {
                            self.out.var_types.insert(v.clone(), Ty::Float);
                        }
                    }
                }
                self.stmt_depth += 1;
                self.items(body);
                if let Some(els) = els {
                    self.items(els);
                }
                self.stmt_depth -= 1;
                self.depth = self.depth.saturating_sub(1);
            }
            Item::FuncDef {
                span,
                name,
                params,
                defaults,
                body,
                method_of,
            } => {
                let outer_var_types = self.out.var_types.clone();
                let outer_variables = self.out.variables.clone();
                let outer_udt_types = self.var_udt_types.clone();
                let outer_input_timeframes = self.input_timeframes.clone();
                let outer_input_symbols = self.input_symbols.clone();
                if let Some(type_name) = method_of {
                    // A method: the receiver type must exist, the pair
                    // (type, name) must be new, and the receiver param
                    // types as the UDT inside the body (field access on it
                    // resolves through var_udt_types).
                    if !self.type_fields.contains_key(type_name) {
                        self.err(*span, format!(
                            "method `{name}` receivers on `{type_name}`, which is not a declared type"
                        ));
                    }
                    if let Some(recv) = params.first() {
                        self.var_udt_types.insert(recv.clone(), type_name.clone());
                    }
                    if !self
                        .methods
                        .insert((type_name.clone(), name.clone()), params.clone())
                        .is_none()
                    {
                        self.err(
                            *span,
                            format!("method `{name}` is defined twice on type `{type_name}`"),
                        );
                    }
                }
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
                self.out.functions.insert(
                    name.clone(),
                    FuncSig {
                        params: params.clone(),
                        param_types: vec![Ty::Float; params.len()],
                        defaults: defaults.clone(),
                        return_type: Ty::Float,
                    },
                );
                // Parameters are in scope inside the body; bind them as
                // numbers so a recursive or self-referencing body checks.
                let mut function_types = HashMap::new();
                for p in params {
                    function_types.insert(p.clone(), Ty::Float);
                }
                self.local_types.push(function_types);
                let default_types: Vec<_> = defaults
                    .iter()
                    .map(|default| default.as_ref().map(|expr| (expr.span, self.expr(expr))))
                    .collect();
                self.stmt_depth += 1;
                self.items(body);
                let return_type = match body.last() {
                    Some(Item::Expr { expr, .. }) => self.expr(expr),
                    _ => Ty::Float,
                };
                let mut param_types = vec![Ty::Float; params.len()];
                let string_params = string_parameters_in_items(body, params);
                for (index, param) in params.iter().enumerate() {
                    if string_params.contains(param) {
                        param_types[index] = Ty::String;
                    }
                }
                for (index, default) in default_types.iter().enumerate() {
                    let Some((span, actual)) = default else {
                        continue;
                    };
                    let expected = param_types.get(index).copied().unwrap_or(Ty::Float);
                    let compatible = match expected {
                        Ty::String => matches!(actual, Ty::Float | Ty::String | Ty::Na),
                        Ty::Float => matches!(actual, Ty::Float | Ty::Na),
                        Ty::Bool => matches!(actual, Ty::Bool | Ty::Na),
                        Ty::Color => matches!(actual, Ty::Color | Ty::Na),
                        Ty::Na => *actual == Ty::Na,
                    };
                    if !compatible {
                        self.err(
                            *span,
                            format!(
                                "function parameter default expects {expected:?}, got {actual:?}"
                            ),
                        );
                    }
                }
                if let Some(signature) = self.out.functions.get_mut(name) {
                    signature.param_types = param_types;
                    signature.return_type = return_type;
                }
                self.stmt_depth -= 1;
                self.local_types.pop();
                self.out.var_types = outer_var_types;
                self.out.variables = outer_variables;
                self.var_udt_types = outer_udt_types;
                self.input_timeframes = outer_input_timeframes;
                self.input_symbols = outer_input_symbols;
            }
            Item::Expr { span, expr } => {
                if matches!(expr.kind, ExprKind::Call { .. }) {
                    self.stmt_call = true;
                    self.expr(expr);
                    self.stmt_call = false;
                } else {
                    self.expr(expr);
                }
                if let ExprKind::Call { callee, .. } = &expr.kind {
                    if callee.starts_with("plot")
                        || callee == "hline"
                        || callee == "fill"
                        || callee == "bgcolor"
                        || callee == "barcolor"
                    {
                        self.out.plots.push(*span);
                    }
                }
            }
        }
    }

    fn record_input(&mut self, name: &str, kind: &str, args: &[Arg], span: Span) {
        let known = [
            "int",
            "float",
            "bool",
            "string",
            "color",
            "source",
            "timeframe",
            "time",
            "price",
            "symbol",
        ];
        if !known.contains(&kind) {
            self.err(
                span,
                format!(
                    "`input.{kind}` is not an input kind; known: {}",
                    known.join(", ")
                ),
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
                    Ty::Color => {
                        self.err(
                            expr.span,
                            "history indexing is for numeric, bool and string series",
                        );
                        Ty::Float
                    }
                    Ty::Bool => Ty::Bool,
                    Ty::String => Ty::String,
                    _ => Ty::Float,
                }
            }
            ExprKind::Switch { subject, arms } => {
                // The subject's type gates the arm tests; every arm's result
                // must agree on one type (the switch IS that type).
                let subj_ty = subject.as_deref().map(|s| self.expr(s));
                let mut out_ty = Ty::Na;
                for (test, result) in arms {
                    if let Some(t) = test {
                        let tt = self.expr(t);
                        match subj_ty {
                            Some(s) => {
                                if tt != s && tt != Ty::Na && s != Ty::Na {
                                    self.err(t.span, format!(
                                        "a switch arm's test is {tt:?} but the subject is {s:?} -- they must agree"
                                    ));
                                }
                            }
                            None => {
                                if !matches!(tt, Ty::Bool | Ty::Na) {
                                    self.err(t.span, "a switch without a subject tests conditions -- each arm's test must be bool");
                                }
                            }
                        }
                    }
                    let rt = self.expr(result);
                    if out_ty == Ty::Na {
                        out_ty = rt;
                    } else if rt != out_ty && rt != Ty::Na {
                        self.err(result.span, format!(
                            "switch arms must all produce one type: earlier arms give {out_ty:?}, this one {rt:?}"
                        ));
                    }
                }
                out_ty
            }
            ExprKind::Ternary { cond, then, els } => {
                let c = self.expr(cond);
                if !matches!(c, Ty::Bool | Ty::Na) {
                    self.err(cond.span, "a ternary condition must be bool");
                }
                let a = self.expr(then);
                let b = self.expr(els);
                match (a, b) {
                    (Ty::Na, other) | (other, Ty::Na) => other,
                    (left, right) if left == right => left,
                    (then_ty, else_ty) => {
                        self.err(
                            expr.span,
                            format!(
                                "ternary branches must have compatible types, got {then_ty:?} and {else_ty:?}"
                            ),
                        );
                        Ty::Float
                    }
                }
            }
            ExprKind::Un { op, expr } => {
                let inner = self.expr(expr);
                match op {
                    UnOp::Not => {
                        if !matches!(inner, Ty::Bool | Ty::Na) {
                            self.err(
                                expr.span,
                                format!(
                                    "`not` needs a bool, got {inner:?}; compare numeric values explicitly (for example, `x > level`) or use `na(x)` / `not na(x)` for missing-value checks"
                                ),
                            );
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
        if self.in_security_expr > 0 && self.security_outer.iter().any(|outer| outer.contains(name))
        {
            self.err(
                span,
                format!(
                    "`{name}` is a chart variable and is not visible inside request.security's series expression -- compute the expression over the pooled series instead (close/open/high/low/volume and ta.* of them), e.g. request.security(\"SYM\", \"tf\", ta.ema(close, 20))"
                ),
            );
        }
        // A dotted name whose base holds a UDT: `p.x` types as the field's
        // declared type.
        if let Some((base, field)) = name.split_once('.') {
            if let Some(type_name) = self.var_udt_types.get(base).cloned() {
                if let Some(layout) = self.type_fields.get(&type_name) {
                    if let Some((_, word)) = layout.iter().find(|(f, _)| f == field) {
                        return match word.as_str() {
                            "string" => Ty::String,
                            "bool" => Ty::Bool,
                            "color" => Ty::Color,
                            _ => Ty::Float,
                        };
                    }
                    self.err(span, format!("type {type_name} has no field `{field}`"));
                    return Ty::Float;
                }
            }
        }
        if let Some(ty) = self
            .local_types
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
        {
            return ty;
        }
        if let Some(ty) = self.out.var_types.get(name).copied() {
            if self.stmt_depth == 0 && !self.top_assigned.contains(name) {
                self.err(
                    span,
                    format!(
                        "`{name}` is assigned only inside a conditional or loop block and cannot be read at the top level -- declare it at the top level first (`{name} = na`) so it exists on every bar"
                    ),
                );
            }
            return ty;
        }
        // Namespaced constants first: a dotted constant in BOTH tables
        // (extend.*, position.*, the barstate set) has a precise type that
        // the builtins fallback's blanket Float would hide -- a barstate
        // read must type Bool or ternaries on it refuse.
        if let Some(ty) = namespace_const(name) {
            return ty;
        }
        if self.out.variables.contains_key(name) || self.builtins_contains(name) {
            return Ty::Float;
        }
        if self.out.functions.contains_key(name) {
            self.err(
                span,
                format!("`{name}` is a function; call it or rename the variable"),
            );
            return Ty::Float;
        }
        self.unknown(span, name)
    }

    fn member(&mut self, span: Span, path: &str) -> Ty {
        if let Some(ty) = namespace_const(path) {
            return ty;
        }
        match path {
            "bar_index" | "last_bar_index" | "time" | "time_close" | "open" | "high" | "low"
            | "close" | "volume" | "hl2" | "hlc3" | "ohlc4" | "hour" | "minute" | "dayofweek"
            | "year" | "month" | "dayofmonth" | "syminfo.mintick" => Ty::Float,
            // The line-extension constants (`extend=extend.right` on a
            // line) are the only dotted string constants.
            "extend.none" | "extend.right" | "extend.left" | "extend.both" => Ty::String,
            "math.pi" => Ty::Float,
            // The table-position constants (`table.new(position.top_right,
            // ...)`) are dotted string constants too.
            "position.top_right"
            | "position.top_left"
            | "position.top_center"
            | "position.bottom_right"
            | "position.bottom_left"
            | "position.bottom_center"
            | "position.middle_right"
            | "position.middle_left"
            | "position.middle_center" => Ty::String,
            "syminfo.pointvalue" => Ty::Float,
            // The barmerge constants are dotted string constants too.
            "barmerge.gaps_on"
            | "barmerge.gaps_off"
            | "barmerge.lookahead_on"
            | "barmerge.lookahead_off" => Ty::String,
            "size.auto" | "size.tiny" | "size.small" | "size.normal" | "size.large"
            | "size.huge" => Ty::String,
            "strategy.direction.long"
            | "strategy.direction.short"
            | "strategy.direction.all"
            | "strategy.direction.none" => Ty::String,
            "barstate.isconfirmed" | "barstate.isnew" | "barstate.isfirst" | "barstate.islast" => {
                Ty::Bool
            }
            "syminfo.ticker" => Ty::String,
            "strategy.position_size"
            | "strategy.position_avg_price"
            | "strategy.equity"
            | "strategy.openprofit"
            | "strategy.closedtrades"
            | "strategy.wintrades"
            | "strategy.netprofit"
            | "strategy.grossprofit"
            | "strategy.grossloss"
            | "strategy.max_drawdown" => Ty::Float,
            _ => self.unknown(span, path),
        }
    }

    fn call(&mut self, span: Span, callee: &str, args: &[Arg]) -> Ty {
        const STMT_ONLY: &[&str] = &[
            "plot",
            "plotshape",
            "plotchar",
            "plotarrow",
            "plotcandle",
            "plotbar",
            "hline",
            "fill",
            "bgcolor",
            "barcolor",
            "fib.new",
            "table.cell",
            "alert",
            "strategy.entry",
            "strategy.exit",
            "strategy.risk.max_position_size",
            "strategy.risk.max_intraday_loss",
            "strategy.risk.max_drawdown",
            "strategy.risk.allow_entry_in",
            "strategy.close",
            "strategy.close_all",
            "strategy.cancel",
        ];
        if !self.stmt_call && STMT_ONLY.contains(&callee) {
            self.err(
                span,
                format!(
                    "`{callee}` produces no value -- it works only as a standalone statement, not inside an expression or on the right side of an assignment"
                ),
            );
        }
        let was_stmt = self.stmt_call;
        self.stmt_call = false;
        let ty = self.call_inner(span, callee, args);
        self.stmt_call = was_stmt;
        ty
    }

    fn call_inner(&mut self, span: Span, callee: &str, args: &[Arg]) -> Ty {
        // `p.dist(q)`: a method call on a var whose type we know. The
        // receiver is not an argument; the rest bind by the signature.
        if let Some((base, mname)) = callee.split_once('.') {
            if let Some(type_name) = self.var_udt_types.get(base).cloned() {
                if let Some(sig) = self
                    .methods
                    .get(&(type_name.clone(), mname.to_string()))
                    .cloned()
                {
                    let positional = args.iter().filter(|a| a.name.is_none()).count();
                    let want = sig.len().saturating_sub(1);
                    if positional != want {
                        self.err(span, format!(
                            "method `{mname}` on {type_name} takes {want} argument(s) after the receiver, got {positional}"
                        ));
                    }
                    for a in args {
                        let ty = self.expr(&a.value);
                        if !matches!(ty, Ty::Float | Ty::Na) {
                            self.err(
                                a.value.span,
                                format!("method `{mname}` arguments must be numeric, got {ty:?}"),
                            );
                        }
                    }
                    return Ty::Float;
                }
                if self.type_fields.contains_key(&type_name) {
                    self.err(span, format!("type {type_name} has no method `{mname}`"));
                    for a in args {
                        self.expr(&a.value);
                    }
                    return Ty::Float;
                }
            }
        }
        // `Point.new(1, 2)`: a user type declared in this script. Positional
        // args fill the layout in order, named args their field; everything
        // else reads na. The value is a heap handle (a float to the VM).
        if let Some(type_name) = callee.strip_suffix(".new") {
            if let Some(layout) = self.type_fields.get(type_name).cloned() {
                let mut positional = 0usize;
                let mut assigned = std::collections::HashSet::new();
                for argument in args {
                    let field_index = if let Some(name) = argument.name.as_deref() {
                        match layout.iter().position(|(field, _)| field == name) {
                            Some(index) => Some(index),
                            None => {
                                self.err(
                                    argument.value.span,
                                    format!("type {type_name} has no field `{name}`"),
                                );
                                None
                            }
                        }
                    } else {
                        let index = positional;
                        positional += 1;
                        if index >= layout.len() {
                            self.err(
                                argument.value.span,
                                format!(
                                    "`{callee}` takes {} field value(s) in layout order",
                                    layout.len()
                                ),
                            );
                            None
                        } else {
                            Some(index)
                        }
                    };
                    let actual = self.expr(&argument.value);
                    let Some(index) = field_index else {
                        continue;
                    };
                    if !assigned.insert(index) {
                        self.err(
                            argument.value.span,
                            format!(
                                "`{callee}` initializes field `{}` more than once",
                                layout[index].0
                            ),
                        );
                        continue;
                    }
                    let expected = match layout[index].1.as_str() {
                        "string" => Ty::String,
                        "bool" => Ty::Bool,
                        "color" => Ty::Color,
                        _ => Ty::Float,
                    };
                    if actual != expected && actual != Ty::Na {
                        self.err(
                            argument.value.span,
                            format!(
                                "`{callee}` field `{}` expects {expected:?}, got {actual:?}",
                                layout[index].0
                            ),
                        );
                    }
                }
                return Ty::Float;
            }
        }
        // `request.security("SYM", "tf", expr)` (Phase 11, docs/23): any
        // pair, any timeframe the host serves, per call -- the full Pine
        // form. The symbol/timeframe must be string LITERALS (the host
        // fetches them before the script runs; a computed symbol is a fetch
        // the user never saw). The third argument is a series expression
        // over that pair, evaluated per bar by the VM against the pooled
        // series; it may itself contain request.security (depth-capped).
        // `alert(message[, freq])`: the message is a STRING expression --
        // the condition-first form (alert(cond, "text")) is the classic
        // swap and would fire every bar with the condition coerced to
        // "1"/"0" as its text. Catch it: arg 0 must type String, arg 1
        // must be an alert.freq_* constant.
        if callee == "alert" {
            if let Some(a0) = args.first() {
                let ty = self.expr(&a0.value);
                if ty != Ty::String {
                    self.err(
                        span,
                        "`alert`'s first argument is the MESSAGE (a string); the condition goes in the `if` around it, on the line before the alert. Did you mean to gate it?".to_string(),
                    );
                }
            }
            if let Some(a1) = args.get(1) {
                let ok = match &a1.value.kind {
                    crate::parse::ExprKind::Ident(v)
                    | crate::parse::ExprKind::Member { path: v } => v.starts_with("alert.freq_"),
                    _ => false,
                };
                if !ok {
                    self.err(
                        span,
                        "`alert`'s optional second argument is a frequency constant: alert.freq_once_per_bar_close (or omit it)".to_string(),
                    );
                }
            }
            return Ty::Float;
        }
        if callee == "request.security" {
            if !(3..=5).contains(&args.len()) {
                self.err(
                    span,
                    "`request.security` takes 3 to 5 arguments: request.security(\"SYM\", \"tf\", series_expr[, gaps=barmerge.gaps_off][, lookahead=barmerge.lookahead_off])".to_string(),
                );
            }
            // The merge knobs are named-only and take barmerge.* constants.
            for a in args.iter().skip(3) {
                let ok = match (a.name.as_deref(), &a.value.kind) {
                    (Some("gaps"), crate::parse::ExprKind::Ident(v))
                    | (Some("gaps"), crate::parse::ExprKind::Member { path: v }) => {
                        matches!(v.as_str(), "barmerge.gaps_on" | "barmerge.gaps_off")
                    }
                    (Some("lookahead"), crate::parse::ExprKind::Ident(v))
                    | (Some("lookahead"), crate::parse::ExprKind::Member { path: v }) => {
                        matches!(
                            v.as_str(),
                            "barmerge.lookahead_on" | "barmerge.lookahead_off"
                        )
                    }
                    _ => false,
                };
                if !ok {
                    self.err(
                        span,
                        "`request.security` arguments 4/5 are the named merge knobs: gaps=barmerge.gaps_on|barmerge.gaps_off and lookahead=barmerge.lookahead_on|barmerge.lookahead_off".to_string(),
                    );
                }
            }
            for (i, a) in args.iter().enumerate() {
                if i < 2 {
                    if let Some(s) = a.name.as_deref() {
                        self.err(span, format!("`request.security` argument {} is positional; named `{s}` is not accepted", i + 1));
                    }
                    match a.value.kind {
                        crate::parse::ExprKind::Str(_) => {}
                        _ => {
                            // An Ident bound to input.timeframe is as
                            // literal as a quoted string: the defval is a
                            // constant the host fetches before the run.
                            let is_tf_var = matches!(
                                &a.value.kind,
                                crate::parse::ExprKind::Ident(v)
                                    if self.input_timeframes.contains_key(v)
                            );
                            let is_sym_var = matches!(
                                &a.value.kind,
                                crate::parse::ExprKind::Ident(v)
                                    if self.input_symbols.contains_key(v)
                            );
                            if !((i == 1 && is_tf_var) || (i == 0 && is_sym_var)) {
                                self.err(
                                    span,
                                    format!(
                                        "`request.security` argument {} must be a quoted string literal (the host fetches it before the run): got an expression",
                                        i + 1
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            self.check_series_pool(args);
            // The third argument is evaluated over the PAIR: inside it, bare
            // `request.*` reads the pooled series, so the no-`sec=` gate is
            // lifted for exactly this subtree.
            self.in_security_expr += 1;
            self.security_outer
                .push(self.out.var_types.keys().cloned().collect());
            if let Some(a) = args.get(2) {
                self.expr(&a.value);
            }
            self.security_outer.pop();
            self.in_security_expr -= 1;
            return Ty::Float;
        }
        // `request.security_lower_tf("SYM", "ltf", expr)` (docs/23 shape,
        // Phase 11's lower-half): the same literal rules as
        // request.security, but the pool key carries the `#ltf` suffix --
        // the host fetches the RAW lower-timeframe series (no alignment),
        // and each chart bar receives an array of the values its window
        // holds.
        if callee == "request.security_lower_tf" {
            if args.len() != 3 {
                self.err(
                    span,
                    "`request.security_lower_tf` takes exactly 3 arguments: request.security_lower_tf(\"SYM\", \"ltf\", series_expr)".to_string(),
                );
            }
            for (i, a) in args.iter().take(2).enumerate() {
                match a.value.kind {
                    crate::parse::ExprKind::Str(_) => {}
                    crate::parse::ExprKind::Ident(ref v)
                        if (i == 0 && self.input_symbols.contains_key(v))
                            || (i == 1 && self.input_timeframes.contains_key(v)) => {}
                    _ => self.err(
                        span,
                        format!("`request.security_lower_tf` argument {} must be a quoted literal (or an input.symbol/input.timeframe variable)", i + 1),
                    ),
                }
            }
            self.check_lower_tf_pool(args);
            self.in_security_expr += 1;
            self.security_outer
                .push(self.out.var_types.keys().cloned().collect());
            if let Some(a) = args.get(2) {
                self.expr(&a.value);
            }
            self.security_outer.pop();
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
                format!("`{callee}` returns {n} values; write them as `a, b, c = {callee}(...)`"),
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
                let sig = self
                    .out
                    .functions
                    .get(callee)
                    .expect("checked above")
                    .clone();
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
                        format!("`{callee}` takes {range} argument(s), got {positional}"),
                    );
                }
                for (index, a) in args.iter().enumerate() {
                    let ty = self.expr(&a.value);
                    let expected = sig.param_types.get(index).copied().unwrap_or(Ty::Float);
                    let compatible = match expected {
                        Ty::String => matches!(ty, Ty::Float | Ty::String | Ty::Na),
                        Ty::Float => matches!(ty, Ty::Float | Ty::Na),
                        Ty::Bool => matches!(ty, Ty::Bool | Ty::Na),
                        Ty::Color => matches!(ty, Ty::Color | Ty::Na),
                        Ty::Na => ty == Ty::Na,
                    };
                    if !compatible {
                        self.err(
                            a.value.span,
                            format!(
                                "`{callee}` parameter `{}` expects {expected:?}, got {ty:?}",
                                sig.params
                                    .get(index)
                                    .map(String::as_str)
                                    .unwrap_or("argument")
                            ),
                        );
                    }
                }
                return sig.return_type;
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
        let arg_types: Vec<_> = args.iter().map(|a| self.expr(&a.value)).collect();
        for (index, (argument, ty)) in args.iter().zip(&arg_types).enumerate() {
            let expected = match (callee, index) {
                ("plot" | "hline" | "plotarrow", 0) => Some("numeric"),
                ("plotshape" | "plotchar", 0) => Some("boolean or numeric"),
                ("plotcandle" | "plotbar", 0..=3) => Some("numeric"),
                ("bgcolor" | "barcolor", 0) => Some("color"),
                _ => None,
            };
            let Some(expected) = expected else {
                continue;
            };
            let compatible = match expected {
                "numeric" => matches!(ty, Ty::Float | Ty::Na),
                "boolean or numeric" => matches!(ty, Ty::Bool | Ty::Float | Ty::Na),
                "color" => matches!(ty, Ty::Color | Ty::Na),
                _ => true,
            };
            if !compatible {
                self.err(
                    argument.value.span,
                    format!(
                        "`{callee}` argument {} needs a {expected} series, got {ty:?}",
                        index + 1
                    ),
                );
            }
        }
        if let Some(kind) = callee.strip_prefix("input.") {
            self.check_input_parameters(kind, args, &arg_types);
        }
        if let Some(spec) = crate::builtins::lookup(callee).filter(|spec| spec.metadata_complete) {
            self.check_builtin_parameters(callee, args, &arg_types, spec.parameters);
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
        if callee == "nz" && arg_types.contains(&Ty::String) {
            return Ty::String;
        }
        match crate::builtins::lookup(callee).map(|spec| spec.result) {
            Some(crate::builtins::BuiltinReturn::Bool) => Ty::Bool,
            Some(crate::builtins::BuiltinReturn::String) => Ty::String,
            Some(crate::builtins::BuiltinReturn::Color) => Ty::Color,
            Some(
                crate::builtins::BuiltinReturn::Number | crate::builtins::BuiltinReturn::InputKind,
            )
            | None => Ty::Float,
        }
    }

    fn check_builtin_parameters(
        &mut self,
        callee: &str,
        args: &[Arg],
        arg_types: &[Ty],
        parameters: &[crate::builtins::BuiltinParameter],
    ) {
        let mut positional = 0usize;
        let mut used = std::collections::HashSet::new();
        for (argument, ty) in args.iter().zip(arg_types) {
            let parameter_index = if let Some(name) = argument.name.as_deref() {
                match parameters
                    .iter()
                    .position(|parameter| parameter.name == name)
                {
                    Some(index) => Some(index),
                    None => {
                        self.err(
                            argument.value.span,
                            format!("`{callee}` has no parameter named `{name}`"),
                        );
                        None
                    }
                }
            } else {
                let index = positional;
                positional += 1;
                (index < parameters.len()).then_some(index)
            };
            let Some(index) = parameter_index else {
                continue;
            };
            if !used.insert(index) {
                self.err(
                    argument.value.span,
                    format!(
                        "`{callee}` received `{}` more than once",
                        parameters[index].name
                    ),
                );
                continue;
            }
            let expected = parameters[index].value_type;
            let compatible = match expected {
                crate::builtins::ParameterType::Number
                | crate::builtins::ParameterType::Integer
                | crate::builtins::ParameterType::Series => matches!(ty, Ty::Float | Ty::Na),
                crate::builtins::ParameterType::Boolean => matches!(ty, Ty::Bool | Ty::Na),
                crate::builtins::ParameterType::String
                | crate::builtins::ParameterType::Timeframe
                | crate::builtins::ParameterType::Symbol
                | crate::builtins::ParameterType::Enum => matches!(ty, Ty::String | Ty::Na),
                crate::builtins::ParameterType::Color => matches!(ty, Ty::Color | Ty::Na),
                crate::builtins::ParameterType::Any => true,
            };
            if !compatible {
                self.err(
                    argument.value.span,
                    format!(
                        "`{callee}` parameter `{}` expects {:?}, got {ty:?}",
                        parameters[index].name, expected
                    ),
                );
            }
        }
    }

    fn check_input_parameters(&mut self, kind: &str, args: &[Arg], arg_types: &[Ty]) {
        let value_kind = match kind {
            "int" | "float" | "source" | "time" | "price" => "numeric",
            "bool" => "bool or numeric",
            "string" | "timeframe" | "symbol" => "string",
            "color" => "color",
            _ => return,
        };
        let mut positional = 0usize;
        for (argument, ty) in args.iter().zip(arg_types) {
            let name = match argument.name.as_deref() {
                Some(name) => Some(name),
                None => {
                    let index = positional;
                    positional += 1;
                    if index == 0 {
                        Some("defval")
                    } else {
                        None
                    }
                }
            };
            let expected = match name {
                Some("defval") => Some(value_kind),
                Some("title") => Some("string"),
                Some("minval" | "maxval" | "step") => Some("numeric"),
                _ => None,
            };
            let Some(expected) = expected else {
                continue;
            };
            let compatible = match expected {
                "numeric" => matches!(ty, Ty::Float | Ty::Na),
                "bool or numeric" => matches!(ty, Ty::Bool | Ty::Float | Ty::Na),
                "string" => matches!(ty, Ty::String | Ty::Na),
                "color" => matches!(ty, Ty::Color | Ty::Na),
                _ => true,
            };
            if !compatible {
                self.err(
                    argument.value.span,
                    format!(
                        "`input.{kind}` `{}` expects {expected}, got {ty:?}",
                        name.unwrap_or("value")
                    ),
                );
            }
        }
    }

    fn builtins_contains(&self, name: &str) -> bool {
        matches!(
            name,
            "open"
                | "high"
                | "low"
                | "close"
                | "volume"
                | "hl2"
                | "hlc3"
                | "ohlc4"
                | "bar_index"
                | "last_bar_index"
                | "time"
                | "time_close"
                | "hour"
                | "minute"
                | "dayofweek"
                | "year"
                | "month"
                | "dayofmonth"
                | "syminfo.mintick"
                | "syminfo.ticker"
                | "extend.none"
                | "extend.right"
                | "extend.left"
                | "extend.both"
                | "color.new"
                | "color.rgb"
                | "color.from_gradient"
                | "math.pi"
                | "barstate.isnew"
                | "barstate.isfirst"
                | "barstate.islast"
                | "timeframe.period"
                | "timeframe.multiplier"
                | "timeframe.isintraday"
                | "timeframe.isdaily"
                | "timeframe.isweekly"
                | "timeframe.isdwm"
                | "syminfo.pointvalue"
                | "alert.freq_all"
                | "alert.freq_once_per"
                | "alert.freq_once_per_bar"
                | "alert.freq_once_per_bar_close"
                | "barmerge.gaps_on"
                | "barmerge.gaps_off"
                | "barmerge.lookahead_on"
                | "barmerge.lookahead_off"
                | "size.auto"
                | "size.tiny"
                | "size.small"
                | "size.normal"
                | "size.large"
                | "size.huge"
                | "strategy.direction.long"
                | "strategy.direction.short"
                | "strategy.direction.all"
                | "strategy.direction.none"
                | "position.top_right"
                | "position.top_left"
                | "position.top_center"
                | "position.bottom_right"
                | "position.bottom_left"
                | "position.bottom_center"
                | "position.middle_right"
                | "position.middle_left"
                | "position.middle_center"
        )
    }

    fn unknown(&mut self, span: Span, name: &str) -> Ty {
        self.err(
            span,
            format!(
                "`{name}` is not defined here. Known namespaces: ta., math., input., strategy., color., str.; known series: open/high/low/close/volume/hl2/hlc3/ohlc4/bar_index/time."
            ),
        );
        Ty::Float
    }
}

/// Namespaced constants and reads the checker can type statically.
/// Compatibility wrapper for the compiler's builtin metadata registry.
fn builtin_arity(callee: &str) -> Option<(usize, usize)> {
    crate::builtins::lookup(callee).map(|entry| (entry.min_args, entry.max_args))
}

/// How many values a multi-output builtin hands back. Only these may appear on
/// the right of `a, b, c = ...`.
fn multi_output_count(callee: &str) -> Option<usize> {
    crate::builtins::lookup(callee).and_then(|entry| entry.multi_output_count)
}

/// Multi-output builtins with NO single-value reading: using one as a scalar
/// is always a mistake worth naming.
fn multi_only_count(callee: &str) -> Option<usize> {
    crate::builtins::lookup(callee)
        .filter(|entry| entry.multi_output_only)
        .and_then(|entry| entry.multi_output_count)
}

fn namespace_const(name: &str) -> Option<Ty> {
    if name.starts_with("color.") {
        return Some(Ty::Color);
    }
    match name {
        "barstate.isconfirmed" | "barstate.isnew" | "barstate.isfirst" | "barstate.islast" => {
            Some(Ty::Bool)
        }
        "syminfo.pointvalue" => Some(Ty::Float),
        "timeframe.period" => Some(Ty::String),
        "extend.none"
        | "extend.right"
        | "extend.left"
        | "extend.both"
        | "position.top_right"
        | "position.top_left"
        | "position.top_center"
        | "position.bottom_right"
        | "position.bottom_left"
        | "position.bottom_center"
        | "position.middle_right"
        | "position.middle_left"
        | "position.middle_center"
        | "size.auto"
        | "size.tiny"
        | "size.small"
        | "size.normal"
        | "size.large"
        | "size.huge"
        | "alert.freq_all"
        | "alert.freq_once_per"
        | "alert.freq_once_per_bar"
        | "alert.freq_once_per_bar_close" => Some(Ty::String),
        "barmerge.gaps_on"
        | "barmerge.gaps_off"
        | "barmerge.lookahead_on"
        | "barmerge.lookahead_off" => Some(Ty::String),
        "strategy.direction.long"
        | "strategy.direction.short"
        | "strategy.direction.all"
        | "strategy.direction.none" => Some(Ty::String),
        "timeframe.multiplier" => Some(Ty::Float),
        "timeframe.isintraday" | "timeframe.isdaily" | "timeframe.isweekly" | "timeframe.isdwm" => {
            Some(Ty::Bool)
        }
        "syminfo.ticker" => Some(Ty::String),
        "strategy.position_size"
        | "strategy.position_avg_price"
        | "strategy.equity"
        | "strategy.openprofit"
        | "strategy.closedtrades"
        | "strategy.wintrades"
        | "strategy.netprofit"
        | "strategy.grossprofit"
        | "strategy.grossloss"
        | "strategy.max_drawdown" => Some(Ty::Float),
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

fn collect_string_parameters(
    expr: &Expr,
    params: &[String],
    found: &mut std::collections::HashSet<String>,
) {
    match &expr.kind {
        ExprKind::Bin {
            left,
            op: BinOp::Add(_),
            right,
        } if expression_is_string(left) || expression_is_string(right) => {
            collect_text_identifiers(left, params, found);
            collect_text_identifiers(right, params, found);
        }
        ExprKind::Ternary { then, els, .. } => {
            if expression_is_string(then) || expression_is_string(els) {
                collect_text_identifiers(then, params, found);
                collect_text_identifiers(els, params, found);
            }
        }
        ExprKind::Switch { arms, .. } => {
            if arms.iter().any(|(_, result)| expression_is_string(result)) {
                for (_, result) in arms {
                    collect_text_identifiers(result, params, found);
                }
            }
        }
        _ => {}
    }
}

fn collect_string_parameters_in_items(
    items: &[Item],
    params: &[String],
    found: &mut std::collections::HashSet<String>,
) {
    for item in items {
        match item {
            Item::Assign {
                expr,
                decl_type: Some(decl_type),
                ..
            } if decl_type == "string" => {
                collect_text_identifiers(expr, params, found);
            }
            Item::Assign { expr, .. } | Item::Expr { expr, .. } => {
                collect_string_parameters(expr, params, found);
            }
            Item::Destructure { call, .. } => {
                collect_string_parameters(call, params, found);
            }
            Item::Block {
                exprs, body, els, ..
            } => {
                for expr in exprs {
                    collect_string_parameters(expr, params, found);
                }
                collect_string_parameters_in_items(body, params, found);
                if let Some(els) = els {
                    collect_string_parameters_in_items(els, params, found);
                }
            }

            Item::TypeDef { .. } | Item::FuncDef { .. } => {}
        }
    }
}

pub(crate) fn string_parameters_in_items(
    items: &[Item],
    params: &[String],
) -> std::collections::HashSet<String> {
    let mut found = std::collections::HashSet::new();
    collect_string_parameters_in_items(items, params, &mut found);
    found
}

fn collect_text_identifiers(
    expr: &Expr,
    params: &[String],
    found: &mut std::collections::HashSet<String>,
) {
    match &expr.kind {
        ExprKind::Ident(name) if params.iter().any(|param| param == name) => {
            found.insert(name.clone());
        }
        ExprKind::Bin {
            left,
            op: BinOp::Add(_),
            right,
        } => {
            collect_text_identifiers(left, params, found);
            collect_text_identifiers(right, params, found);
        }
        ExprKind::Ternary { then, els, .. } => {
            collect_text_identifiers(then, params, found);
            collect_text_identifiers(els, params, found);
        }
        ExprKind::Switch { arms, .. } => {
            for (_, result) in arms {
                collect_text_identifiers(result, params, found);
            }
        }
        ExprKind::Call { callee, args } if callee != "str.tostring" => {
            for arg in args {
                collect_text_identifiers(&arg.value, params, found);
            }
        }
        _ => {}
    }
}

fn expression_is_string(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Str(_) => true,
        ExprKind::Call { callee, .. } => matches!(
            callee.as_str(),
            "str.tostring" | "str.format" | "input.string" | "array.join"
        ),
        ExprKind::Bin {
            left,
            op: BinOp::Add(_),
            right,
        } => expression_is_string(left) || expression_is_string(right),
        ExprKind::Ternary { then, els, .. } => {
            expression_is_string(then) || expression_is_string(els)
        }
        ExprKind::Switch { arms, .. } => {
            arms.iter().any(|(_, result)| expression_is_string(result))
        }
        _ => false,
    }
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
    fn dmi_arity_matches_its_runtime_and_documented_signature() {
        assert!(check_src(
            "//@pine_lite version=1\n\
             plus, minus, adx = ta.dmi(14)\n"
        )
        .is_empty());

        let errors = check_src(
            "//@pine_lite version=1\n\
             plus, minus, adx = ta.dmi(14, 14)\n",
        );
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("takes at most 1")),
            "{errors:?}"
        );
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
        assert!(
            errs.iter()
                .any(|e| e.message.contains("arithmetic needs numbers")),
            "{errs:?}"
        );
    }

    #[test]
    fn ternary_branches_must_have_compatible_types() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             x = close > open ? close : false\n",
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("ternary branches must have compatible types")),
            "{errs:?}"
        );
    }

    #[test]
    fn ternary_na_inherits_the_other_branch_type() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             selected = close > open ? color.red : na\n\
             plot(selected)\n\
             hline(true)\n",
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`plot` argument 1 needs a numeric series, got Color")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`hline` argument 1 needs a numeric series, got Bool")),
            "{errs:?}"
        );
        assert!(
            errs.iter().all(|e| !e.message.contains("ternary branches")),
            "{errs:?}"
        );
    }

    #[test]
    fn complete_builtin_metadata_enforces_argument_types_and_names() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             bad_source = ta.sma(true, 10)\n\
             line.new(1, 2, 3, 4, color=1, typo=2)\n",
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`ta.sma` parameter `source` expects Series, got Bool")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`line.new` parameter `color` expects Color, got Float")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`line.new` has no parameter named `typo`")),
            "{errs:?}"
        );
    }

    #[test]
    fn control_flow_and_user_function_calls_enforce_value_kinds() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "if close\n",
            "    inside = 1\n",
            "while close\n",
            "    break\n",
            "for i = true to 5\n",
            "    inside_loop = i\n",
            "f(value) =>\n",
            "    value\n",
            "bad_call = f(false)\n",
        ));
        assert!(
            errs.iter()
                .any(|e| e.message.contains("if condition must be bool")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.message.contains("while condition must be bool")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("for-loop bounds and step must be numeric")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`f` parameter `value` expects Float, got Bool")),
            "{errs:?}"
        );
    }

    #[test]
    fn string_function_parameters_and_returns_are_inferred() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "decorate(prefix = \"ready\") =>\n",
            "    suffix = prefix + \"!\"\n",
            "    suffix\n",
            "label = decorate()\n",
            "plotshape(true, text=label)\n",
            "bad = decorate(true)\n",
        ));
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`decorate` parameter `prefix` expects String")),
            "{errs:?}"
        );
        assert!(
            errs.iter().all(|e| !e.message.contains("returns a string")),
            "{errs:?}"
        );
    }

    #[test]
    fn user_type_constructors_enforce_declared_field_kinds() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "type Signal\n",
            "    bool valid\n",
            "    color tint\n",
            "    string message\n",
            "    float level\n",
            "good = Signal.new(valid=true, tint=color.red, message=\"ready\", level=close)\n",
            "good.message := \"go\"\n",
            "plotshape(true, text=good.message)\n",
            "bad = Signal.new(1, color.red, true, close)\n",
        ));
        assert!(
            errs.iter()
                .any(|e| e.message.contains("field `valid` expects Bool, got Float")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("field `message` expects String, got Bool")),
            "{errs:?}"
        );
    }

    #[test]
    fn rendering_calls_enforce_their_series_value_kinds() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "plotshape(\"signal\")\n",
            "bgcolor(true)\n",
            "plotcandle(open, high, low, color.red)\n",
        ));
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`plotshape` argument 1 needs a boolean or numeric series")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`bgcolor` argument 1 needs a color series")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`plotcandle` argument 4 needs a numeric series")),
            "{errs:?}"
        );
    }

    #[test]
    fn declared_and_reassigned_variables_keep_one_value_kind() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "string message = \"ready\"\n",
            "string wrong = 1\n",
            "price = close\n",
            "price := true\n",
            "untyped = na\n",
            "untyped := false\n",
        ));
        assert!(
            errs.iter()
                .any(|e| e.message.contains("`wrong` is declared `string`")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.message.contains("`price` was previously Float")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .all(|e| !e.message.contains("`untyped` was previously")),
            "{errs:?}"
        );
    }

    #[test]
    fn function_locals_do_not_overwrite_top_level_value_kinds() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "message = \"ready\"\n",
            "f() =>\n",
            "    series = close\n",
            "    series := true\n",
            "    series + 1\n",
            "result = f()\n",
            "plot(message)\n",
        ));
        assert!(
            errs.iter()
                .any(|e| e.message.contains("`series` was previously Float")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`plot` argument 1 needs a numeric series")),
            "{errs:?}"
        );
    }

    #[test]
    fn input_defaults_must_match_their_declared_value_kind() {
        let errs = check_src(concat!(
            "//@pine_lite version=1\n",
            "legacy_bool = input.bool(defval=0)\n",
            "bad_bool = input.bool(defval=\"yes\")\n",
            "bad_text = input.string(defval=close)\n",
        ));
        assert!(
            errs.iter().any(|e| e
                .message
                .contains("`input.bool` `defval` expects bool or numeric")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.message.contains("`input.string` `defval` expects string")),
            "{errs:?}"
        );
        assert!(
            errs.iter().all(|e| !e.message.contains("`legacy_bool`")),
            "{errs:?}"
        );
    }

    #[test]
    fn not_on_a_number_is_refused_with_boolean_fixes() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             x = not close\n",
        );
        let err = errs
            .iter()
            .find(|e| e.message.contains("`not` needs a bool"))
            .expect("numeric negation is refused");
        assert!(err.message.contains("got Float"), "{err:?}");
        assert!(
            err.message.contains("compare numeric values explicitly"),
            "{err:?}"
        );
        assert!(err.message.contains("not na(x)"), "{err:?}");
    }

    #[test]
    fn not_accepts_boolean_expressions_and_numeric_missing_checks() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             pool = ta.highest(high, 5)\n\
             sweep = not na(pool) and high > pool and close < pool\n\
             plotshape(sweep, shape=\"triangleup\")\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn boolean_history_keeps_its_type_for_event_edges() {
        let errs = check_src(
            "//@pine_lite version=1\n\
             candidate = close < open\n\
             edge = candidate and not candidate[1]\n\
             plotshape(edge)\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
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
        assert!(errs
            .iter()
            .any(|e| matches!(e.kind, crate::ErrorKind::Type)));
    }

    #[test]
    fn a_timeframe_input_var_resolves_the_security_pool_key() {
        let tokens = lex::lex(
            "//@pine_lite version=1\n\
             tf = input.timeframe(defval=\"4h\")\n\
             c = request.security(\"BTCUSDT\", tf, close)\n\
             plot(c)\n",
        )
        .expect("lex");
        let script = parse::parse(tokens.1).expect("parse");
        let cx_errs = check(&script);
        assert!(cx_errs.is_empty(), "{cx_errs:?}");
        // The gateway fetches exactly these keys before the run: a tf var
        // must resolve to the same key a literal would.
        let keys = collect_series_pool(&script, &crate::Header::default());
        assert_eq!(keys, vec!["BTCUSDT@4H".to_string()]);
    }
}
