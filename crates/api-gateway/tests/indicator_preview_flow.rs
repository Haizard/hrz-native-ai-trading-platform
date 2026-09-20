//! End-to-end proof that a workspace generation turn stores a **non-empty**
//! preview.
//!
//! ## What this is for
//!
//! The generation path used to store:
//!
//! ```text
//! IndicatorOutput { evidence: vec![], zones: vec![], markers: vec![], links: vec![] }
//! ```
//!
//! -- a revision whose chart described nothing. The replay-and-translate fix is
//! only as good as the claim "a revision now carries real evidence", and that
//! claim spans a lot this crate's unit tests do not: the message route, the
//! sandboxed replay over **candles loaded from the database**, the evidence
//! builder, and the revision write. So this test drives the whole thing the way
//! a client would.
//!
//! ## The one stubbed link
//!
//! The LLM is a [`ScriptedClient`] that returns a fixed document, because a live
//! model is neither reachable nor deterministic here. Everything after it is the
//! real code: the real `create_message` handler, the real sandbox, the real
//! `indicator_preview::replay_preview`, the real database. The agent's *output*
//! is stubbed; its *consumption* is not.
//!
//! ## The document is deliberately guaranteed to fire
//!
//! `close > threshold(0)` is always true, so the replay cannot produce an empty
//! preview by accident -- a test that passed because the strategy happened never
//! to signal would prove nothing about the wiring. If the chain breaks, the
//! evidence count goes to zero and the assertion says so.

mod common;

use std::sync::Arc;

use ai_agent::llm_client::ScriptedClient;
use ai_agent::{AgentConfig, ContentBlock, LlmResponse, Message, Role, StopReason, Usage};
use analytics_core::types::{Candle, Timeframe};
use serde_json::json;

/// The scripted document. `close > threshold(0)` fires on every bar, so a
/// non-empty preview is guaranteed if the plumbing works at all.
const FIREABLE_STRATEGY: &str = r#"
name: "Preview probe"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: close > threshold(0)
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: "risk_multiple"
    value: 1.0
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
"#;

/// A scripted agent whose one draft is [`FIREABLE_STRATEGY`].
fn scripted_agent() -> Arc<ai_agent::Agent> {
    let response = LlmResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "d1".into(),
                name: "draft_strategy".into(),
                input: json!({ "yaml": FIREABLE_STRATEGY }),
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

/// A week of 5m candles for `symbol`, ending at the last closed 5m bar.
///
/// A gentle uptrend with a sine wobble: enough shape that `below_recent_low`
/// always resolves a stop on the correct side of a long entry, so setups become
/// trades rather than skips.
fn seed_candles(symbol: &str) -> Vec<Candle> {
    let width = Timeframe::M5.nanos();
    let latest_open = (now_ns() / width - 1) * width;
    // Seven days plus a margin, so the window's leading edge is covered.
    let count: i64 = 7 * 24 * 12 + 120;
    (0..count)
        .map(|i| {
            let open_time = latest_open - (count - 1 - i) * width;
            let base = 100.0 + (i as f64 * 0.01) + ((i as f64 * 0.15).sin() * 2.0);
            let open = base;
            let close = base + ((i as f64 * 0.15).cos() * 0.4);
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

#[tokio::test]
async fn a_generation_turn_stores_a_non_empty_validated_preview() {
    let Some(h) = common::Harness::with_agent(scripted_agent()).await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZPREVIEW{}", uuid::Uuid::new_v4().simple()).to_uppercase();

    // Candles first, so the replay has data by the time the message arrives.
    let candles = seed_candles(&symbol);
    db::repositories::insert_candles(h.database.pool(), &candles)
        .await
        .expect("seeding candles must succeed");

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Preview probe", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "give me an always-on long" }),
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
    let links = preview["links"].as_array().expect("a links array");
    let zones = preview["zones"].as_array().expect("a zones array");

    assert!(
        !evidence.is_empty(),
        "the preview carries no evidence; the empty-preview bug is back: {preview}"
    );
    assert!(!markers.is_empty(), "evidence with no markers: {preview}");
    assert!(!links.is_empty(), "evidence with no links: {preview}");
    assert!(!zones.is_empty(), "an entry with no risk band: {preview}");

    // A marker must cite evidence that exists, which is the chain's own contract.
    let ids: Vec<&str> = evidence.iter().filter_map(|e| e["id"].as_str()).collect();
    for marker in markers {
        let cited = marker["evidence_id"].as_str().expect("a cited evidence id");
        assert!(
            ids.contains(&cited),
            "marker cites evidence `{cited}` that is absent"
        );
    }

    // The stored revision, read back through the API, is the same non-empty one.
    let listed = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    let stored = &listed[0]["preview"]["evidence"];
    assert!(
        stored.as_array().is_some_and(|nodes| !nodes.is_empty()),
        "the stored revision's preview is empty: {listed}"
    );

    // Cleanup: children before the user, candles by symbol, nothing left behind.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::strategies::delete_strategy(h.database.pool(), body["strategy_id"].as_str().unwrap().parse().unwrap())
        .await
        .expect("strategy cleanup");
    db::repositories::delete_candles_for_symbol(h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
    user.cleanup(&h.database).await;
}
