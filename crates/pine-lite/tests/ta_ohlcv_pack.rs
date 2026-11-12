use analytics_core::types::{Candle, Timeframe};
use pine_lite::{run, vet, Inputs};

fn candle(index: usize, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Candle {
    Candle {
        symbol: "TEST".into(),
        timeframe: Timeframe::M1,
        open_time: index as i64 * Timeframe::M1.nanos(),
        open,
        high,
        low,
        close,
        volume,
        buy_volume: volume / 2.0,
        sell_volume: volume / 2.0,
    }
}

#[test]
fn chaikin_money_flow_preserves_warmup_and_ohlcv_values() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"CMF\"\n",
        "flow = ta.cmf(2)\n",
        "plot(flow)\n",
    );
    let (_, parsed) = vet(source).expect("CMF call should vet");
    let candles = vec![
        candle(0, 5.0, 10.0, 0.0, 10.0, 10.0),
        candle(1, 5.0, 10.0, 0.0, 0.0, 10.0),
        candle(2, 5.0, 10.0, 0.0, 5.0, 10.0),
    ];

    let output = run(&parsed, &candles, &Inputs::default()).expect("run CMF");
    let flow = &output.plots[0].values;
    assert!(flow[0].is_nan(), "CMF needs a full window");
    assert!((flow[1] - 0.0).abs() < 1e-9);
    assert!((flow[2] + 0.5).abs() < 1e-9);
}

#[test]
fn awesome_oscillator_uses_its_standard_midpoint_windows() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"AO\"\n",
        "osc = ta.ao()\n",
        "plot(osc)\n",
    );
    let (_, parsed) = vet(source).expect("AO call should vet");
    let candles: Vec<_> = (0..34)
        .map(|index| {
            let midpoint = index as f64;
            candle(
                index,
                midpoint,
                midpoint + 1.0,
                midpoint - 1.0,
                midpoint,
                100.0,
            )
        })
        .collect();

    let output = run(&parsed, &candles, &Inputs::default()).expect("run AO");
    let ao = &output.plots[0].values;
    assert!(ao[32].is_nan(), "the slow 34-bar SMA is still warming");
    assert!(
        (ao[33] - 14.5).abs() < 1e-9,
        "5/34 SMA difference at the tail"
    );
}

#[test]
fn chaikin_money_flow_rejects_non_integer_length_types() {
    let errors = vet("//@pine_lite version=1\nflow = ta.cmf(true)\n")
        .expect_err("boolean length must be rejected");
    assert!(
        errors.iter().any(|error| error
            .message
            .contains("`ta.cmf` parameter `length` expects Integer")),
        "{errors:?}"
    );
}

#[test]
fn named_ta_arguments_are_bound_by_parameter_name() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"Named arguments\"\n",
        "avg = ta.sma(length=2, source=close)\n",
        "macd_line, signal_line, hist = ta.macd(source=close, signal=2, slow=3, fast=2)\n",
        "stoch = ta.stoch(close)\n",
        "plot(avg)\n",
        "plot(macd_line)\n",
        "plot(stoch)\n",
    );
    let (_, parsed) = vet(source).expect("named TA arguments should vet");
    let candles: Vec<_> = (0..20)
        .map(|index| {
            let close = index as f64 + 1.0;
            candle(index, close, close + 1.0, close - 1.0, close, 1.0)
        })
        .collect();
    let output = run(&parsed, &candles, &Inputs::default()).expect("run named TA calls");
    assert_eq!(output.plots[0].values[1], 1.5);
    assert!(output.plots[1].values[2].is_finite());
    assert!(output.plots[2].values[13].is_finite());
}

#[test]
fn ta_cross_reports_both_upward_and_downward_crossings() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"Crosses\"\n",
        "up = ta.crossover(close, 2)\n",
        "down = ta.crossunder(close, 2)\n",
        "either = ta.cross(close, 2)\n",
        "plot(up ? 1 : 0)\n",
        "plot(down ? 1 : 0)\n",
        "plot(either ? 1 : 0)\n",
    );
    let (_, parsed) = vet(source).expect("cross calls should vet");
    let candles = vec![
        candle(0, 1.0, 1.0, 1.0, 1.0, 1.0),
        candle(1, 3.0, 3.0, 3.0, 3.0, 1.0),
        candle(2, 1.0, 1.0, 1.0, 1.0, 1.0),
    ];
    let output = run(&parsed, &candles, &Inputs::default()).expect("run cross calls");
    assert_eq!(output.plots[0].values[1], 1.0);
    assert_eq!(output.plots[1].values[2], 1.0);
    assert_eq!(output.plots[2].values[1], 1.0);
    assert_eq!(output.plots[2].values[2], 1.0);
}

#[test]
fn optional_ta_parameters_execute_with_the_catalog_defaults() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"TA defaults\"\n",
        "macd, signal, hist = ta.macd(close)\n",
        "basis, upper, lower = ta.bb(close)\n",
        "k, d = ta.stoch(close)\n",
        "plus, minus, adx = ta.dmi()\n",
        "kc, kc_upper, kc_lower = ta.kc()\n",
        "trend, direction = ta.supertrend()\n",
        "alma = ta.alma(close, 2)\n",
        "line = ta.linreg(close, 2)\n",
        "lsma = ta.lsma(close, 2)\n",
        "plot(signal)\n",
        "plot(upper)\n",
        "plot(d)\n",
        "plot(adx)\n",
        "plot(kc_upper)\n",
        "plot(trend)\n",
        "plot(alma)\n",
        "plot(line)\n",
        "plot(lsma)\n",
    );
    let (_, parsed) = vet(source).expect("catalog defaults should vet");
    let candles: Vec<_> = (0..40)
        .map(|index| {
            let close = 100.0 + index as f64;
            candle(index, close, close + 1.0, close - 1.0, close, 100.0)
        })
        .collect();
    let output = run(&parsed, &candles, &Inputs::default()).expect("run defaulted TA calls");
    for (plot_index, plot) in output.plots.iter().enumerate() {
        assert!(
            plot.values[39].is_finite(),
            "TA output plot {plot_index} was non-finite at bar 39: {}",
            plot.values[39]
        );
    }
}
