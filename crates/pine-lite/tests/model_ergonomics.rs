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

fn run_ok(parsed: &pine_lite::parse::Script) -> pine_lite::Output {
    pine_lite::run(parsed, &candles(30), &Inputs::default()).expect("run")
}
