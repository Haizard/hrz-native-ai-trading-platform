//! Gap-1 verification (docs/23 Phase 12): user-defined functions. The
//! parser, checker and VM machinery already landed with the language core;
//! these tests pin the contract so the gap stays closed.

use pine_lite::{run, vet, Inputs};
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
fn a_function_vets_and_runs() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Fn\"\n",
        "range_size(h, l) =>\n",
        "    h - l\n",
        "\n",
        "r = range_size(high, low)\n",
        "plot(r)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    assert_eq!(output.plots.len(), 1);
    // high - low is 2.0 everywhere for these fixtures.
    assert!((output.plots[0].values[10] - 2.0).abs() < 1e-9);
}

#[test]
fn a_function_composes_with_ta() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Fn\"\n",
        "smoothed(x, n) =>\n",
        "    ta.sma(x, n)\n",
        "\n",
        "s = smoothed(close, 5)\n",
        "plot(s)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    // Bar 24: closes 20..24 are 110, 110.5, 111, 111.5, 112 -> mean 111.
    let got = output.plots[0].values[24];
    assert!((got - 111.0).abs() < 1e-9, "got {got}");
}

#[test]
fn wrong_arity_is_refused_at_vet_time() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Fn\"\n",
        "add(a, b) =>\n",
        "    a + b\n",
        "\n",
        "x = add(1.0)\n",
        "plot(x)\n",
    );
    let errs = vet(src).expect_err("arity");
    assert!(
        errs.iter().any(|e| e.message.contains("takes 2")),
        "{errs:?}"
    );
}

#[test]
fn recursion_is_refused() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Fn\"\n",
        "loop(x) =>\n",
        "    loop(x)\n",
        "\n",
        "y = loop(1.0)\n",
        "plot(y)\n",
    );
    let (_, parsed) = vet(src).expect("vet (the checker allows the shape)");
    let err = run(&parsed, &candles(10), &Inputs::default()).expect_err("recursion");
    assert!(err.message.contains("recursion is refused"), "{}", err.message);
}
