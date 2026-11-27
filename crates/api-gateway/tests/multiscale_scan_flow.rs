//! `GET /scan/multiscale` end to end, against the real router.
//!
//! The unit tests in `multiscale_routes` cover parsing and overlay assembly;
//! this file proves the route is wired into the router, reads the same candle
//! buffer the market data layer uses, and returns chart-ready overlays.

mod common;

use axum::http::StatusCode;
use common::Harness;

fn seat(h: &Harness, symbol: &str, timeframe: &str, count: usize) {
    let parsed: analytics_core::types::Timeframe = timeframe.parse().expect("a known resolution");
    let step_ns = parsed.nanos();
    let history = h.supervisor.history();
    let now_ns = market_data::symbols::now_ns();
    let newest_open = (now_ns / step_ns) * step_ns;

    for index in 0..count {
        let i = index as i64;
        let close = 100.0 + i as f64 * 0.3 + (index % 5) as f64 * 0.05;
        let open = close - 0.5;
        history.record_closed(&analytics_core::Candle {
            symbol: symbol.to_string(),
            timeframe: parsed,
            open_time: newest_open - (count as i64 - 1 - i) * step_ns,
            open,
            high: close + 0.2,
            low: open - 0.2,
            close,
            volume: 10.0,
            buy_volume: 7.0,
            sell_volume: 3.0,
        });
    }
}

#[tokio::test]
async fn the_multiscale_scan_route_reads_a_ladder_and_returns_overlays() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for timeframe in ["1h", "15m", "5m"] {
        seat(&h, "MTFUSDT", timeframe, 90);
    }

    let (status, body) = h
        .get(
            "/scan/multiscale?symbols=MTFUSDT&timeframes=1h,15m,5m&discipline=bayesian&lookback=60",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discipline"], "bayesian");
    assert_eq!(body["timeframes"], serde_json::json!(["1h", "15m", "5m"]));
    assert!(body["note"]
        .as_str()
        .unwrap()
        .contains("risk gate"));

    let rows = body["rows"].as_array().expect("rows is an array");
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["symbol"], "MTFUSDT");
    assert!(rows[0]["ranking_score"].as_f64().is_some());
    assert!(body["failures"].as_array().unwrap().is_empty(), "{body}");
    assert!(rows[0]["overlays"].as_array().is_some());
}

#[tokio::test]
async fn the_multiscale_scan_route_refuses_an_unknown_discipline() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (status, body) = h
        .get(
            "/scan/multiscale?symbols=MTFUSDT&timeframes=1h,5m&discipline=does_not_exist",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unknown_discipline", "{body}");
}

#[tokio::test]
async fn the_multiscale_scan_route_refuses_an_unknown_timeframe() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (status, body) = h
        .get("/scan/multiscale?symbols=MTFUSDT&timeframes=9x", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unknown_timeframe", "{body}");
}
