//! End-to-end proof that a `kind: indicator` generation turn stores a preview
//! with **real detection output**, not an empty one.
//!
//! ## What this is for
//!
//! The `kind: indicator` path in `replay_preview` used to return
//! `IndicatorOutput::default()` unconditionally, so a validated indicator
//! revision described nothing on the chart. The fix evaluates the document's
//! concepts over the backfilled entry series, but the claim "the revision now
//! carries zones/markers/evidence" spans the message route, the series loading,
//! the detector, and the revision write -- more than the crate's unit tests
//! cover. This test drives the whole thing the way a client would.
//!
//! ## The one stubbed link
//!
//! The LLM is a [`ScriptedClient`] returning a fixed `kind: indicator`
//! document, because a live model is neither reachable nor deterministic here.
//! Everything after it is real: the real `create_message` handler, the real
//! database, the real `replay_preview` indicator branch, the real detector.
//!
//! ## The fixture is guaranteed to fire
//!
//! `seed_candles` plants a fair-value-gap shape -- a three-candle window whose
//! `high(0)` sits strictly below `low(2)` -- so the scripted concept cannot
//! fail to match. If the wiring breaks anywhere, the zone count goes to zero
//! and the assertion names it.

mod common;

use std::sync::Arc;

use ai_agent::llm_client::ScriptedClient;
use ai_agent::{AgentConfig, ContentBlock, LlmResponse, Message, Role, StopReason, Usage};
use analytics_core::types::{Candle, Timeframe};
use serde_json::json;

/// The scripted document. A `kind: indicator` declares only concepts: no
/// `entry`, no `risk`, no `invalidation`. The concept is the platform's own
/// worked fair-value-gap example, whose fixture shape `seed_candles` plants.
const INDICATOR_DOCUMENT: &str = r#"
name: "Detector probe"
version: "1.0"
kind: indicator
market: BTCUSDT
timeframes:
  entry: 5m
concepts:
  - name: bullish_gap
    label: fvg
    side: buy
    window: 3
    lower: {high: 0}
    upper: {low: 2}
    require:
      - {left: {high: 0}, op: below, right: {low: 2}}
    min_band_ratio: 0.2
"#;

/// A scripted agent whose one draft is [`INDICATOR_DOCUMENT`].
fn scripted_agent() -> Arc<ai_agent::Agent> {
    let response = LlmResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "d1".into(),
                name: "draft_strategy".into(),
                input: json!({ "yaml": INDICATOR_DOCUMENT }),
            }],
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
    };
    Arc::new(ai_agent::Agent::new(
        Arc::new(ScriptedClient::new(vec![response])),
        ai_agent::SkillLibrary::new(),
        AgentConfig::default(),
    ))
}

/// Now, in unix nanoseconds -- matching the platform's clock.
fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i64)
}

/// A week of 5m candles ending at the last closed bar, with the fvg shape
/// planted mid-series.
///
/// The body of the series is a gentle uptrend with a sine wobble -- ordinary
/// bars the detector may or may not also match, which is fine: the test pins
/// the *planted* band by its exact prices rather than a total count.
fn seed_candles(symbol: &str) -> Vec<Candle> {
    let width = Timeframe::M5.nanos();
    let latest_open = (now_ns() / width - 1) * width;
    let count: i64 = 7 * 24 * 12 + 120;
    // The planted fair-value-gap window: three hand-built candles buried
    // mid-series, whose edges are exactly PLANTED_LOW/PLANTED_HIGH. The
    // sine wobble below never produces a gap on its own -- its highs always
    // overlap later lows -- which silently emptied this fixture once.
    const PLANT_AT: i64 = 1000;
    (0..count)
        .map(|i| {
            let open_time = latest_open - (count - 1 - i) * width;
            let base = 100.0 + (i as f64 * 0.01) + ((i as f64 * 0.15).sin() * 2.0);
            let mut open = base;
            let mut close = base + ((i as f64 * 0.15).cos() * 0.4);
            if i == PLANT_AT {
                // Candle A: its high is the band's floor, exactly 100.5.
                open = 99.9;
                close = 100.2;
            } else if i == PLANT_AT + 1 {
                // Candle B: entirely inside the band, bridging the gap.
                open = 101.6;
                close = 101.9;
            } else if i == PLANT_AT + 2 {
                // Candle C: its low is the band's ceiling, exactly 104.0.
                open = 104.3;
                close = 105.0;
            }
            let high = open.max(close) + 0.3;
            let low = open.min(close) - 0.3;
            Candle {
                symbol: symbol.to_string(),
                timeframe: Timeframe::M5,
                open_time,
                open,
                high,
                low,
                close,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            }
        })
        .collect()
}

/// The planted band's exact edges: candle A's high, candle C's low.
///
/// The planted candles give `high(0) = 100.5 < low(2) = 104.0`, a band of
/// height 3.5 against a window range of 5.7 -- a 0.61 ratio, well over the
/// document's `min_band_ratio: 0.2`.
const PLANTED_LOW: f64 = 100.5;
const PLANTED_HIGH: f64 = 104.0;

#[tokio::test]
async fn an_indicator_generation_turn_stores_real_detection_output() {
    let Some(h) = common::Harness::with_agent(scripted_agent()).await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZDETECT{}", uuid::Uuid::new_v4().simple()).to_uppercase();

    // Candles first, so the detector has data by the time the message arrives.
    let candles = seed_candles(&symbol);
    db::repositories::insert_candles(h.database.pool(), &candles)
        .await
        .expect("seeding candles must succeed");

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Detector probe", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "detect fair value gaps" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "the generation turn failed: {body}"
    );

    let revision = &body["revision"];
    assert_eq!(revision["status"], "validated", "revision: {revision}");
    let preview = &revision["preview"];

    let evidence = preview["evidence"].as_array().expect("an evidence array");
    let markers = preview["markers"].as_array().expect("a markers array");
    let zones = preview["zones"].as_array().expect("a zones array");

    assert!(
        !zones.is_empty(),
        "the preview carries no zones; the empty-indicator-preview bug is back: {preview}"
    );
    assert_eq!(
        zones.len(),
        markers.len(),
        "every detection band must come with its trigger marker: {preview}"
    );
    assert_eq!(
        zones.len(),
        evidence.len(),
        "every detection band must cite one evidence node: {preview}"
    );

    // The planted band must be among the detections, at its exact prices --
    // this is what pins the test to the fixture rather than to any incidental
    // match the wobble may also produce.
    assert!(
        zones.iter().any(|zone| {
            zone["price_low"].as_f64() == Some(PLANTED_LOW)
                && zone["price_high"].as_f64() == Some(PLANTED_HIGH)
        }),
        "the planted {PLANTED_LOW}..{PLANTED_HIGH} band was not detected: {zones:#?}"
    );

    // A marker must cite evidence that exists, and the marker kind must follow
    // the concept's side (buy -> bullish).
    let ids: Vec<&str> = evidence.iter().filter_map(|e| e["id"].as_str()).collect();
    for marker in markers {
        let cited = marker["evidence_id"].as_str().expect("a cited evidence id");
        assert!(
            ids.contains(&cited),
            "marker cites evidence `{cited}` that is absent"
        );
        assert_eq!(
            marker["kind"], "bullish",
            "a buy-side concept marks bullish: {marker}"
        );
    }

    // Every evidence node names the concept that fired.
    for node in evidence {
        assert_eq!(
            node["event"], "bullish_gap",
            "evidence must name its concept: {node}"
        );
    }

    // The assistant message must stay honest about what an indicator is: a
    // detector layer, zero setups by design.
    let assistant = &body["assistant_message"]["content"];
    let text = assistant.as_str().expect("an assistant message");
    assert!(
        text.contains("0 trading setups"),
        "the message must state the indicator fires 0 setups by design: {text}"
    );

    // The stored revision, read back through the API, carries the same output.
    let listed = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    let stored = &listed[0]["preview"];
    assert!(
        stored["zones"].as_array().is_some_and(|z| !z.is_empty()),
        "the stored revision's zones are empty: {listed}"
    );
    assert!(
        stored["markers"].as_array().is_some_and(|m| !m.is_empty()),
        "the stored revision's markers are empty: {listed}"
    );

    // Cleanup: children before the user, candles by symbol, nothing left behind.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::strategies::delete_strategy(
        h.database.pool(),
        body["strategy_id"].as_str().unwrap().parse().unwrap(),
    )
    .await
    .expect("strategy cleanup");
    db::repositories::delete_candles_for_symbol(h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
    user.cleanup(&h.database).await;
}

/// A concept that matches nothing must still produce a *valid* revision with an
/// empty preview -- an honest empty, not a failure -- and the assistant message
/// must say so rather than inventing detections.
#[tokio::test]
async fn an_indicator_that_matches_nothing_stays_valid_and_honest() {
    let Some(h) = common::Harness::with_agent(scripted_agent()).await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    // No candles seeded at all: the detector has a real but empty series.
    let symbol = format!("ZZDETECT{}", uuid::Uuid::new_v4().simple()).to_uppercase();

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Empty detector", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "detect fair value gaps" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "an indicator with no data must still generate: {body}"
    );

    let revision = &body["revision"];
    assert_eq!(revision["status"], "validated", "revision: {revision}");
    let preview = &revision["preview"];
    assert_eq!(
        preview["zones"].as_array().map(Vec::len),
        Some(0),
        "no candles means no zones: {preview}"
    );

    let assistant = &body["assistant_message"]["content"];
    let text = assistant.as_str().expect("an assistant message");
    assert!(
        text.contains("no detection bands fired"),
        "the message must explain the empty window honestly: {text}"
    );

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::strategies::delete_strategy(
        h.database.pool(),
        body["strategy_id"].as_str().unwrap().parse().unwrap(),
    )
    .await
    .expect("strategy cleanup");
    user.cleanup(&h.database).await;
}
