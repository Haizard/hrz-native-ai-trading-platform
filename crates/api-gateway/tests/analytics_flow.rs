//! Durable Core Analytics predictions: record, settle once, report drift.

mod common;

use axum::http::StatusCode;
use common::Harness;

const MIN_MS: i64 = 60_000;

fn seat_around(h: &Harness, symbol: &str, decision_ms: i64) {
    let tf: analytics_core::Timeframe = "1m".parse().unwrap();
    let history = h.supervisor.history();
    let decision_ns = decision_ms * 1_000_000;
    for index in -2..14 {
        let close = 100.0 + (index + 2) as f64 * 0.5;
        history.record_closed(&analytics_core::Candle {
            symbol: symbol.to_string(),
            timeframe: tf,
            open_time: decision_ns + index * tf.nanos(),
            open: close - 0.4,
            high: close + 0.2,
            low: close - 0.6,
            close,
            volume: 10.0,
            buy_volume: 7.0,
            sell_volume: 3.0,
        });
    }
}

#[tokio::test]
async fn a_prediction_is_recorded_settled_once_and_reported() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let decision_ms = market_data::symbols::now_ns() / 1_000_000 - 20 * MIN_MS;
    seat_around(&h, "DRIFTUSDT", decision_ms);

    let (status, body) = h
        .post(
            "/analytics/predictions",
            serde_json::json!({
                "symbol": "driftusdt",
                "timeframe": "1m",
                "discipline": "bayesian",
                "direction": "Bullish",
                "evidence_strength": 0.8,
                "model_version": "test-v1",
                "decision_timestamp_ms": decision_ms,
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["prediction"]["symbol"], "DRIFTUSDT");
    assert_eq!(body["prediction"]["settled_at"], serde_json::Value::Null);

    let (status, body) = h
        .get(
            "/analytics/predictions?unsettled=true",
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["predictions"].as_array().unwrap().len(), 1);

    let (status, body) = h
        .post(
            "/analytics/predictions/settle",
            serde_json::json!({
                "symbol": "DRIFTUSDT",
                "horizon_ms": 10 * MIN_MS,
                "neutral_threshold": 0.001,
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1, "{body}");

    // A second settlement cannot overwrite the realized outcome.
    let (_, body) = h
        .post(
            "/analytics/predictions/settle",
            serde_json::json!({"horizon_ms": 10 * MIN_MS}),
            Some(&user.token),
        )
        .await;
    assert_eq!(body["settled"], 0, "{body}");

    let (status, body) = h.get("/analytics/drift", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settled"], 1, "{body}");
    assert_eq!(body["promotion"]["promote"], false, "{body}");
    assert!(body["note"]
        .as_str()
        .unwrap()
        .contains("risk review"));
}

#[tokio::test]
async fn a_bad_prediction_is_refused_and_auth_is_required() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let (status, body) = h
        .post(
            "/analytics/predictions",
            serde_json::json!({
                "symbol": "BTCUSDT",
                "timeframe": "1m",
                "discipline": "bayesian",
                "direction": "Sideways",
                "evidence_strength": 0.5,
                "model_version": "v1",
                "decision_timestamp_ms": 1_700_000_000_000i64,
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _) = h
        .get("/analytics/predictions", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
