//! Lexer: source to tokens with spans.
//!
//! Collects every error instead of stopping at the first -- the repair loop
//! feeds the whole list back to the model. Comments are skipped here except
//! the `//@pine_lite` header, which [`lex`] parses into the [`crate::Header`];
//! comments are otherwise insignificant to the grammar.

use crate::{ErrorKind, Header, ScriptError, Span};

/// One lexical token, with the position it started at.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// Where it starts, 1-indexed.
    pub span: Span,
}

/// The vocabulary. Deliberately small: `docs/23` refuses anything not listed.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// An identifier or dotted path (`ta.rsi`, `strategy.entry`, `bar_index`).
    Ident(String),
    /// A floating-point or integer literal, stored as f64.
    Number(f64),
    /// A double-quoted string literal.
    Str(String),
    /// A hex color literal: `#RRGGBB` or `#RRGGBBAA`, as packed RGBA.
    Color(u32),
    /// A newline, which ends statements (Pine's rule; no semicolons).
    Newline,
    /// `+ - * / % **`
    Op(String),
    /// `== != <= >= < >`
    Cmp(String),
    /// `= += -= *= /= %=`
    Assign(String),
    /// `?` and `:`.
    Question,
    Colon,
    /// `( ) [ ] , .`
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    /// A keyword, by name: `var varip if else for to by while true false na`.
    Keyword(String),
    /// End of source.
    Eof,
}

/// Parse the source into tokens plus the header.
///
/// The header must be the first non-comment line. A script without one is
/// refused: the annotation is what tells the host overlay/pane, the title and
/// the history budget, and a script that arrived without it did not come from
/// this platform's tooling.
pub fn lex(source: &str) -> Result<(Header, Vec<Token>), Vec<ScriptError>> {
    let mut errors = Vec::new();
    let mut header = Header::default();
    let mut header_seen = false;
    let mut tokens = Vec::new();

    for (idx, raw_line) in source.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("//@pine_lite") {
                if header_seen {
                    errors.push(err(line, 1, "a second @pine_lite annotation; one per script"));
                }
                header_seen = true;
                match parse_header(line, rest) {
                    Ok(h) => header = h,
                    Err(mut errs) => errors.append(&mut errs),
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let col = raw_line.len() - trimmed.len() + 1;
        match lex_line(line, col, trimmed, &mut tokens) {
            Ok(()) => {}
            Err(mut errs) => errors.append(&mut errs),
        }
        tokens.push(Token { kind: TokenKind::Newline, span: Span::new(line, col + trimmed.len()) });
    }

    if !header_seen {
        errors.push(err(
            1,
            1,
            "a script must start with the `//@pine_lite version=1` annotation",
        ));
    }
    if errors.is_empty() {
        tokens.push(Token { kind: TokenKind::Eof, span: Span::new(source.lines().count() + 1, 1) });
        Ok((header, tokens))
    } else {
        Err(errors)
    }
}

/// Parse one annotation's knobs. Knobs are separated by spaces or commas,
/// `title` may be quoted and contain spaces. Unknown knobs are an error, not
/// metadata: `docs/23` -- the model must not be able to invent host knobs.
fn parse_header(line: usize, rest: &str) -> Result<Header, Vec<ScriptError>> {
    let mut header = Header::default();
    let mut errors = Vec::new();
    let chars: Vec<char> = rest.chars().collect();
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() && chars[i] != ',' {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        let mut value = String::new();
        // `sec = "ETHUSDT"` (spaces around `=`) is as common from a model as
        // the tight form: skip whitespace on BOTH sides of the `=` and require
        // it. A key with no `=` is one malformed knob, named as such -- not a
        // phantom key plus a stray value (the old split, which reported
        // "sec needs a symbol" for what was really a syntax slip).
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j >= chars.len() || chars[j] != '=' {
            errors.push(err(
                line,
                1,
                &format!(
                    "annotation knobs are `key=value` pairs, e.g. sec=\"ETHUSDT\"; `{key}` has no `=`"
                ),
            ));
            break;
        }
        i = j + 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                value.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote
        } else {
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != ',' {
                value.push(chars[i]);
                i += 1;
            }
        }
        parts.push((key, value));
    }
    // The strategy block's sub-knobs arrive as SEPARATE parts (the outer
    // scanner ends every value at a comma): `strategy(initial_capital=10000,
    // default_qty_value=5)` becomes (`strategy(initial_capital`, `10000`)
    // plus (`default_qty_value`, `5`). Re-merge them into one value before
    // the knob match; a top-level knob's key ends the block.
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut pi = 0usize;
    while pi < parts.len() {
        let (key, value) = &parts[pi];
        if key == "strategy" {
            merged.push((key.clone(), value.clone()));
            pi += 1;
        } else if let Some(rest) = key.strip_prefix("strategy(") {
            let mut subs: Vec<String> = Vec::new();
            if !rest.is_empty() {
                subs.push(format!("{}={}", rest.trim_end_matches('='), value.trim_end_matches(')')));
            }
            pi += 1;
            while pi < parts.len() {
                let (k2, v2) = &parts[pi];
                if matches!(k2.as_str(), "version" | "overlay" | "title" | "sec" | "max_bars_back") {
                    break;
                }
                subs.push(format!("{}={}", k2, v2.trim_end_matches(')')));
                pi += 1;
            }
            merged.push(("strategy".to_string(), subs.join(",")));
        } else {
            merged.push((key.clone(), value.clone()));
            pi += 1;
        }
    }
    for (key, value) in merged {
        match key.as_str() {
            "version" => match value.parse::<u32>() {
                Ok(v) if v == 1 => header.version = v,
                _ => errors.push(err(
                    line,
                    1,
                    &format!("unsupported pine_lite version `{value}`; only 1 exists"),
                )),
            },
            "overlay" => match value.as_str() {
                "true" => header.overlay = true,
                "false" => header.overlay = false,
                other => errors.push(err(
                    line,
                    1,
                    &format!("overlay is true or false, not `{other}`"),
                )),
            },
            "title" => header.title = Some(value.clone()),
            // `request.security`-style second instrument: the host reads this
            // knob, fetches that symbol over the SAME window and timeframe,
            // and hands the aligned candles to the VM. The header (not a
            // call argument) is the declaration because the fetch is a host
            // decision -- the script can only read what arrived.
            "sec" => {
                if value.trim().is_empty() {
                    errors.push(err(line, 1, "sec needs a symbol, e.g. sec=\"ETHUSDT\""));
                } else {
                    header.sec = Some(value.trim().to_uppercase());
                }
            }
            "max_bars_back" => match value.parse::<usize>() {
                // The hard cap lives here rather than in the analyser: a
                // header asking for more is refused before anything parses.
                Ok(n) if (1..=5000).contains(&n) => header.max_bars_back = n,
                _ => errors.push(err(
                    line,
                    1,
                    "max_bars_back must be between 1 and 5000",
                )),
            },
            // The strategy account block (docs/24 S1):
            // strategy(initial_capital=10000, default_qty_type="percent_of_equity",
            //          default_qty_value=10, commission_type="percent",
            //          commission_value=0.04, slippage=0)
            // The outer scanner treats it as one `key=value` (the value runs
            // to the next space-separated key=), so sub-knobs are parsed here.
            "strategy" => {
                use crate::sim::{CommissionType, QtyType, StrategyHeader};
                let mut knobs = StrategyHeader::default();
                let mut v = value.as_str();
                // Tolerate the wrapper form `strategy(...)`.
                if let Some(inner) = v.strip_prefix("strategy(") {
                    v = inner.strip_suffix(')').unwrap_or(inner);
                } else if let Some(inner) = v.strip_prefix('(') {
                    v = inner.strip_suffix(')').unwrap_or(inner);
                }
                let mut unknown: Option<String> = None;
                for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let part = part.trim_end_matches(')');
                    let Some((k, val)) = part.split_once('=') else {
                        errors.push(err(line, 1, &format!("strategy(...) knobs are `key=value` pairs; got `{part}`")));
                        continue;
                    };
                    let val = val.trim().trim_matches('"');
                    match k.trim() {
                        "initial_capital" => match val.parse::<f64>() {
                            Ok(n) if n > 0.0 => knobs.initial_capital = n,
                            _ => errors.push(err(line, 1, "strategy.initial_capital must be a positive number")),
                        },
                        "default_qty_type" => match val {
                            "percent_of_equity" => knobs.default_qty_type = QtyType::PercentOfEquity,
                            "fixed" => knobs.default_qty_type = QtyType::Fixed,
                            other => errors.push(err(
                                line,
                                1,
                                &format!("strategy.default_qty_type is \"percent_of_equity\" or \"fixed\", not `\"{other}\"`"),
                            )),
                        },
                        "default_qty_value" => match val.parse::<f64>() {
                            Ok(n) if n > 0.0 => knobs = StrategyHeader { default_qty_value: n, ..knobs },
                            _ => errors.push(err(line, 1, "strategy.default_qty_value must be a positive number")),
                        },
                        "commission_type" => match val {
                            "percent" => knobs.commission_type = CommissionType::Percent,
                            "absolute" => knobs.commission_type = CommissionType::Absolute,
                            other => errors.push(err(
                                line,
                                1,
                                &format!("strategy.commission_type is \"percent\" or \"absolute\", not `\"{other}\"`"),
                            )),
                        },
                        "commission_value" => match val.parse::<f64>() {
                            Ok(n) if n >= 0.0 => knobs = StrategyHeader { commission_value: n, ..knobs },
                            _ => errors.push(err(line, 1, "strategy.commission_value must be a non-negative number")),
                        },
                        "slippage" => match val.parse::<f64>() {
                            Ok(n) if n >= 0.0 => knobs = StrategyHeader { slippage_pct: n, ..knobs },
                            _ => errors.push(err(line, 1, "strategy.slippage must be a non-negative number")),
                        },
                        "pyramiding" => {
                            match val.parse::<usize>() {
                                Ok(0) => {}
                                _ => errors.push(err(
                                    line,
                                    1,
                                    "pyramiding > 0 is not supported yet (single position; docs/24); declare strategy(pyramiding=0)"
                                )),
                            }
                        }
                        other => {
                            unknown = Some(other.to_string());
                        }
                    }
                }
                if let Some(other) = unknown {
                    errors.push(err(
                        line,
                        1,
                        &format!(
                            "unknown strategy(...) knob `{other}`; known knobs: initial_capital, default_qty_type, default_qty_value, commission_type, commission_value, slippage, pyramiding"
                        ),
                    ));
                }
                header.strategy = Some(knobs);
            }
            other => errors.push(err(
                line,
                1,
                &format!(
                    "unknown annotation knob `{other}`; known knobs: version, overlay, title, max_bars_back, sec"
                ),
            )),
        }
    }
    if errors.is_empty() {
        Ok(header)
    } else {
        Err(errors)
    }
}

/// Keyword set. Everything else that looks like a word is an identifier --
/// including dotted paths, which the lexer keeps whole (`ta.rsi`) so the
/// parser sees one name rather than three tokens.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "var" | "varip" | "if" | "else" | "for" | "to" | "by" | "while" | "true" | "false" | "na"
        // The logic connectives are reserved words: the parser's `and`/`or`
        // precedence levels match on `Keyword`, so lexing them as identifiers
        // silently terminated every compound condition at the first `and`.
        | "and" | "or" | "not"
    )
}

fn lex_line(
    line: usize,
    start_col: usize,
    text: &str,
    out: &mut Vec<Token>,
) -> Result<(), Vec<ScriptError>> {
    let mut errors = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let col = start_col;
    // Offsets in the *line*, so spans land where the editor's caret does.
    fn push(kind: TokenKind, col: usize, out: &mut Vec<Token>, line: usize) {
        out.push(Token { kind, span: Span::new(line, col) });
    }

    while i < chars.len() {
        let c = chars[i];
        let here = col + i;
        match c {
            ' ' | '\t' => {
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                break; // trailing comment: the rest of the line is skipped
            }
            '0'..='9' => {
                let start = i;
                let mut dot = false;
                while i < chars.len()
                    && (chars[i].is_ascii_digit()
                        || (chars[i] == '.' && !dot && chars.get(i + 1) != Some(&'.'))
                        || (chars[i] == '_'))
                {
                    if chars[i] == '.' {
                        dot = true;
                    }
                    i += 1;
                }
                let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
                match text.parse::<f64>() {
                    Ok(n) if n.is_finite() => push(TokenKind::Number(n), here, out, line),
                    _ => errors.push(err(line, here, &format!("bad number `{text}`"))),
                }
            }
            '"' => {
                let start = i;
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    errors.push(err(line, here, "unterminated string"));
                } else {
                    i += 1; // closing quote
                }
                let _ = start;
                push(TokenKind::Str(s), here, out, line);
            }
            '#' => {
                let start = i + 1;
                let mut hex = String::new();
                i += 1;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    hex.push(chars[i]);
                    i += 1;
                }
                match hex.len() {
                    6 | 8 => {
                        let rgba = u32::from_str_radix(&hex, 16).unwrap_or(0)
                            | if hex.len() == 6 { 0xFF00_0000 } else { 0 };
                        push(TokenKind::Color(rgba), here, out, line);
                    }
                    _ => errors.push(err(
                        line,
                        here,
                        &format!("`#{hex}` is not a color; use #RRGGBB or #RRGGBBAA"),
                    )),
                }
                let _ = start;
            }
            '=' | '!' | '<' | '>' => {
                // Comparison vs assignment: two-char forms win. `=>` is the
                // function-body arrow and must be checked before the lone `>`
                // comparison, which would otherwise swallow the `=` too.
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                if two == "=>" {
                    push(TokenKind::Op("=>".into()), here, out, line);
                    i += 2;
                } else if two == "==" || two == "!=" || two == "<=" || two == ">=" {
                    push(TokenKind::Cmp(two), here, out, line);
                    i += 2;
                } else if c == '=' {
                    // `=` alone, or a compound `+=`-style that started with `=`.
                    push(TokenKind::Assign("=".into()), here, out, line);
                    i += 1;
                } else {
                    push(TokenKind::Cmp(c.to_string()), here, out, line);
                    i += 1;
                }
            }
            '+' | '-' | '*' | '/' | '%' => {
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                if matches!(two.as_str(), "+=" | "-=" | "*=" | "/=" | "%=") {
                    push(TokenKind::Assign(two), here, out, line);
                    i += 2;
                } else if c == '*' && chars.get(i + 1) == Some(&'*') {
                    push(TokenKind::Op("**".into()), here, out, line);
                    i += 2;
                } else {
                    push(TokenKind::Op(c.to_string()), here, out, line);
                    i += 1;
                }
            }
            '?' => {
                push(TokenKind::Question, here, out, line);
                i += 1;
            }
            ':' => {
                // `:=` is Pine's reassignment; a bare `:` is the ternary's.
                if chars.get(i + 1) == Some(&'=') {
                    push(TokenKind::Assign(":=".into()), here, out, line);
                    i += 2;
                } else {
                    push(TokenKind::Colon, here, out, line);
                    i += 1;
                }
            }
            '(' => {
                push(TokenKind::LParen, here, out, line);
                i += 1;
            }
            ')' => {
                push(TokenKind::RParen, here, out, line);
                i += 1;
            }
            '[' => {
                push(TokenKind::LBracket, here, out, line);
                i += 1;
            }
            ']' => {
                push(TokenKind::RBracket, here, out, line);
                i += 1;
            }
            ',' => {
                push(TokenKind::Comma, here, out, line);
                i += 1;
            }
            '.' => {
                push(TokenKind::Dot, here, out, line);
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // Dotted path: keep `ta.rsi` whole when the next char is `.`
                // and the segment after it is a word.
                let mut full = word;
                let mut j = i;
                while j < chars.len() && chars[j] == '.' {
                    let seg_start = j + 1;
                    let mut k = seg_start;
                    while k < chars.len() && (chars[k].is_alphanumeric() || chars[k] == '_') {
                        k += 1;
                    }
                    if k == seg_start {
                        break;
                    }
                    full.push('.');
                    full.push_str(&chars[seg_start..k].iter().collect::<String>());
                    j = k;
                }
                if j > i {
                    i = j;
                }
                if is_keyword(&full) {
                    push(TokenKind::Keyword(full), here, out, line);
                } else {
                    push(TokenKind::Ident(full), here, out, line);
                }
            }
            other => errors.push(err(line, here, &format!("unexpected character `{other}`"))),
        }
        if !matches!(
            out.last().map(|t| &t.kind),
            Some(TokenKind::Number(_))
                | Some(TokenKind::Str(_))
                | Some(TokenKind::Color(_))
                | Some(TokenKind::Op(_))
                | Some(TokenKind::Cmp(_))
                | Some(TokenKind::Assign(_))
                | Some(TokenKind::Question)
                | Some(TokenKind::Colon)
                | Some(TokenKind::LParen)
                | Some(TokenKind::RParen)
                | Some(TokenKind::LBracket)
                | Some(TokenKind::RBracket)
                | Some(TokenKind::Comma)
                | Some(TokenKind::Dot)
                | Some(TokenKind::Keyword(_))
                | Some(TokenKind::Ident(_))
                | Some(TokenKind::Newline)
        ) {
            // A malformed token above may not have advanced `i`; guarantee
            // progress so a bad line cannot loop the lexer forever.
            if i < chars.len() && errors.last().is_some() {
                i += 1;
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn err(line: usize, col: usize, message: &str) -> ScriptError {
    ScriptError {
        kind: ErrorKind::Lex,
        span: Span::new(line, col),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_knobs_parse() {
        let (h, _) = lex(r#"//@pine_lite version=1 overlay=true title="My RSI" max_bars_back=500"#)
            .expect("header");
        assert!(h.overlay);
        assert_eq!(h.title.as_deref(), Some("My RSI"));
        assert_eq!(h.max_bars_back, 500);
    }

    #[test]
    fn comments_are_skipped_but_the_header_is_found() {
        let (_, tokens) = lex("// a comment\n//@pine_lite version=1\n// another\nx = 1").expect("ok");
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Ident(w) if w == "x")));
    }

    #[test]
    fn dotted_names_stay_whole() {
        let (_, tokens) = lex("//@pine_lite version=1\nr = ta.rsi(close, 14)").expect("ok");
        assert!(tokens
            .iter()
            .any(|t| matches!(&t.kind, TokenKind::Ident(w) if w == "ta.rsi")));
    }

    #[test]
    fn colors_and_strings_lex() {
        let (_, tokens) = lex("//@pine_lite version=1\nc = #ff0000\ns = \"hi\"").expect("ok");
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Color(c) if *c == 0xFF_FF_00_00)));
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Str(s) if s == "hi")));
    }

    #[test]
    fn a_missing_header_is_the_only_error_for_a_bare_line() {
        let errs = lex("x = 1").expect_err("no header");
        assert!(errs.iter().any(|e| e.message.contains("@pine_lite")));
    }

    #[test]
    fn compound_assignments_do_not_confuse_the_comparison_reader() {
        let (_, tokens) = lex("//@pine_lite version=1\na += 1").expect("ok");
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Assign(op) if op == "+=")));
    }
}
