//! Model-ergonomics repairs: the syntax models write reflexively (Pine's
//! multi-line calls, typed declarations, `=` for `==` in comparisons) used to
//! be refused, and each refusal burned one of the generator's five attempts.
//! These tests pin the tolerances so the gap stays closed.

use pine_lite::{vet, Inputs};
use analytics_core::types::{Candle, Timeframe};

fn candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let close = 100.0 + (i as f64) * 0.5;
            Candle {
                symbol: "T".into(),
                timeframe: Timeframe::M1,
                open_time: i as i64 * 60_000_000_000,
                open: close,
                high: close + 1.0,
                low: close - 1.0,
                close,
                volume: 10.0,
                buy_volume: 5.0,
                sell_volume: 5.0,
            }
        })
        .collect()
}

#[test]
fn a_call_may_wrap_across_lines() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "plot(\n",
        "    close,\n",
        "    title=\"close line\",\n",
        "    linewidth=2,\n",
        ")\n",
    );
    let (_, parsed) = vet(src).expect("a wrapped call parses");
    let output = run_ok(&parsed);
    assert_eq!(output.plots.len(), 1);
}

#[test]
fn a_nested_call_may_wrap_across_lines() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "x = ta.sma(\n",
        "    close,\n",
        "    5)\n",
        "plot(x)\n",
    );
    let (_, parsed) = vet(src).expect("a nested wrapped call parses");
    let output = run_ok(&parsed);
    // Bar 24 of the ramp fixture: sma(close, 5) = mean of closes 20..24.
    let got = output.plots[0].values[24];
    assert!((got - 111.0).abs() < 1e-9, "got {got}");
}

#[test]
fn a_typed_var_declaration_parses_as_a_plain_one() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "var float x = 0.0\n",
        "x := x + 1\n",
        "var int count = na\n",
        "plot(x)\n",
    );
    let (_, parsed) = vet(src).expect("typed declarations parse");
    let output = run_ok(&parsed);
    // `var float x = 0.0` initializes once and `x := x + 1` ticks every bar,
    // so by bar 29 the counter reads 30 -- the type word changed nothing.
    let got = output.plots[0].values[29];
    assert!((got - 30.0).abs() < 1e-9, "got {got}");
}

#[test]
fn a_bare_equals_in_a_comparison_reads_as_eq() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "up = close = close\n",
        "plot(up ? 1 : 0)\n",
    );
    let (_, parsed) = vet(src).expect("`=` in a comparison chain reads as `==`");
    let output = run_ok(&parsed);
    assert_eq!(output.plots[0].values[10], 1.0);
}

#[test]
fn assignment_still_binds_not_compares() {
    // The tolerance must not swallow real assignments: `x = y = 5` would be a
    // chained assignment in another language, but here the statement parser
    // takes `x = <expr>` and the comparison tolerance only lives inside
    // expressions, so this is `x` assigned the boolean `y == 5`.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "y = 5.0\n",
        "x = y = 5.0\n",
        "plot(x ? 1 : 0)\n",
    );
    let (_, parsed) = vet(src).expect("chained-looking assign parses");
    let output = run_ok(&parsed);
    assert_eq!(output.plots[0].values[3], 1.0);
}

#[test]
fn the_bracketed_destructure_spelling_parses() {
    // `[macd_line, signal] = ta.macd(...)` is the spelling models consider
    // canonical Pine; the bare `a, b = ...` form was already legal.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "[macd_line, signal, hist] = ta.macd(close, 12, 26, 9)\n",
        "plot(macd_line)\n",
    );
    let (_, parsed) = vet(src).expect("bracketed destructure parses");
    let output = run_ok(&parsed);
    assert_eq!(output.plots.len(), 1);
}

#[test]
fn the_call_form_of_the_time_of_day_reads_works() {
    // The bare words (`hour`, `minute`) are series; models also write the
    // call form `hour(time)`, which must read the same clock.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "h = hour(time)\n",
        "m = minute(time)\n",
        "plot(h * 60 + m)\n",
    );
    let (_, parsed) = vet(src).expect("the call form vets");
    let output = run_ok(&parsed);
    // Fixture candles start at epoch midnight, 1m apart: hour 0, minute = bar.
    let got = output.plots[0].values[17];
    assert!((got - 17.0).abs() < 1e-9, "got {got}");
}

// ---- v1.1 (docs/23 Phase 15): while loops, parameter defaults, object cap ----

#[test]
fn a_while_loop_runs_until_its_condition_turns_false() {
    // The counter idiom the prompt teaches: a `var` flag the body mutates,
    // so the condition turns false instead of burning the fuel budget.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "var count = 0\n",
        "while count < 7\n",
        "    count := count + 1\n",
        "plot(count)\n",
    );
    let (_, parsed) = vet(src).expect("while vets");
    let output = run_ok(&parsed);
    // Per-bar execution: the loop finishes on bar 0 and stays there.
    assert_eq!(output.plots[0].values[0], 7.0);
    assert_eq!(output.plots[0].values[29], 7.0);
}

#[test]
fn a_runaway_while_dies_on_the_fuel_budget_not_on_a_hang() {
    // The safety contract: `while true` must ERROR, never hang the chart.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "while true\n",
        "    x = 1\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("the shape parses");
    let err = pine_lite::run(&parsed, &candles(30), &Inputs::default())
        .expect_err("a runaway while must die on fuel");
    assert!(err.message.contains("step budget"), "{}", err.message);
}

#[test]
fn while_nesting_beyond_four_is_refused() {
    // The `for` nesting cap now covers `while` too.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "while close > 0\n",
        "    while close > 0\n",
        "        while close > 0\n",
        "            while close > 0\n",
        "                while close > 0\n",
        "                    x = 1\n",
        "plot(close)\n",
    );
    let errs = vet(src).expect_err("nesting cap");
    assert!(errs.iter().any(|e| e.message.contains("nesting deeper than 4")), "{errs:?}");
}

#[test]
fn a_function_default_fills_an_omitted_trailing_argument() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "shrink(s, mult = 2.0) =>\n",
        "    s * mult\n",
        "a = shrink(high - low)\n",
        "b = shrink(high - low, 3.0)\n",
        "plot(a)\n",
        "plot(b)\n",
    );
    let (_, parsed) = vet(src).expect("defaults vet");
    let output = run_ok(&parsed);
    // Fixture: high - low == 2.0 on every bar. Default mult -> 4.0, explicit
    // 3.0 -> 6.0.
    assert_eq!(output.plots[0].values[5], 4.0);
    assert_eq!(output.plots[1].values[5], 6.0);
}

#[test]
fn a_default_may_read_the_caller_series_history() {
    // Caller-scope series semantics for defaults, mirroring explicit args:
    // a default of `close[1]` is the caller's YESTERDAY, re-evaluated per
    // call, not a value frozen at definition time.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "prev(x = close[1]) =>\n",
        "    x\n",
        "v = prev()\n",
        "plot(v)\n",
    );
    let (_, parsed) = vet(src).expect("the history default vets");
    let output = run_ok(&parsed);
    assert_eq!(output.plots[0].values[5], 102.0, "close[1] at bar 5 == close(bar 4)");
}

#[test]
fn a_required_parameter_after_a_default_is_refused() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "f(a = 1.0, b) =>\n",
        "    a + b\n",
        "plot(f(1.0, 2.0))\n",
    );
    let errs = vet(src).expect_err("required-after-optional");
    assert!(errs.iter().any(|e| e.message.contains("defaults must be trailing")), "{errs:?}");
}

#[test]
fn a_call_below_min_arity_is_refused() {
    // `h(a, b, c = 3.0)` accepts 2 or 3 args; one is a refusal that names
    // the accepted range.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\"\n",
        "h(a, b, c = 3.0) =>\n",
        "    a + b + c\n",
        "plot(h(1.0))\n",
    );
    let errs = vet(src).expect_err("below min arity");
    assert!(errs.iter().any(|e| e.message.contains("2..=3")), "{errs:?}");
}

fn run_ok(parsed: &pine_lite::parse::Script) -> pine_lite::Output {
    pine_lite::run(parsed, &candles(30), &Inputs::default()).expect("run")
}
