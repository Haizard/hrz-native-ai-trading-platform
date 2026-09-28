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
//! The LLM is a [`ScriptedClient`] returning a fixed pine-lite script,
//! because a live model is neither reachable nor deterministic here.
//! Everything after it is real: the real `create_message` handler, the real
//! database, the real vet pipeline and the real script preview replay.

mod common;

use std::sync::Arc;

use ai_agent::llm_client::ScriptedClient;
use ai_agent::{AgentConfig, ContentBlock, LlmResponse, Message, Role, StopReason, Usage};
use analytics_core::types::{Candle, Timeframe};
use serde_json::json;

/// The scripted script: an EMA line, the smallest script whose preview has
/// something finite to report on the seeded candles.
const INDICATOR_SCRIPT: &str = concat!(
    "//@pine_lite version=1 overlay=false title=\"EMA probe\"\n",
    "len = input.int(defval=9, title=\"EMA Length\")\n",
    "e = ta.ema(close, len)\n",
    "plot(e, title=\"EMA9\", color=color.orange)\n",
);

/// A scripted agent whose one draft is [`INDICATOR_SCRIPT`].
fn scripted_agent() -> Arc<ai_agent::Agent> {
    let response = LlmResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "d1".into(),
                name: "submit_script".into(),
                input: json!({ "script": INDICATOR_SCRIPT }),
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

/// A week of 5m candles ending at the last closed bar -- a gentle uptrend
/// with a sine wobble, enough for an EMA to draw a finite line over.
fn seed_candles(symbol: &str) -> Vec<Candle> {
    let width = Timeframe::M5.nanos();
    let latest_open = (now_ns() / width - 1) * width;
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
async fn a_script_generation_turn_stores_code_and_preview() {
    let Some(h) = common::Harness::with_agent(scripted_agent()).await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZSCRIPT{}", uuid::Uuid::new_v4().simple()).to_uppercase();

    // Candles first, so the preview has data by the time the message arrives.
    let candles = seed_candles(&symbol);
    db::repositories::insert_candles(h.database.pool(), &candles)
        .await
        .expect("seeding candles must succeed");

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Script probe", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "plot an ema of the close" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "the generation turn failed: {body}"
    );

    // The revision stores the SCRIPT as source, vetted by pine-lite's real
    // pipeline -- no YAML anywhere.
    let revision = &body["revision"];
    assert_eq!(revision["status"], "validated", "revision: {revision}");
    let source = revision["source"].as_str().expect("a source string");
    assert!(
        source.contains("//@pine_lite"),
        "the stored source is the script, not a document: {source}"
    );
    assert!(
        source.contains("ta.ema"),
        "the stored source is the model's code: {source}"
    );
    assert!(
        !source.contains("kind: indicator"),
        "a YAML document must not leak into a code revision: {source}"
    );
    // The validation block names the code engine, not the DSL's.
    assert_eq!(
        revision["validation"]["engine"], "pine-lite-v1",
        "the vetting engine must be the script one: {revision}"
    );

    // The preview ran the script over the seeded candles and reported counts.
    let preview = &revision["preview"];
    assert!(
        preview["zones"].as_array().is_some_and(|z| !z.is_empty()),
        "one finite plot over a week of candles is one preview zone: {preview}"
    );

    // The assistant message reports the code generation honestly.
    let text = body["assistant_message"]["content"]
        .as_str()
        .expect("an assistant message");
    assert!(
        text.contains("pine-lite script"),
        "the message must name the representation: {text}"
    );

    // Cleanup: children before the user, nothing left behind.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::repositories::delete_candles_for_symbol(h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
    user.cleanup(&h.database).await;
}

/// A script over an empty store must still produce a *valid* revision -- an
/// honest empty preview -- and the assistant message must say so rather than
/// inventing detections.
#[tokio::test]
async fn a_script_with_no_data_stays_valid_and_honest() {
    let Some(h) = common::Harness::with_agent(scripted_agent()).await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    // No candles seeded at all: the preview runs over a real but empty series.
    let symbol = format!("ZZSCRIPT{}", uuid::Uuid::new_v4().simple()).to_uppercase();

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Empty script", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "plot an ema of the close" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "a script with no data must still generate: {body}"
    );

    let revision = &body["revision"];
    assert_eq!(revision["status"], "validated", "revision: {revision}");
    let preview = &revision["preview"];
    assert_eq!(
        preview["zones"].as_array().map(Vec::len),
        Some(0),
        "no candles means no zones: {preview}"
    );

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    user.cleanup(&h.database).await;
}
