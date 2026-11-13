use analytics_core::types::{Candle, Timeframe};
use pine_lite::interp::{self, Inputs};
use sandbox::{PinePreviewRequest, Sandbox, SandboxLimits};
use std::time::Duration;

fn candles(closes: &[f64]) -> Vec<Candle> {
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::M1,
            open_time: index as i64 * Timeframe::M1.nanos(),
            open: *close,
            high: *close + 1.0,
            low: *close - 1.0,
            close: *close,
            volume: 100.0,
            buy_volume: 50.0,
            sell_volume: 50.0,
        })
        .collect()
}

#[test]
fn wasm_preview_matches_native_indicator_evidence() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"Fixture\"\n",
        "avg = ta.sma(close, 2)\n",
        "plot(avg, title=\"Average\")\n",
        "plotshape(close > 2, shape=\"circle\", location_value=high)\n",
    );
    let candles = candles(&[1.0, 2.0, 3.0]);
    let (_, parsed) = pine_lite::vet(source).expect("fixture should vet");
    let native =
        interp::run(&parsed, &candles, &Inputs::default()).expect("native fixture should execute");

    let sandbox = Sandbox::new().expect("the shipped Wasm module should compile");
    let (preview, usage) = sandbox
        .execute_pine_preview(PinePreviewRequest {
            source: source.into(),
            candles,
            inputs: Inputs::default(),
        })
        .expect("Wasm fixture should execute");

    let preview_plot = preview
        .plots
        .iter()
        .find(|plot| plot.title == "Average")
        .expect("guest should preserve the numeric plot");
    let native_plot = native
        .plots
        .iter()
        .find(|plot| plot.title == "Average")
        .expect("native should preserve the numeric plot");
    let expected: Vec<_> = native_plot
        .values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    assert_eq!(
        (preview_plot.low, preview_plot.high),
        (
            expected.iter().copied().reduce(f64::min).unwrap(),
            expected.iter().copied().reduce(f64::max).unwrap()
        )
    );
    assert_eq!(preview.shapes.len(), native.shapes.len());
    for (guest, host) in preview.shapes.iter().zip(&native.shapes) {
        assert_eq!(guest.bar, host.bar);
        assert_eq!(guest.value, host.value);
        assert_eq!(guest.glyph, host.glyph);
    }
    assert!(usage.fuel_consumed > 0);
    assert!(usage.fuel_consumed <= usage.fuel_budget);
    assert!(usage.memory_bytes <= sandbox.limits().max_memory_bytes);
}

#[test]
fn wasm_preview_preserves_missing_provider_values_as_na() {
    let source = concat!(
        "//@pine_lite version=1 overlay=false title=\"Missing flow\"\n",
        "flow = request.data(\"TEST.trade_delta\")\n",
        "plot(flow, title=\"Trade delta\")\n",
    );
    let mut inputs = Inputs::default();
    inputs
        .data_series
        .insert("TEST.trade_delta".into(), vec![f64::NAN, 2.0]);

    let sandbox = Sandbox::new().expect("the shipped Wasm module should compile");
    let (preview, _) = sandbox
        .execute_pine_preview(PinePreviewRequest {
            source: source.into(),
            candles: candles(&[1.0, 2.0]),
            inputs,
        })
        .expect("missing provider values must survive the Wasm JSON boundary");

    let plot = preview
        .plots
        .iter()
        .find(|plot| plot.title == "Trade delta")
        .expect("guest should preserve the non-missing value");
    assert_eq!((plot.low, plot.high), (2.0, 2.0));
}

#[test]
fn wasm_preview_fuel_exhaustion_is_reported() {
    let sandbox = Sandbox::with_limits(
        SandboxLimits::default()
            .with_fuel(100_000)
            .with_timeout(Duration::from_secs(2)),
    )
    .expect("the shipped Wasm module should compile");
    let error = sandbox
        .execute_pine_preview(PinePreviewRequest {
            source: concat!(
                "//@pine_lite version=1 overlay=false\n",
                "var i = 0\n",
                "while true\n",
                "    i := i + 1\n",
                "plot(i)\n",
            )
            .into(),
            candles: candles(&[1.0]),
            inputs: Inputs::default(),
        })
        .expect_err("the Wasm fuel ceiling must stop runaway execution");
    assert!(
        matches!(error, sandbox::SandboxError::FuelExhausted { .. }),
        "{error}"
    );
}

#[test]
fn host_rejects_preview_payloads_over_the_candle_cap_before_guest_entry() {
    let sandbox = Sandbox::new().expect("the shipped Wasm module should compile");
    let candles = vec![candles(&[1.0])[0].clone(); 50_001];
    let error = sandbox
        .execute_pine_preview(PinePreviewRequest {
            source: "//@pine_lite version=1\nplot(close)\n".into(),
            candles,
            inputs: Inputs::default(),
        })
        .expect_err("oversized chart series should be refused by the host");
    assert!(error.to_string().contains("50000 chart candles"), "{error}");
}
