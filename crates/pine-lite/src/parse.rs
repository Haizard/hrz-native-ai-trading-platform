//! Parser: tokens to an AST with spans.
//!
//! Statements are newline-terminated (Pine's rule; no semicolons), blocks are
//! indentation-driven, and every error carries a span. Like the lexer, the
//! parser collects errors rather than stopping at the first, with one
//! exception recorded in [`parse`]: after a hard structural break it skips to
//! the next line so one broken statement does not cascade into hundreds of
//! phantom errors -- the repair loop reads the list, and noise buries signal.

use crate::lex::{Token, TokenKind};
use crate::{ErrorKind, Header, ScriptError, Span};

/// A whole parsed script: the header plus the statement list.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    /// The `//@pine_lite` knobs, copied from the lexer.
    pub header: Header,
    /// Top-level statements, in source order.
    pub items: Vec<Item>,
}

/// A top-level statement. Pine has no top-level `return`; the body is the
/// script.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `r = ta.rsi(close, 14)` or `var count = 0`.
    Assign {
        /// Where the statement starts.
        span: Span,
        /// Target name.
        name: String,
        /// `var` / `varip` / plain (re-assigned each bar).
        mode: VarMode,
        /// Compound operator, when the target already exists.
        op: Option<String>,
        /// The right-hand side.
        expr: Expr,
    },
    /// `basis, upper, lower = ta.bb(close, 20, 2)`: one multi-output call,
    /// several names. The count is checked against the callee at vet time.
    Destructure {
        /// Where the statement starts.
        span: Span,
        /// Targets, left to right.
        names: Vec<String>,
        /// The call whose outputs are taken apart.
        call: Expr,
    },
    /// `if` / `for` / `while` with an indented body.
    Block {
        /// Where the statement starts.
        span: Span,
        /// Which block.
        kind: BlockKind,
        /// The condition(s); `for` has three (from, to, step).
        exprs: Vec<Expr>,
        /// The loop variable, when this is a `for`.
        loop_var: Option<String>,
        /// The indented body.
        body: Vec<Item>,
        /// The `else` body, when present.
        els: Option<Vec<Item>>,
    },
    /// A user-defined function: `f(x) =>` with an indented body.
    FuncDef {
        /// Where the definition starts.
        span: Span,
        /// Function name.
        name: String,
        /// Parameter names.
        params: Vec<String>,
        /// The indented body; the last expression is the return value.
        body: Vec<Item>,
    },
    /// A bare call statement whose value is discarded (`plot(...)`,
    /// `strategy.entry(...)`, or any expression).
    Expr {
        /// Where the statement starts.
        span: Span,
        /// The expression.
        expr: Expr,
    },
}

/// Which block statement this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// `if cond` (the `exprs` list has one).
    If,
    /// `for i = a to b [by c]` (three exprs: init, limit, step).
    For,
    /// `while cond` (one expr; the analyser refuses it, see `docs/23`).
    While,
}

/// How a variable accumulates across bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarMode {
    /// Recomputed every bar (the default; no keyword).
    Auto,
    /// `var` -- initialized once, carried across bars.
    Var,
    /// `varip` -- like `var`, and updates intrabar in Pine; this platform runs
    /// on closed bars only, so it behaves as `var` and the analyser notes it.
    Varip,
}

/// An expression, with the span of its first token for error messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// Shape.
    pub kind: ExprKind,
    /// Where it starts.
    pub span: Span,
}

/// Expression shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A numeric literal.
    Num(f64),
    /// A string literal.
    Str(String),
    /// A packed-RGBA color literal.
    Color(u32),
    /// `true` / `false`.
    Bool(bool),
    /// `na`.
    Na,
    /// `na(x)`: the Pine test form. Evaluates to true where the value is
    /// missing. (The bare keyword stays `Na`.)
    NaChecked {
        /// The tested expression.
        value: Box<Expr>,
    },

    /// An identifier: a variable, a builtin, or a dotted path (`ta.rsi`,
    /// `input.int`, `strategy.entry`, `color.red`, `barstate.isconfirmed`).
    Ident(String),
    /// Binary operator: arithmetic, comparison, `and`/`or` (kept as words in
    /// [`ExprKind::Ident`]-free form: the parser lowers the keywords to ops).
    Bin {
        /// Left operand.
        left: Box<Expr>,
        /// The operator.
        op: BinOp,
        /// Right operand.
        right: Box<Expr>,
    },
    /// Unary `-` / `+` / `not`.
    Un {
        /// The operator.
        op: UnOp,
        /// The operand.
        expr: Box<Expr>,
    },
    /// Ternary `cond ? a : b`.
    Ternary {
        /// The condition.
        cond: Box<Expr>,
        /// Then-value.
        then: Box<Expr>,
        /// Else-value.
        els: Box<Expr>,
    },
    /// History index: `expr[n]`.
    History {
        /// The series expression.
        base: Box<Expr>,
        /// Bars back; evaluated but expected to be a literal or simple int.
        offset: Box<Expr>,
    },
    /// A call: builtin, dotted builtin, or user function.
    Call {
        /// Callee name (dotted paths stay whole).
        callee: String,
        /// Arguments, positional or `name=value`.
        args: Vec<Arg>,
    },

    /// A member read that is not a call: `strategy.position_size`,
    /// `barstate.isconfirmed`.
    Member {
        /// The whole dotted path.
        path: String,
    },
}

/// Binary operators, in precedence order (loosest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `or`
    Or,
    /// `and`
    And,
    /// `==` `!=` `<` `<=` `>` `>=`
    Cmp(CmpOp),
    /// `+` `-`
    Add(AddOp),
    /// `*` `/` `%`
    Mul(MulOp),
    /// `**`
    Pow,
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// Additive operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOp {
    /// `+`
    Add,
    /// `-`
    Sub,
}

/// Multiplicative operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MulOp {
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-`
    Neg,
    /// `+` (no-op, kept for symmetry)
    Pos,
    /// `not`
    Not,
}

/// One call argument: positional or named (`title="RSI"`).
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// The name, when written `name=value`.
    pub name: Option<String>,
    /// The value.
    pub value: Expr,
}

/// Parse a token stream into a [`Script`].
///
/// The lexer already split source into lines (each ending with `Newline`), so
/// the parser tracks indentation as the token count from the line start the
/// lexer recorded in each token's span. An indentation increase opens a block;
/// back to a previous level closes it -- the standard Python/Pine rule.
pub fn parse(tokens: Vec<Token>) -> Result<Script, Vec<ScriptError>> {
    let mut p = Parser { tokens, pos: 0, errors: Vec::new() };
    let items = p.items(0);
    if p.errors.is_empty() {
        // The lexer parsed the header; the parser carries the default until
        // `vet` re-attaches the real one.
        Ok(Script { header: Header::default(), items })
    } else {
        Err(p.errors)
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<ScriptError>,
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(self.tokens.last().expect("eof token"))
    }
    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn err(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(ScriptError {
            kind: ErrorKind::Parse,
            span,
            message: message.into(),
        });
    }

    /// Parse statements until indentation drops below `min_indent` or EOF.
    fn items(&mut self, min_indent: usize) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            // Skip blank lines (consecutive Newlines).
            while matches!(self.peek().kind, TokenKind::Newline) {
                self.bump();
            }
            if matches!(self.peek().kind, TokenKind::Eof) {
                break;
            }
            let indent = self.peek().span.col.saturating_sub(1);
            if indent < min_indent {
                break;
            }
            let before = self.errors.len();
            match self.statement(indent) {
                Some(item) => items.push(item),
                None => {
                    // Hard break: if no progress was made, skip to the next
                    // line so one bad statement cannot loop the parser.
                    if self.errors.len() == before {
                        self.err(self.peek().span, "unexpected token");
                    }
                    self.skip_line();
                }
            }
        }
        items
    }

    fn skip_line(&mut self) {
        while !matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof) {
            self.bump();
        }
        if matches!(self.peek().kind, TokenKind::Newline) {
            self.bump();
        }
    }

    fn statement(&mut self, indent: usize) -> Option<Item> {
        let span = self.peek().span;
        match self.peek().kind.clone() {
            TokenKind::Keyword(k) if k == "if" => Some(self.if_stmt(indent, span)),
            TokenKind::Keyword(k) if k == "for" => Some(self.for_stmt(indent, span)),
            TokenKind::Keyword(k) if k == "var" || k == "varip" => {
                self.bump();
                Some(self.var_decl(span, k))
            }
            TokenKind::Keyword(k) if k == "while" => {
                self.bump();
                let cond = self.expr()?;
                let body = self.block_body(indent);
                Some(Item::Block {
                    span,
                    kind: BlockKind::While,
                    exprs: vec![cond],
                    loop_var: None,
                    body,
                    els: None,
                })
            }
            TokenKind::Ident(ref name) if matches!(self.peek_ahead(1).kind, TokenKind::LParen) => {
                // `name(...)`: a user function *definition* ends with `=>`,
                // a call is everything else.
                let name = name.clone();
                if self.looks_like_funcdef() {
                    Some(self.funcdef(span, name))
                } else {
                    let expr = self.expr()?;
                    self.end_of_line();
                    Some(Item::Expr { span, expr })
                }
            }
            // `a, b = ta.macd(...)`: names, commas, then one multi-output call.
            TokenKind::Ident(_) if matches!(self.peek_ahead(1).kind, TokenKind::Comma) => {
                Some(self.destructure(span))
            }
            TokenKind::Ident(ref name)
                if matches!(self.peek_ahead(1).kind, TokenKind::Assign(_)) =>
            {
                let name = name.clone();
                self.bump(); // name
                let op = match self.bump().kind {
                    TokenKind::Assign(op) => op,
                    _ => unreachable!("guarded above"),
                };
                // Reassignment carries no new declaration mode; the
                // declaration's `var`/`varip` already lives in `variables`.
                let mode = VarMode::Auto;
                let expr = self.expr()?;
                self.end_of_line();
                Some(Item::Assign { span, name, mode, op: Some(op), expr })
            }
            _ => {
                let expr = self.expr()?;
                self.end_of_line();
                Some(Item::Expr { span, expr })
            }
        }
    }

    /// `a, b, c = f(...)` -- the multi-value form. Called with the first name
    /// still at the cursor.
    fn destructure(&mut self, span: Span) -> Item {
        let mut names = Vec::new();
        loop {
            match self.bump().kind {
                TokenKind::Ident(n) => names.push(n),
                other => {
                    self.err(span, format!("a multi-value assignment needs names, found `{other:?}`"));
                    break;
                }
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        match self.bump().kind {
            TokenKind::Assign(op) if op == "=" => {}
            other => self.err(
                span,
                format!("a multi-value assignment needs `=`, found `{other:?}`"),
            ),
        }
        let call = self.expr().unwrap_or(Expr { span, kind: ExprKind::Na });
        self.end_of_line();
        Item::Destructure { span, names, call }
    }

    /// `var`/`varip` declarations: `var name = expr`. The caller has already
    /// consumed the keyword.
    fn var_decl(&mut self, span: Span, keyword: String) -> Item {
        let mode = if keyword == "var" { VarMode::Var } else { VarMode::Varip };
        let name = match self.bump().kind {
            TokenKind::Ident(n) => n,
            other => {
                self.err(span, format!("`{keyword}` needs a name, found `{other:?}`"));
                return Item::Expr { span, expr: Expr { kind: ExprKind::Na, span } };
            }
        };
        let op = match self.peek().kind {
            TokenKind::Assign(ref op) => {
                let op = op.clone();
                self.bump();
                Some(op)
            }
            _ => None,
        };
        let expr = self.expr().unwrap_or(Expr { kind: ExprKind::Na, span });
        self.end_of_line();
        Item::Assign { span, name, mode, op, expr }
    }

    fn if_stmt(&mut self, indent: usize, span: Span) -> Item {
        self.bump(); // `if`
        let cond = self.expr().unwrap_or(Expr { kind: ExprKind::Na, span });
        let body = self.block_body(indent);
        let mut els = None;
        // `else` may sit on the body's closing line or its own; the lexer's
        // newline-per-line rule means it is the next statement at `indent`.
        let save = self.pos;
        self.skip_blank();
        if matches!(self.peek().kind, TokenKind::Keyword(ref k) if k == "else")
            && self.peek().span.col.saturating_sub(1) == indent
        {
            self.bump(); // `else`
            if matches!(self.peek().kind, TokenKind::Keyword(ref k) if k == "if") {
                let nested = self.if_stmt(indent, self.peek().span);
                els = Some(vec![nested]);
            } else {
                els = Some(self.block_body(indent));
            }
        } else {
            self.pos = save;
        }
        Item::Block { span, kind: BlockKind::If, exprs: vec![cond], loop_var: None, body, els }
    }

    fn for_stmt(&mut self, indent: usize, span: Span) -> Item {
        self.bump(); // `for`
        let loop_var = match self.bump().kind {
            TokenKind::Ident(v) => Some(v),
            other => {
                self.err(span, format!("`for` needs a loop variable, found `{other:?}`"));
                None
            }
        };
        let _eq = match self.bump().kind {
            TokenKind::Assign(op) => op,
            other => {
                self.err(span, format!("`for` needs `=`, found `{other:?}`"));
                String::new()
            }
        };
        let from = self.expr().unwrap_or(Expr { kind: ExprKind::Na, span });
        let _to = match self.bump().kind {
            TokenKind::Keyword(k) if k == "to" => k,
            other => {
                self.err(span, format!("`for` needs `to`, found `{other:?}`"));
                String::new()
            }
        };
        let to = self.expr().unwrap_or(Expr { kind: ExprKind::Na, span });
        let mut step = Expr { kind: ExprKind::Num(1.0), span };
        if matches!(self.peek().kind, TokenKind::Keyword(ref k) if k == "by") {
            self.bump();
            step = self.expr().unwrap_or(step);
        }
        let body = self.block_body(indent);
        Item::Block {
            span,
            kind: BlockKind::For,
            exprs: vec![from, to, step],
            loop_var,
            body,
            els: None,
        }
    }

    fn funcdef(&mut self, span: Span, name: String) -> Item {
        self.bump(); // name
        self.bump(); // (
        let mut params = Vec::new();
        loop {
            match self.bump().kind {
                TokenKind::Ident(p) => params.push(p),
                TokenKind::RParen => break,
                TokenKind::Comma => {}
                other => {
                    self.err(span, format!("bad parameter `{other:?}`"));
                    break;
                }
            }
        }
        // `=>`
        match self.bump().kind {
            TokenKind::Op(op) if op == "=>" => {}
            other => self.err(span, format!("a function body starts with `=>`, found `{other:?}`")),
        }
        let body = self.block_body(span.col.saturating_sub(1));
        Item::FuncDef { span, name, params, body }
    }

    /// Does the current `name(...)` continue with `=>` on this line? A call's
    /// parens close before any `=>`; a definition's do not.
    fn looks_like_funcdef(&mut self) -> bool {
        let save = self.pos;
        self.bump(); // name
        self.bump(); // (
        let mut depth = 1usize;
        let mut result = false;
        while depth > 0 && !matches!(self.peek().kind, TokenKind::Eof | TokenKind::Newline) {
            match self.peek().kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth -= 1,
                _ => {}
            }
            self.bump();
        }
        if depth == 0 {
            // Optional newline inside a call's parens is legal; here we only
            // need "is the next significant token `=>`".
            self.skip_blank();
            result = matches!(self.peek().kind, TokenKind::Op(ref op) if op == "=>");
        }
        self.pos = save;
        result
    }

    fn skip_blank(&mut self) {
        while matches!(self.peek().kind, TokenKind::Newline) {
            self.bump();
        }
    }

    /// The indented body of a block statement: statements whose indentation is
    /// greater than the opener's.
    fn block_body(&mut self, opener_indent: usize) -> Vec<Item> {
        let save = self.pos;
        self.skip_blank();
        let body_indent = self.peek().span.col.saturating_sub(1);
        if body_indent <= opener_indent {
            // `if cond` with no body: an error at the *caller's* span would be
            // better, but the body parser has no span; the caller's `items`
            // loop reports the empty statement instead.
            self.pos = save;
            return Vec::new();
        }
        self.items(body_indent)
    }

    /// Statements end at a newline. A missing one (two statements on a line)
    /// is an error naming the rule.
    fn end_of_line(&mut self) {
        match self.peek().kind {
            TokenKind::Newline | TokenKind::Eof => {
                if matches!(self.peek().kind, TokenKind::Newline) {
                    self.bump();
                }
            }
            _ => self.err(
                self.peek().span,
                "one statement per line; statements end at a newline",
            ),
        }
    }

    fn peek_ahead(&self, n: usize) -> &Token {
        self.tokens
            .get(self.pos + n)
            .unwrap_or(self.tokens.last().expect("eof token"))
    }

    // ---- expression precedence climbing, loosest to tightest ----

    fn expr(&mut self) -> Option<Expr> {
        self.ternary()
    }

    fn ternary(&mut self) -> Option<Expr> {
        let cond = self.or()?;
        if matches!(self.peek().kind, TokenKind::Question) {
            self.bump();
            let then = self.expr()?;
            match self.bump().kind {
                TokenKind::Colon => {}
                other => self.err(self.peek().span, format!("`?:` needs `:`, found `{other:?}`")),
            }
            let els = self.expr()?;
            let span = cond.span;
            return Some(Expr {
                span,
                kind: ExprKind::Ternary {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                },
            });
        }
        Some(cond)
    }

    fn or(&mut self) -> Option<Expr> {
        let mut left = self.and()?;
        while matches!(self.peek().kind, TokenKind::Keyword(ref k) if k == "or") {
            self.bump();
            let right = self.and()?;
            let span = left.span;
            left = Expr {
                span,
                kind: ExprKind::Bin {
                    left: Box::new(left),
                    op: BinOp::Or,
                    right: Box::new(right),
                },
            };
        }
        Some(left)
    }

    fn and(&mut self) -> Option<Expr> {
        let mut left = self.comparison()?;
        while matches!(self.peek().kind, TokenKind::Keyword(ref k) if k == "and") {
            self.bump();
            let right = self.comparison()?;
            let span = left.span;
            left = Expr {
                span,
                kind: ExprKind::Bin {
                    left: Box::new(left),
                    op: BinOp::And,
                    right: Box::new(right),
                },
            };
        }
        Some(left)
    }

    fn comparison(&mut self) -> Option<Expr> {
        let mut left = self.additive()?;
        while let TokenKind::Cmp(op) = self.peek().kind.clone() {
            self.bump();
            let right = self.additive()?;
            let span = left.span;
            let op = match op.as_str() {
                "==" => CmpOp::Eq,
                "!=" => CmpOp::Ne,
                "<" => CmpOp::Lt,
                "<=" => CmpOp::Le,
                ">" => CmpOp::Gt,
                _ => CmpOp::Ge,
            };
            left = Expr {
                span,
                kind: ExprKind::Bin {
                    left: Box::new(left),
                    op: BinOp::Cmp(op),
                    right: Box::new(right),
                },
            };
        }
        Some(left)
    }

    fn additive(&mut self) -> Option<Expr> {
        let mut left = self.multiplicative()?;
        while let TokenKind::Op(op) = self.peek().kind.clone() {
            let add = match op.as_str() {
                "+" => AddOp::Add,
                "-" => AddOp::Sub,
                _ => break,
            };
            self.bump();
            let right = self.multiplicative()?;
            let span = left.span;
            left = Expr {
                span,
                kind: ExprKind::Bin {
                    left: Box::new(left),
                    op: BinOp::Add(add),
                    right: Box::new(right),
                },
            };
        }
        Some(left)
    }

    fn multiplicative(&mut self) -> Option<Expr> {
        let mut left = self.unary()?;
        while let TokenKind::Op(op) = self.peek().kind.clone() {
            let mul = match op.as_str() {
                "*" => MulOp::Mul,
                "/" => MulOp::Div,
                "%" => MulOp::Rem,
                _ => break,
            };
            self.bump();
            let right = self.unary()?;
            let span = left.span;
            left = Expr {
                span,
                kind: ExprKind::Bin {
                    left: Box::new(left),
                    op: BinOp::Mul(mul),
                    right: Box::new(right),
                },
            };
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Expr> {
        let span = self.peek().span;
        match self.peek().kind.clone() {
            TokenKind::Op(op) if op == "-" => {
                self.bump();
                let expr = self.unary()?;
                return Some(Expr { span, kind: ExprKind::Un { op: UnOp::Neg, expr: Box::new(expr) } });
            }
            TokenKind::Op(op) if op == "+" => {
                self.bump();
                let expr = self.unary()?;
                return Some(Expr { span, kind: ExprKind::Un { op: UnOp::Pos, expr: Box::new(expr) } });
            }
            TokenKind::Keyword(k) if k == "not" => {
                self.bump();
                let expr = self.unary()?;
                return Some(Expr { span, kind: ExprKind::Un { op: UnOp::Not, expr: Box::new(expr) } });
            }
            _ => {}
        }
        self.power()
    }

    fn power(&mut self) -> Option<Expr> {
        let mut left = self.postfix()?;
        while matches!(self.peek().kind, TokenKind::Op(ref op) if op == "**") {
            self.bump();
            let right = self.unary()?;
            let span = left.span;
            left = Expr {
                span,
                kind: ExprKind::Bin { left: Box::new(left), op: BinOp::Pow, right: Box::new(right) },
            };
        }
        Some(left)
    }

    fn postfix(&mut self) -> Option<Expr> {
        let mut expr = self.primary()?;
        while matches!(self.peek().kind, TokenKind::LBracket) {
            self.bump();
            let offset = self.expr()?;
            match self.bump().kind {
                TokenKind::RBracket => {}
                other => {
                    self.err(self.peek().span, format!("history index needs `]`, found `{other:?}`"))
                }
            }
            let span = expr.span;
            expr = Expr {
                span,
                kind: ExprKind::History { base: Box::new(expr), offset: Box::new(offset) },
            };
        }
        Some(expr)
    }

    fn primary(&mut self) -> Option<Expr> {
        let span = self.peek().span;
        match self.bump().kind {
            TokenKind::Number(n) => Some(Expr { span, kind: ExprKind::Num(n) }),
            TokenKind::Str(s) => Some(Expr { span, kind: ExprKind::Str(s) }),
            TokenKind::Color(c) => Some(Expr { span, kind: ExprKind::Color(c) }),
            TokenKind::Keyword(k) if k == "true" => Some(Expr { span, kind: ExprKind::Bool(true) }),
            TokenKind::Keyword(k) if k == "false" => Some(Expr { span, kind: ExprKind::Bool(false) }),
            TokenKind::Keyword(k) if k == "na" => {
                // `na` is both the literal and Pine's test function: allow the
                // call form `na(x)` alongside the bare literal.
                if matches!(self.peek().kind, TokenKind::LParen) {
                    self.bump(); // (
                    let inner = self.expr()?;
                    match self.bump().kind {
                        TokenKind::RParen => {}
                        other => self.err(self.peek().span, format!("closing `)` for `na(...)`, found `{other:?}`")),
                    }
                    Some(Expr { span, kind: ExprKind::NaChecked { value: Box::new(inner) } })
                } else {
                    Some(Expr { span, kind: ExprKind::Na })
                }
            }
            TokenKind::Keyword(k) if k == "var" || k == "varip" => {
                // A declaration mid-expression position is a statement; treat
                // it as one by handing back to the statement parser via a
                // synthetic item is not possible here, so record the error.
                self.err(span, "`var` declarations start a statement, not an expression");
                None
            }
            TokenKind::Ident(name) => {
                if matches!(self.peek().kind, TokenKind::LParen) {
                    self.bump(); // (
                    let mut args = Vec::new();
                    loop {
                        if matches!(self.peek().kind, TokenKind::RParen) {
                            self.bump();
                            break;
                        }
                        // Named argument?
                        let name_arg = if let TokenKind::Ident(n) = self.peek().kind.clone() {
                            if matches!(self.peek_ahead(1).kind, TokenKind::Assign(ref op) if op == "=")
                            {
                                self.bump();
                                self.bump(); // =
                                Some(n)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let value = self.expr()?;
                        args.push(Arg { name: name_arg, value });
                        match self.bump().kind {
                            TokenKind::Comma => {}
                            TokenKind::RParen => break,
                            other => {
                                self.err(
                                    self.peek().span,
                                    format!("`, or ) in a call, found `{other:?}`"),
                                );
                                break;
                            }
                        }
                    }
                    Some(Expr { span, kind: ExprKind::Call { callee: name, args } })
                } else {
                    Some(Expr { span, kind: ExprKind::Ident(name) })
                }
            }
            TokenKind::LParen => {
                let inner = self.expr()?;
                match self.bump().kind {
                    TokenKind::RParen => {}
                    other => {
                        self.err(self.peek().span, format!("closing `)`, found `{other:?}`"))
                    }
                }
                Some(inner)
            }
            other => {
                self.err(span, format!("unexpected token `{other:?}` in an expression"));
                None
            }
        }
    }
}
