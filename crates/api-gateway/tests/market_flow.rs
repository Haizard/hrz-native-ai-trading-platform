//! `/symbols`, `/orderbook`, and the error envelope on rejections.
//!
//! ## What only a route test can show
//!
//! That the inventory reports the *span* and not just a count; that a missing
//! query parameter comes back in this API's envelope rather than axum's plain
//! text; and that a symbol with no depth says so instead of returning an empty
//! book that reads as a market with no liquidity.
//!
//! These routes are read-only, so unlike the bot tests there is nothing to
//! clean up.

mod common;

use axum::http::StatusCode;
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn the_inventory_reports_every_resolution_and_its_span() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let symbols = body.as_array().expect("a list");
    assert!(!symbols.is_empty(), "the database has candles");

    let btc = symbols
        .iter()
        .find(|s| s["symbol"] == "BTCUSDT")
        .expect("BTCUSDT is loaded");
    let timeframes = btc["timeframes"].as_array().expect("a list");
    assert!(timeframes.len() >= 2, "several resolutions are loaded");

    for tf in timeframes {
        assert!(tf["candles"].as_i64().unwrap_or(0) > 0, "{tf}");
        let first = tf["first"].as_i64().expect("a first timestamp");
        let last = tf["last"].as_i64().expect("a last timestamp");
        // The span, not just the count: "1,110 candles" is reassuring and says
        // nothing, and the whole reason this route exists is that the span is
        // what bites.
        assert!(first <= last, "{tf}");
        assert!(
            tf["missing"].as_i64().is_some(),
            "a known resolution must report its gaps: {tf}"
        );
    }

    // And the warning fires when one resolution covers far less than another,
    // which is the shape of the incident that cost real time here. Asserted as
    // a shape rather than as text, because backfilling 1m would legitimately
    // make it disappear.
    if let Some(note) = btc.get("coverage_note").and_then(|n| n.as_str()) {
        assert!(
            note.contains("covers"),
            "the note must name what is thin: {note}"
        );
    }
}

#[tokio::test]
async fn a_symbol_with_no_depth_says_so_rather_than_returning_an_empty_book() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/orderbook?symbol=BTCUSDT", None).await;
    match status {
        StatusCode::OK => {
            // If depth has been collected, the contract is a book with both
            // sides and a spread.
            assert!(!body["bids"].as_array().expect("bids").is_empty(), "{body}");
            assert!(!body["asks"].as_array().expect("asks").is_empty(), "{body}");
            assert!(body["spread"].as_f64().is_some(), "{body}");
        }
        StatusCode::NOT_FOUND => {
            // An empty book would read as "no liquidity", which is a different
            // and much more alarming claim than "nothing was collected".
            assert_eq!(body["error"]["code"], "NO_ORDERBOOK_DATA", "{body}");
            assert!(
                body["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("collector"),
                "the message must say what would fix it: {body}"
            );
        }
        other => panic!("unexpected status {other}: {body}"),
    }
}

#[tokio::test]
async fn a_missing_query_parameter_comes_back_in_this_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Axum's own rejection is plain text, which a client cannot branch on.
    let (status, body) = h.get("/orderbook", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "QUERY_INVALID");
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn a_malformed_body_comes_back_in_this_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Wrong shape: JSON, but not the shape asked for.
    let (status, body) = h.post("/auth/login", json!({ "email": 42 }), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BODY_INVALID");

    // Not JSON at all: a syntax error, which is a 400 rather than a 422.
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/auth/login")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("not json"))
        .expect("request");
    let response = tower::ServiceExt::oneshot(h.app.clone(), request)
        .await
        .expect("the router must answer");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_market_routes_are_public() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // docs/12: market data is not user data, so anonymous chart viewing is
    // allowed. Everything that belongs to somebody is not.
    let (status, _) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .get("/candles?symbol=BTCUSDT&timeframe=5m&limit=5", None)
        .await;
    assert_eq!(status, StatusCode::OK);
}
