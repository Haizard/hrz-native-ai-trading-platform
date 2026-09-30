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
pub mod sim;
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
    /// A second instrument (`sec="ETHUSDT"`) the host fetched and aligned
    /// onto the script's own bars. Its series read through `request.close` /
    /// `request.open` / `request.high` / `request.low` / `request.volume`;
    /// `request.symbol` is its ticker. None when the header declares no
    /// second instrument -- and then those calls are a vet error, not a
    /// runtime surprise.
    pub sec: Option<String>,
    /// The `strategy(...)` knob block (docs/24 S1): capital, sizing,
    /// commission, slippage. None when the header declares none -- then the
    /// simulator uses Pine-compatible defaults, and a script that READS a
    /// strategy state builtin without the block is refused at vet time.
    pub strategy: Option<sim::StrategyHeader>,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            version: 1,
            overlay: false,
            title: None,
            max_bars_back: 300,
            sec: None,
            strategy: None,
        }
    }
}

impl Header {
    /// The `const`-constructible empty header: the anchor for the body-less
    /// script a `request.security` sub-VM runs (`docs/23` Phase 11).
    pub const fn empty() -> Self {
        Self {
            version: 1,
            overlay: false,
            title: None,
            max_bars_back: 300,
            sec: None,
            strategy: None,
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
    // The lexed header attaches to the AST here: `run` (and every consumer
    // of the parsed script) must see the real `sec=` and `strategy(...)`
    // knobs, not the parser's default placeholder.
    let mut script = script;
    script.header = header.clone();
    // The lexed header rides the check: `request.*` legality depends on the
    // script having declared its second instrument (`sec=`), which only the
    // header knows.
    errors.extend(typecheck::check_with_header(&script, &header));
    errors.extend(limits::check(&script));
    // Refuse the unknowable (docs/24): a script that READS strategy account
    // state must declare the `strategy(...)` block the simulation reads its
    // knobs from. Indicators reading `strategy.equity` used to get a silent
    // 0.0 -- a number that looks real and means nothing.
    if header.strategy.is_none() && reads_strategy_state(&script) {
        errors.push(crate::ScriptError {
            kind: crate::ErrorKind::Type,
            span: crate::Span::new(1, 1),
            message: "reading strategy state (strategy.equity, strategy.position_size, ...) needs a strategy(...) header block declaring the account; add strategy(initial_capital=...) to the first line".to_string(),
        });
    }
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

/// Does the script READ one of the strategy account-state builtins? A
/// recursive walk over calls (the same shape the limits pass uses), so the
/// vet refusal above fires wherever the read hides -- inside an if body, a
/// function, a ternary.
fn reads_strategy_state(script: &parse::Script) -> bool {
    const STATE_BUILTINS: [&str; 6] = [
        "strategy.position_size",
        "strategy.position_avg_price",
        "strategy.equity",
        "strategy.openprofit",
        "strategy.closedtrades",
        "strategy.wintrades",
    ];
    fn has_state_read(e: &parse::Expr, builtins: &[&str; 6]) -> bool {
        use crate::parse::ExprKind;
        match &e.kind {
            // `strategy.equity` lexes as a dotted-path IDENT, not a call.
            ExprKind::Ident(path) => builtins.contains(&path.as_str()),
            ExprKind::Call { callee, args } => {
                builtins.contains(&callee.as_str())
                    || args.iter().any(|a| has_state_read(&a.value, builtins))
            }
            ExprKind::Bin { left, right, .. } => {
                has_state_read(left, builtins) || has_state_read(right, builtins)
            }
            ExprKind::Un { expr, .. } => has_state_read(expr, builtins),
            ExprKind::NaChecked { value } => has_state_read(value, builtins),
            ExprKind::Ternary { cond, then, els } => {
                has_state_read(cond, builtins)
                    || has_state_read(then, builtins)
                    || has_state_read(els, builtins)
            }
            ExprKind::History { base, offset } => {
                has_state_read(base, builtins) || has_state_read(offset, builtins)
            }
            _ => false,
        }
    }
    fn item_has_read(it: &parse::Item, builtins: &[&str; 6]) -> bool {
        match it {
            parse::Item::Assign { expr, .. } => has_state_read(expr, builtins),
            parse::Item::Destructure { call, .. } => has_state_read(call, builtins),
            parse::Item::Expr { expr, .. } => has_state_read(expr, builtins),
            parse::Item::Block { exprs, body, els, .. } => {
                exprs.iter().any(|e| has_state_read(e, builtins))
                    || body.iter().any(|i| item_has_read(i, builtins))
                    || els.as_ref().is_some_and(|b| b.iter().any(|i| item_has_read(i, builtins)))
            }
            parse::Item::FuncDef { body, .. } => body.iter().any(|i| item_has_read(i, builtins)),
        }
    }
    script.items.iter().any(|i| item_has_read(i, &STATE_BUILTINS))
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

    fn candles(n: usize) -> Vec<analytics_core::types::Candle> {
        (0..n)
            .map(|i| {
                let close = 100.0 + (i as f64) * 0.7 + ((i % 7) as f64) * 0.4;
                analytics_core::types::Candle {
                    symbol: "T".into(),
                    timeframe: analytics_core::types::Timeframe::M1,
                    open_time: i as i64 * 60_000_000_000,
                    open: close - 0.2,
                    high: close + 0.9,
                    low: close - 0.9,
                    close,
                    volume: 5.0,
                    buy_volume: 2.5,
                    sell_volume: 2.5,
                }
            })
            .collect()
    }

    #[test]
    fn multi_value_calls_destructure_vet_and_run() {
        let src = "//@pine_lite version=1 overlay=false title=\"MACD\"\n\
                   macd_line, signal_line, hist = ta.macd(close, 12, 26, 9)\n\
                   plot(macd_line, title=\"MACD\", color=color.blue)\n\
                   plot(signal_line, title=\"Signal\", color=color.orange)\n\
                   plot(hist, title=\"Histogram\", style=\"histogram\", color=color.gray)\n";
        let (_, parsed) = vet(&src).expect("vet");
        let candles = candles(80);
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        assert_eq!(output.plots.len(), 3);
        // Past the slow EMA's warmup the MACD line is finite.
        let macd = &output.plots[0].values;
        assert!(macd[70].is_finite(), "macd warms up: {}", macd[70]);
    }

    #[test]
    fn bollinger_bands_and_stoch_destructure() {
        let src = "//@pine_lite version=1\n\
                   basis, upper, lower = ta.bb(close, 20, 2)\n\
                   k, d = ta.stoch(close, 14)\n\
                   plot(basis)\n\
                   plot(upper)\n\
                   plot(lower)\n\
                   plot(k)\n\
                   plot(d)\n";
        let (_, parsed) = vet(src).expect("vet");
        let candles = candles(80);
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        let basis = &output.plots[0].values;
        let upper = &output.plots[1].values;
        let lower = &output.plots[2].values;
        assert!(basis[70].is_finite());
        assert!(upper[70] > basis[70] && lower[70] < basis[70], "bands straddle the basis");
    }

    #[test]
    fn a_multi_value_call_read_as_one_value_names_the_fix() {
        let errs = vet("//@pine_lite version=1\nm = ta.macd(close, 12, 26, 9)\nplot(m)\n")
            .expect_err("macd needs destructuring");
        assert!(
            errs.iter().any(|e| e.message.contains("a, b, c = ta.macd")),
            "{errs:?}"
        );
    }

    #[test]
    fn a_request_call_without_sec_is_refused_at_vet_time() {
        // The second instrument is declared in the header; a read without the
        // declaration is a compile error naming the fix.
        let errs = vet("//@pine_lite version=1\nother = request.close()\nplot(other)\n")
            .expect_err("request without sec");
        assert!(
            errs.iter().any(|e| e.message.contains("sec=")),
            "{errs:?}"
        );
    }

    #[test]
    fn smt_divergence_runs_end_to_end() {
        // The real shape: our chart makes a swing high while the correlated
        // pair makes the opposite -- SMT divergence, as cross-market math.
        let src = "//@pine_lite version=1 overlay=false title=\"SMT\" sec=\"ETHUSDT\"\n\
            w = 3\n\
            hh = ta.highest(high, 2 * w + 1)\n\
            pivot_high = hh[w] == high[w]\n\
            var theirs = na\n\
            var mine = na\n\
            if pivot_high\n\
                mine := high[w]\n\
                theirs := request.high()\n\
            smt_bear = not na(theirs) and request.high() < theirs and mine > nz(mine[1], mine)\n            plot(smt_bear ? 1.0 : 0.0, title=\"smt\")\n            plot(request.close(), title=\"ETH close\")\n";
        let (_, parsed) = vet(src).expect("vet");
        // The host's aligned second instrument: same bar count, falling highs
        // while ours rise, so the divergence genuinely fires.
        let mut candles = candles(80);
        let mut security = Vec::new();
        for (i, c) in candles.iter_mut().enumerate() {
            c.close = 100.0 + (i as f64) * 0.3; // ours rises
            c.high = c.close + 1.0;
            security.push(analytics_core::types::Candle {
                symbol: "ETHUSDT".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: c.open_time,
                open: 50.0,
                high: 51.0 - (i as f64) * 0.2, // theirs falls: divergence
                low: 49.0,
                close: 50.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        let inputs = Inputs { security, ..Inputs::default() };
        let output = run(&parsed, &candles, &inputs).expect("run");
        assert_eq!(output.plots.len(), 2);
        // The second plot is the pair's close, bar-aligned.
        assert!((output.plots[1].values[79] - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_sec_header_without_fetched_data_reads_as_data_error() {
        // The script declared sec= but the HOST failed to fetch: the error
        // must say the data is missing, not that the feature is.
        let src = "//@pine_lite version=1 sec=\"ETHUSDT\"\nplot(request.close())\n";
        let (_, parsed) = vet(src).expect("vet");
        let err = run(&parsed, &candles(10), &Inputs::default()).expect_err("no security data");
        assert!(err.message.contains("sec="), "{err}");
    }

    #[test]
    fn a_wrong_name_count_is_refused() {
        let errs = vet("//@pine_lite version=1\na, b = ta.macd(close, 12, 26, 9)\nplot(a)\n")
            .expect_err("macd has three outputs");
        assert!(errs.iter().any(|e| e.message.contains("3 value")), "{errs:?}");
    }

    #[test]
    fn weighted_moving_average_weights_the_newest_bar_most() {
        // A rising series: the WMA, weighting the newest bar most, sits at or
        // above the SMA of the same length.
        let src: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let wma = ta::ta_wma(&src, 5.0);
        let sma = ta::ta_sma(&src, 5.0);
        assert!(wma[29] > sma[29], "wma {} sma {}", wma[29], sma[29]);
        // The last window is 125..129 with weights 1..5: 1915/15.
        assert!((wma[29] - 1915.0 / 15.0).abs() < 1e-9, "weighted mean: {}", wma[29]);
    }

    #[test]
    fn arrays_collect_pivots_the_smc_way() {
        // The shape every SMC/market-structure script takes: detect pivots,
        // push their prices and bar stamps into arrays, read the last two
        // back to build a line. No indexed assignment, real collections.
        let src = "//@pine_lite version=1 overlay=false title=\"Pivot log\"\n\
            w = 3\n\
            var highs = array.new()\n\
            var bars = array.new()\n\
            hh = ta.highest(high, 2 * w + 1)\n\
            if hh[w] == high[w]\n\
                array.push(highs, high[w])\n\
                array.push(bars, bar_index - w)\n\
            n = array.size(highs)\n\
            plot(n, title=\"pivot count\", style=\"columns\")\n\
            last_pivot = array.get(highs, n - 1)\n\
            plot(last_pivot, title=\"last pivot\")\n";
        let (_, parsed) = vet(src).expect("vet");
        let candles = candles(60);
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        // The rising series makes a new pivot roughly every w+1 bars.
        let count = output.plots[0].values[59];
        assert!(count >= 4.0, "several pivots collected: {count}");
        let last = output.plots[1].values[59];
        assert!(last.is_finite() && last > 100.0, "the last pivot's price: {last}");
    }

    #[test]
    fn array_bounds_and_mutators_behave() {
        let src = "//@pine_lite version=1\n\
            var a = array.new()\n\
            array.push(a, 1.0)\n\
            array.push(a, 2.0)\n\
            array.push(a, 3.0)\n\
            array.pop(a)\n\
            first = array.first(a)\n\
            avg = array.avg(a)\n\n\
            // out-of-bounds reads are na; nz() folds the read back for the sum\n\
            missing = nz(array.get(a, 99))\n\
            plot(first + avg + missing)\n";
        let (_, parsed) = vet(src).expect("vet");
        let candles = candles(20);
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        // a = [1, 2] after the pop; first=1, avg=1.5, missing=nz(na)=0 -> 2.5.
        assert!((output.plots[0].values[19] - 2.5).abs() < 1e-9, "{}", output.plots[0].values[19]);
    }

    #[test]
    fn session_words_tell_the_time_of_day() {
        // 8 hourly bars starting at 00:00 UTC: hour must climb 0..7 and
        // dayofweek must be a Monday (2) for 2026-09-28.
        let src = "//@pine_lite version=1\nh = hour\nplot(h)\nplot(dayofweek, title=\"dow\")\n";
        let (_, parsed) = vet(src).expect("vet");
        let mut candles = candles(8);
        for (i, c) in candles.iter_mut().enumerate() {
            c.open_time = 1_790_553_600_000_000_000i64 + (i as i64) * 3_600_000_000_000;
        }
        let output = run(&parsed, &candles, &Inputs::default()).expect("run");
        assert_eq!(output.plots[0].values[7], 7.0, "the eighth bar is 07:00 UTC");
        assert_eq!(output.plots[1].values[0], 2.0, "2026-09-28 is a Monday");
    }

    #[test]
    fn an_array_allocated_without_var_is_refused() {
        // `a = array.new()` allocates EVERY bar, blowing the heap cap -- the
        // error must say so, because this is the one way the pattern breaks.
        let src = "//@pine_lite version=1\na = array.new()\narray.push(a, 1.0)\nplot(array.size(a))\n";
        let (_, parsed) = vet(src).expect("vet");
        let candles = candles(200);
        let err = run(&parsed, &candles, &Inputs::default()).expect_err("heap cap");
        assert!(err.message.contains("arrays"), "{err}");
    }
}

