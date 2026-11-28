//! `GET /kronos/forecast` with the committed pretrained weights.

mod common;

use axum::http::StatusCode;
use common::Harness;

#[tokio::test]
async fn the_kronos_route_returns_a_pretrained_forecast() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let step = analytics_core::Timeframe::M5.nanos();
    let now = market_data::symbols::now_ns();
    let newest = (now / step) * step;
    let candles: Vec<analytics_core::Candle> = (0..64)
        .map(|index| {
            let close = 100.0 + index as f64 * 0.2;
            analytics_core::Candle {
                symbol: "KRONOSTEST".to_string(),
                timeframe: analytics_core::Timeframe::M5,
                open_time: newest - (63 - index) * step,
                open: close - 0.2,
                high: close + 0.4,
                low: close - 0.4,
                close,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            }
        })
        .collect();
    for candle in &candles {
        h.supervisor.history().record_closed(candle);
    }

    let (status, body) = h
        .get("/kronos/forecast?symbol=KRONOSTEST&timeframe=5m&lookback=32&horizon=2", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["symbol"], "KRONOSTEST");
    assert_eq!(body["forecast"]["bars"].as_array().unwrap().len(), 2);
    assert!(body["forecast"]["limitation"]
        .as_str()
        .unwrap()
        .contains("pretrained"));
}
