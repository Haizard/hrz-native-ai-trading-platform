//! Markov evidence journal round-trip against the local database.
//!
//! A record written after `analyze_markov` must be settleable once the
//! forecast horizon has matured, and settling must be one-shot.

mod common;

use axum::http::StatusCode;
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn markov_evidence_is_recorded_and_settled_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZMRK{}", uuid::Uuid::new_v4().simple()).to_uppercase();
    let timeframe = analytics_core::Timeframe::M5;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos() as i64;
    let candles: Vec<analytics_core::Candle> = (0..12)
        .map(|i| analytics_core::Candle {
            symbol: symbol.clone(),
            timeframe,
            open_time: now - (12 - i) * timeframe.nanos(),
            open: 100.0 + i as f64,
            high: 101.0 + i as f64,
            low: 99.0 + i as f64,
            close: 100.5 + i as f64,
            volume: 1.0,
            buy_volume: 0.7,
            sell_volume: 0.3,
        })
        .collect();
    db::repositories::insert_candles(h.database.pool(), &candles)
        .await
        .expect("seeding candles must succeed");

    let row = db::markov_evidence::insert_evidence(
        h.database.pool(),
        user.id,
        &db::markov_evidence::NewMarkovEvidence {
            symbol: symbol.clone(),
            timeframe: "5m".into(),
            evidence: json!({
                "instrument": symbol,
                "timeframe": "5m",
                "current_regime": "bull_trend",
                "model_version": "markov-engine-v1"
            }),
            model_version: "markov-engine-v1".into(),
            horizon_bars: Some(4.0),
            observed_at: now - 8 * timeframe.nanos(),
        },
    )
    .await
    .expect("the evidence row must be stored");
    assert!(row.settled_at.is_none());

    let (status, body) = h
        .post(
            &format!("/markov/evidence/{}/settle", row.id),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"]["direction_hit"], true, "{body}");

    let settled = db::markov_evidence::get_evidence(h.database.pool(), user.id, row.id)
        .await
        .expect("the row must be readable")
        .expect("the row still exists");
    assert!(settled.settled_at.is_some());
    assert!(settled.outcome.is_some());

    let (second, _) = h
        .post(
            &format!("/markov/evidence/{}/settle", row.id),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(second, StatusCode::CONFLICT, "settlement must be one-shot");

    db::repositories::delete_candles_for_symbol(&h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
}
