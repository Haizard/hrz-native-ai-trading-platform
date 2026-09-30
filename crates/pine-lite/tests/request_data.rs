//! Phase 14 (docs/23): `request.data("NAME")` — host-supplied platform
//! feeds (ticker fields today, venue funding/OI as those land) as per-bar
//! values aligned onto the chart's bars. An unknown name is a runtime
//! report naming the key, never a silent zero.

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
fn request_data_reads_the_host_series() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Feed\"\n",
        "chg = request.data(\"BTCUSDT.change_pct\")\n",
        "plot(chg)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let mut data_series = std::collections::HashMap::new();
    data_series.insert("BTCUSDT.change_pct".to_string(), vec![1.5; 30]);
    let inputs = Inputs { data_series, ..Inputs::default() };
    let output = run(&parsed, &candles(30), &inputs).expect("run");
    assert_eq!(output.plots.len(), 1);
    assert!((output.plots[0].values[10] - 1.5).abs() < 1e-9);
}

#[test]
fn an_unknown_data_name_is_a_runtime_report() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Feed\"\n",
        "chg = request.data(\"BTCUSDT.funding\")\n",
        "plot(chg)\n",
    );
    let (_, parsed) = vet(src).expect("vet (the name is a literal; the feed is host-side)");
    let inputs = Inputs::default();
    let err = run(&parsed, &candles(10), &inputs).expect_err("no feed");
    assert!(err.message.contains("BTCUSDT.funding"), "{}", err.message);
    assert!(err.message.contains("host supplies"), "{}", err.message);
}

#[test]
fn a_non_literal_name_is_refused_at_vet_time() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Feed\"\n",
        "key = \"BTCUSDT.change_pct\"\n",
        "chg = request.data(key)\n",
        "plot(chg)\n",
    );
    let errs = vet(src).expect_err("computed name");
    assert!(
        errs.iter().any(|e| e.message.contains("quoted series name")),
        "{errs:?}"
    );
}

#[test]
fn data_composes_with_math() {
    // A funding-adjusted spread shape: chart close minus a feed-scaled term.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Feed\"\n",
        "chg = request.data(\"BTCUSDT.change_pct\")\n",
        "signal = close - chg * 0.1\n",
        "plot(signal)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let mut data_series = std::collections::HashMap::new();
    data_series.insert("BTCUSDT.change_pct".to_string(), vec![2.0; 20]);
    let inputs = Inputs { data_series, ..Inputs::default() };
    let output = run(&parsed, &candles(20), &inputs).expect("run");
    // Bar 10: close 105 - 2*0.1 = 104.8
    assert!((output.plots[0].values[10] - 104.8).abs() < 1e-9);
}

#[test]
fn the_collector_lists_the_names_for_the_host() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"Feed\"\n",
        "a = request.data(\"BTCUSDT.change_pct\")\n",
        "b = request.data(\"ETHUSDT.change_pct\")\n",
        "plot(a - b)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let names = pine_lite::typecheck::collect_data_names(&parsed);
    assert_eq!(names, vec!["BTCUSDT.change_pct", "ETHUSDT.change_pct"]);
}
