use analytics_core::types::{Candle, Timeframe};
use api_gateway::pine_preview_runner::{PinePreviewExecutor, PinePreviewWorker};
use pine_lite::interp::Inputs;
use sandbox::PinePreviewRequest;

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

fn worker() -> PinePreviewWorker {
    PinePreviewWorker::new(env!("CARGO_BIN_EXE_pine-preview-worker"))
        .expect("Cargo builds the worker beside the integration test")
}

#[tokio::test]
async fn worker_roundtrips_missing_provider_values_and_preview_evidence() {
    let mut inputs = Inputs::default();
    inputs
        .data_series
        .insert("TEST.trade_delta".into(), vec![f64::NAN, 4.0]);
    let request = PinePreviewRequest {
        source: concat!(
            "//@pine_lite version=1 overlay=false title=\"Worker flow\"\n",
            "flow = request.data(\"TEST.trade_delta\")\n",
            "plot(flow, title=\"Trade delta\")\n",
        )
        .into(),
        candles: candles(&[1.0, 2.0]),
        inputs,
    };
    let worker = worker();
    let preview = worker
        .execute(request.clone())
        .await
        .expect("the isolated worker should return preview evidence");
    let repeated = worker
        .execute(request)
        .await
        .expect("the worker should safely handle a second framed request");

    assert_eq!(preview.plot_count, 1);
    assert_eq!(preview.plots.len(), 1);
    assert_eq!(preview.plots[0].title, "Trade delta");
    assert_eq!((preview.plots[0].low, preview.plots[0].high), (4.0, 4.0));
    assert_eq!((repeated.plots[0].low, repeated.plots[0].high), (4.0, 4.0));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_worker_proves_filesystem_network_and_environment_denial() {
    worker()
        .validate_isolation()
        .await
        .expect("Linux worker must fail closed unless kernel restrictions work");
}
