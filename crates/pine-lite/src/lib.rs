//! pine-lite -- a Pine Script-like imperative language, interpreted.
//!
//! `docs/23-PINE-LITE-SCRIPTING-LANGUAGE.md` is the spec. The crate is a leaf:
//! its only internal dependency is `analytics-core`, because `ta.*` must be
//! the same math the chart, the scanner and the validator already compute --
//! never a second implementation of it.
//!
//! ## The one design decision everything else follows
//!
//! Pine's defining semantic is that **the script is re-entered once per bar**
//! and every variable is implicitly a series. This crate therefore evaluates
//! the AST once per candle, and "a variable" is a ring buffer of one value per
//! bar. History indexing `x[3]` reads that buffer. `na` is a real value in
//! every float series, never NaN leaked from arithmetic.
//!
//! ## Layers, in the order the host applies them
//!
//! 1. [`lex`] -- source to tokens with spans, all errors, not the first.
//! 2. [`parse`] -- tokens to an AST, all errors, spans carried for messages.
//! 3. [`typecheck`] -- names, arity, Pine casting rules.
//! 4. [`limits`] -- the static budgets: plots, statements, functions, history.
//! 5. [`interp`] -- per-bar execution over a host-provided candle window.
//!
//! A script that has not passed 1-4 never runs; a script that runs out of
//! dynamic fuel (the sandbox's job, not this crate's) is killed and reported,
//! never drawn half-way.

pub mod interp;
pub mod lex;
pub mod limits;
pub mod parse;
pub mod ta;
pub mod typecheck;

pub use interp::{run, Inputs, Output, Plot, PlotKind, PlotStyle, Vm};
pub use lex::Token;
pub use parse::{Expr, Item};

use thiserror::Error;

/// A vetting failure: one parse/type/limit problem, with where it happened.
///
/// The AI repair loop needs the *list*, not the first error, so every layer
/// returns `Vec<ScriptError>` through [`vet`].
#[derive(Debug, Clone, PartialEq, Error)]
#[error("{} error at line {}, col {}: {}", kind_name(*kind), span.line, span.col, message)]
pub struct ScriptError {
    /// Which layer refused it.
    pub kind: ErrorKind,
    /// Where in the source, 1-indexed.
    pub span: Span,
    /// Human-readable, written to be shown to the author (human or model).
    pub message: String,
}

/// Which vetting layer produced an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The lexer refused the source.
    Lex,
    /// The parser refused the token stream.
    Parse,
    /// The type checker refused the AST.
    Type,
    /// The static analyser refused the budgets.
    Limit,
}

/// The layer's name, for the error message.
#[must_use]
pub const fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Lex => "lex",
        ErrorKind::Parse => "parse",
        ErrorKind::Type => "type",
        ErrorKind::Limit => "limit",
    }
}

/// The type-checker's static output (inputs, functions, var types). The
/// gateway's vet route reads it for the settings UI.
pub use typecheck::Checked;

/// A position in the source, 1-indexed, as the editor shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Line, 1-indexed.
    pub line: usize,
    /// Column, 1-indexed.
    pub col: usize,
}

impl Span {
    #[must_use]
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// The script's header: the `//@pine_lite` annotation's knobs.
///
/// Unknown knobs are a validation error (`docs/23`): the annotation is the
/// host contract, not free-form metadata the model can invent.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// Annotation version. Only 1 exists.
    pub version: u32,
    /// Plots go to the price pane when true; otherwise the script owns a
    /// sub-pane.
    pub overlay: bool,
    /// Display name, for the chart legend.
    pub title: Option<String>,
    /// History depth the script may index, `max_bars_back`. Default 300.
    pub max_bars_back: usize,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            version: 1,
            overlay: false,
            title: None,
            max_bars_back: 300,
        }
    }
}

/// Vet a script end-to-end without running it: lex, parse, type check, limits.
///
/// Returns every problem, not the first, because the studio's repair loop
/// feeds the whole list back to the model. An empty list means the AST is
/// ready for [`run`].
pub fn vet(source: &str) -> Result<(Header, parse::Script), Vec<ScriptError>> {
    let mut errors = Vec::new();
    let (header, tokens) = match lex::lex(source) {
        Ok((h, tokens)) => (h, tokens),
        Err(mut errs) => {
            errors.append(&mut errs);
            return Err(errors);
        }
    };
    let script = match parse::parse(tokens) {
        Ok(script) => script,
        Err(mut errs) => {
            errors.append(&mut errs);
            return Err(errors);
        }
    };
    // Lex and parse failures stop the pipeline: the later layers need an AST.
    if !errors.is_empty() {
        return Err(errors);
    }
    let header = header_or_default(script.header.clone(), &header);
    errors.extend(typecheck::check(&script));
    errors.extend(limits::check(&script));
    if errors.is_empty() {
        Ok((header, script))
    } else {
        Err(errors)
    }
}

/// The parser carries the default header; `vet` re-attaches the lexer's real
/// one. (Kept as a helper so the pipeline stays a straight line.)
fn header_or_default(parsed: Header, lexed: &Header) -> Header {
    let _ = parsed;
    lexed.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_script_vets_clean() {
        let src = r#"
            //@pine_lite version=1 overlay=false title="RSI"
            r = ta.rsi(close, 14)
            plot(r, title="RSI", color=color.blue)
        "#;
        let result = vet(src);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn every_error_of_a_layer_is_reported_not_just_the_first() {
        // Two unknown names: both must come back. (A parse error is a hard
        // stop -- the type checker needs an AST -- so cross-layer bundling is
        // deliberately not promised.)
        let src = r#"
            //@pine_lite version=1
            a = first_unknown
            b = second_unknown
        "#;
        let errs = vet(src).expect_err("two errors");
        assert!(errs.len() >= 2, "{errs:?}");
        assert!(errs.iter().any(|e| e.message.contains("first_unknown")));
        assert!(errs.iter().any(|e| e.message.contains("second_unknown")));
    }

    #[test]
    fn an_unknown_annotation_knob_is_refused() {
        let src = r#"
            //@pine_lite version=1 host_freebie=true
            plot(close)
        "#;
        let errs = vet(src).expect_err("unknown knob");
        assert!(errs.iter().any(|e| e.message.contains("host_freebie")), "{errs:?}");
    }

    #[test]
    fn a_script_without_the_annotation_is_refused() {
        let errs = vet("plot(close)").expect_err("no annotation");
        assert!(errs.iter().any(|e| e.message.contains("@pine_lite")), "{errs:?}");
    }
}
