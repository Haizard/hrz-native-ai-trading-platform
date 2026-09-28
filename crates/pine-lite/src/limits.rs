//! Static limits: the budgets that make "anything" safe to run.
//!
//! The sandbox's fuel metering is the *dynamic* half; this is the static half.
//! A script that declares 500 plots or indexes 10,000 bars back is refused
//! before it runs, with a message that names the budget -- the same shape of
//! refusal `chart_engine::indicator::IndicatorOutput::validate` gives for
//! primitive counts.

use crate::parse::{BlockKind, Expr, ExprKind, Item, Script};

/// The budgets, from `docs/23`.
pub const MAX_PLOTS: usize = 64;
pub const MAX_FILLS: usize = 8;
pub const MAX_STATEMENTS: usize = 500;
pub const MAX_FUNCTIONS: usize = 32;
pub const MAX_LOOP_DEPTH: usize = 4;

/// Check the budgets. Statement counting is recursive: a body counts each of
/// its statements, because fuel cost is what the cap is really about.
pub fn check(script: &Script) -> Vec<crate::ScriptError> {
    let mut cx = Limiter { errors: Vec::new(), statements: 0, plots: 0, fills: 0 };
    cx.count(&script.items);
    cx.errors
}

struct Limiter {
    errors: Vec<crate::ScriptError>,
    statements: usize,
    plots: usize,
    fills: usize,
}

impl Limiter {
    fn fail(&mut self, span: crate::Span, message: &str) {
        self.errors.push(crate::ScriptError {
            kind: crate::ErrorKind::Limit,
            span,
            message: message.to_string(),
        });
    }

    fn count(&mut self, items: &[Item]) {
        for item in items {
            self.statements += 1;
            if self.statements > MAX_STATEMENTS {
                self.fail(
                    span_of(item),
                    "more than 500 statements; a script is a measurement, not a program",
                );
            }
            match item {
                Item::Assign { span, expr, .. } => {
                    self.expr(expr);
                    let _ = span;
                }
                Item::Expr { span, expr } => {
                    if let ExprKind::Call { callee, .. } = &expr.kind {
                        if callee == "fill" {
                            self.fills += 1;
                            if self.fills > MAX_FILLS {
                                self.fail(*span, "more than 8 fill() calls");
                            }
                        } else if callee.starts_with("plot")
                            || callee == "hline"
                            || callee == "bgcolor"
                            || callee == "barcolor"
                        {
                            self.plots += 1;
                            if self.plots > MAX_PLOTS {
                                self.fail(*span, "more than 64 plots");
                            }
                        }
                    }
                    self.expr(expr);
                }
                Item::Block { span, kind, exprs, body, els, .. } => {
                    for e in exprs {
                        self.expr(e);
                    }
                    if *kind == BlockKind::While {
                        // v1 refuses `while` outright: "provably terminating"
                        // is a proof obligation, not a lint.
                        self.fail(
                            *span,
                            "`while` is refused in v1; use `for i = a to b [by c]`",
                        );
                    }
                    self.count(body);
                    if let Some(els) = els {
                        self.count(els);
                    }
                }
                Item::FuncDef { span, body, .. } => {
                    if self.statements > MAX_STATEMENTS {
                        self.fail(*span, "more than 500 statements");
                    }
                    self.count(body);
                }
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Bin { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Un { expr, .. } => self.expr(expr),
            ExprKind::Ternary { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            ExprKind::History { base, offset } => {
                self.expr(base);
                self.expr(offset);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    self.expr(&a.value);
                }
            }
            _ => {}
        }
    }
}

fn span_of(item: &Item) -> crate::Span {
    match item {
        Item::Assign { span, .. }
        | Item::Block { span, .. }
        | Item::FuncDef { span, .. }
        | Item::Expr { span, .. } => *span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lex, parse};

    fn limit_check(src: &str) -> Vec<crate::ScriptError> {
        let (_, tokens) = lex::lex(src).expect("lex");
        let script = parse::parse(tokens).expect("parse");
        check(&script)
    }

    #[test]
    fn a_normal_script_is_within_budgets() {
        let src = "//@pine_lite version=1\n\
                   r = ta.rsi(close, 14)\n\
                   plot(r)\n\
                   hline(70)\n\
                   hline(30)\n";
        assert!(limit_check(src).is_empty());
    }

    #[test]
    fn while_is_refused() {
        let src = "//@pine_lite version=1\n\
                   while true\n\
                       x = 1\n";
        let errs = limit_check(src);
        assert!(errs.iter().any(|e| e.message.contains("`while` is refused")), "{errs:?}");
    }

    #[test]
    fn too_many_plots_is_refused() {
        let mut src = String::from("//@pine_lite version=1\n");
        for i in 0..70 {
            src.push_str(&format!("plot(close, title=\"p{i}\")\n"));
        }
        let errs = limit_check(&src);
        assert!(errs.iter().any(|e| e.message.contains("plots")), "{errs:?}");
    }
}
