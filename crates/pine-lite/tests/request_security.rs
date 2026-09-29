//! Phase 11 (docs/23): `request.security("SYM", "tf", expr)` — any pair,
//! any timeframe the host pooled, per call. The checker collects the named
//! pairs (so the host knows what to fetch), refuses non-literal symbols,
//! caps the pool, and the VM evaluates the third argument over the pooled
//! series at the parent's bar.

use pine_lite::{run, vet, Inputs};
use analytics_core::types::{Candle, Timeframe};

fn candle(sym: &str, i: usize, tf: Timeframe, base: f64, step: f64) -> Candle {
    let close = base + (i as f64) * step;
    Candle {
        symbol: sym.into(),
        timeframe: tf,
        open_time: i as i64 * 60_000_000_000,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 10.0,
        buy_volume: 5.0,
        sell_volume: 5.0,
    }
}

#[test]
fn the_checker_collects_named_pairs() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
        "a = request.security(\"ETHUSDT\", \"5m\", request.close())\n",
        "b = request.security(\"SOLUSDT\", \"1h\", request.close())\n",
        "c = request.security(\"ETHUSDT\", \"5m\", request.high())\n",
        "plot(a + b + c)\n",
    );
    assert!(vet(src).is_ok(), "must vet");
}

#[test]
fn the_pool_surfaces_for_the_host() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
        "a = request.security(\"ETHUSDT\", \"5m\", request.close())\n",
        "b = request.security(\"SOLUSDT\", \"1h\", request.close())\n",
        "c = request.security(\"ETHUSDT\", \"5m\", request.high())\n",
        "plot(a + b + c)\n",
    );
    let (header, script) = vet(src).expect("vet");
    let pool = pine_lite::typecheck::collect_series_pool(&script, &header);
    assert_eq!(pool, vec!["ETHUSDT@5M", "SOLUSDT@1H"]);
}

#[test]
fn a_non_literal_symbol_is_refused() {
    // The host fetches BEFORE the run: a computed symbol is a fetch the
    // user never saw, so it is a vet error, not a runtime surprise.
    let src = concat!(
        "//@pine_lite version=1\n",
        "sym = \"ETHUSDT\"\n",
        "a = request.security(sym, \"5m\", request.close())\n",
        "plot(a)\n",
    );
    let errs = vet(src).expect_err("non-literal symbol");
    assert!(
        errs.iter().any(|e| e.message.contains("quoted string literal")),
        "{errs:?}"
    );
}

#[test]
fn the_pool_is_capped() {
    let mut src = String::from("//@pine_lite version=1\n");
    for (i, sym) in ["A", "B", "C", "D", "E", "F", "G", "H", "I"].iter().enumerate() {
        src.push_str(&format!(
            "p{i} = request.security(\"{sym}USDT\", \"5m\", request.close())\n"
        ));
    }
    src.push_str("plot(p0 + p1 + p2 + p3 + p4 + p5 + p6 + p7 + p8)\n");
    let errs = vet(&src).expect_err("9 pairs");
    assert!(
        errs.iter().any(|e| e.message.contains("at most 8")),
        "{errs:?}"
    );
}

#[test]
fn security_evaluates_over_the_pooled_series() {
    // The chart is flat at 100; the pair rises 0.5/bar from 50. The third
    // argument `request.close()` reads the PAIR's close at the chart's bar,
    // so the plot is the pair's series — not the chart's.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
        "pair = request.security(\"ETHUSDT\", \"1m\", request.close())\n",
        "plot(pair)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let candles: Vec<Candle> = (0..30).map(|i| candle("BTCUSDT", i, Timeframe::M1, 100.0, 0.0)).collect();
    let pool: Vec<Candle> = (0..30).map(|i| candle("ETHUSDT", i, Timeframe::M1, 50.0, 0.5)).collect();
    let mut series_pool = std::collections::HashMap::new();
    series_pool.insert("ETHUSDT@1M".to_string(), pool);
    let inputs = Inputs { series_pool, ..Inputs::default() };
    let output = run(&parsed, &candles, &inputs).expect("run");
    assert_eq!(output.plots.len(), 1);
    // Bar 20: the pair's close (50 + 20*0.5 = 60), not the chart's 100.
    assert!(
        (output.plots[0].values[20] - 60.0).abs() < 1e-9,
        "got {}",
        output.plots[0].values[20]
    );
}

#[test]
fn security_composes_with_ta_over_the_pair() {
    // ta.sma over the pair's closes: smoothed pair data, not chart data.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
        "smoothed = ta.sma(request.security(\"ETHUSDT\", \"1m\", request.close()), 5)\n",
        "plot(smoothed)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let candles: Vec<Candle> = (0..30).map(|i| candle("BTCUSDT", i, Timeframe::M1, 100.0, 0.0)).collect();
    let pool: Vec<Candle> = (0..30).map(|i| candle("ETHUSDT", i, Timeframe::M1, 50.0, 0.5)).collect();
    let mut series_pool = std::collections::HashMap::new();
    series_pool.insert("ETHUSDT@1M".to_string(), pool);
    let inputs = Inputs { series_pool, ..Inputs::default() };
    let output = run(&parsed, &candles, &inputs).expect("run");
    // Bar 24: SMA of the pair's closes 52.5..62.5 -> wait: closes at bars
    // 20..24 are 60,60.5,61,61.5,62 -> mean 61.4... precisely (60+60.5+61+61.5+62)/5 = 61.
    let got = output.plots[0].values[24];
    assert!(
        (got - 61.0).abs() < 1e-9,
        "got {got}"
    );
}

#[test]
fn a_missing_pooled_series_is_an_error_naming_the_key() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"MTF\"\n",
        "pair = request.security(\"ETHUSDT\", \"1m\", request.close())\n",
        "plot(pair)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let candles: Vec<Candle> = (0..10).map(|i| candle("BTCUSDT", i, Timeframe::M1, 100.0, 0.0)).collect();
    let inputs = Inputs::default();
    let err = run(&parsed, &candles, &inputs).expect_err("no pool");
    assert!(err.message.contains("ETHUSDT@1M"), "{}", err.message);
}
