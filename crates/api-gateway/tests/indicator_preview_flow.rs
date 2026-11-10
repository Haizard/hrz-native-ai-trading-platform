//! End-to-end proof that a workspace generation turn stores a **non-empty**
//! preview.
//!
//! ## What this is for
//!
//! The generation path used to store an empty preview -- a revision whose
//! chart described nothing. The fix is to *run* what the model produced over
//! candles loaded from the database and store the counts; this test drives
//! that whole chain the way a client would: the message route, the script
//! preview replay, and the revision write.
//!
//! ## The one stubbed link
//!
//! The LLM is a [`ScriptedClient`] that returns a fixed script, because a live
//! model is neither reachable nor deterministic here. Everything after it is
//! the real code: the real `create_message` handler, the real vet pipeline,
//! the real `replay_script_preview`, the real database.
//!
//! ## The script is guaranteed to plot and emit one box and label
//!
//! `plot(ta.ema(close, 9))` is finite on any candle with a past, so a
//! non-empty preview is guaranteed if the plumbing works at all. The final
//! bar also emits one drawing of each kind so server-measured counts are
//! checked against interpreter output.

mod common;

use std::sync::Arc;

use ai_agent::llm_client::ScriptedClient;
use ai_agent::{AgentConfig, ContentBlock, LlmResponse, Message, Role, StopReason, Usage};
use analytics_core::types::{Candle, Timeframe};
use serde_json::json;

/// The scripted script: an EMA line plus one final-bar box and label.
const FIREABLE_SCRIPT: &str = concat!(
    "//@pine_lite version=1 overlay=true title=\"EMA probe\"\n",
    "e = ta.ema(close, 9)\n",
    "plot(e, title=\"EMA9\", color=color.orange)\n",
    "if bar_index == last_bar_index\n",
    "    box.new(bar_index - 2, high, bar_index, low, color=color.teal)\n",
    "    label.new(bar_index, high, \"preview\", color=color.teal)\n",
);

const RUNTIME_FAILURE_SCRIPT: &str = concat!(
    "//@pine_lite version=1 overlay=false title=\"Runtime failure\"\n",
    "reset_time = input.timeframe(defval=\"D\", title=\"Reset CVD on\")\n",
    "plot(timeframe.change(reset_time) ? close : na, title=\"Reset\")\n",
);

/// A scripted agent whose drafts are returned in order.
fn scripted_agent_with_scripts(scripts: &[&str]) -> Arc<ai_agent::Agent> {
    let responses = scripts
        .iter()
        .enumerate()
        .map(|(index, script)| LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: format!("d{}", index + 1),
                    name: "submit_script".into(),
                    input: json!({
                        "script": script,
                        "report": "Model report without revision or replay details."
                    }),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        })
        .collect();
    Arc::new(ai_agent::Agent::new(
        Arc::new(ScriptedClient::new(responses)),
        ai_agent::SkillLibrary::new(),
        AgentConfig::default(),
    ))
}

/// A scripted agent whose one draft is [`FIREABLE_SCRIPT`].
fn scripted_agent() -> Arc<ai_agent::Agent> {
    scripted_agent_with_scripts(&[FIREABLE_SCRIPT])
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
            json!({ "content": "plot an ema" }),
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
    let assistant = &body["assistant_message"];
    let assistant_text = assistant["content"].as_str().expect("assistant report");
    let preview_stats = &assistant["payload"]["preview_stats"];
    assert_eq!(
        assistant["payload"]["revision_number"],
        revision["revision_number"]
    );
    assert_eq!(assistant["payload"]["revision_status"], "validated");
    assert_eq!(preview_stats["drawing_objects"], 2);
    assert_eq!(preview_stats["boxes"], 1);
    assert_eq!(preview_stats["labels"], 1);
    assert_eq!(preview_stats["objects_truncated"], false);
    assert!(
        assistant_text.contains("Model report without revision or replay details."),
        "the model's useful report must be preserved: {assistant_text}"
    );
    assert!(
        assistant_text.contains(&format!(
            "Saved as active validated revision #{}. {}-day preview: {} bars, {} plot(s), {} level(s), {} marker(s), and {} drawing object(s), including {} box(es) and {} label(s).",
            revision["revision_number"].as_i64().expect("revision number"),
            preview_stats["window_days"].as_i64().expect("preview days"),
            preview_stats["bars"].as_u64().expect("preview bars"),
            preview_stats["plots"].as_u64().expect("preview plots"),
            preview_stats["levels"].as_u64().expect("preview levels"),
            preview_stats["shapes"].as_u64().expect("preview markers"),
            preview_stats["drawing_objects"]
                .as_u64()
                .expect("preview drawings"),
            preview_stats["boxes"].as_u64().expect("preview boxes"),
            preview_stats["labels"].as_u64().expect("preview labels"),
        )),
        "the assistant must report server-computed revision and preview facts: {assistant_text}"
    );

    let zones = preview["zones"].as_array().expect("a zones array");
    assert!(
        !zones.is_empty(),
        "one finite EMA plot over a week of candles is one preview zone; \
         the empty-preview bug is back: {preview}"
    );

    // The stored source is the script, and read back through the API the
    // stored revision is the same non-empty one.
    assert!(
        revision["source"]
            .as_str()
            .is_some_and(|s| s.contains("//@pine_lite")),
        "the revision stores the script as source: {revision}"
    );
    let listed = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    assert_eq!(listed[0]["id"], revision["id"]);
    assert_eq!(listed[0]["revision_number"], revision["revision_number"]);
    let workspace = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        workspace["active_revision_id"], revision["id"],
        "a successful validated generation must become the workspace's active revision"
    );
    let stored = &listed[0]["preview"]["zones"];
    assert!(
        stored.as_array().is_some_and(|z| !z.is_empty()),
        "the stored revision's preview is empty: {listed}"
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

#[tokio::test]
async fn runtime_preview_error_is_sent_back_for_repair_before_revision_creation() {
    let Some(h) = common::Harness::with_agent(scripted_agent_with_scripts(&[
        RUNTIME_FAILURE_SCRIPT,
        FIREABLE_SCRIPT,
    ]))
    .await
    else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZREPAIR{}", uuid::Uuid::new_v4().simple()).to_uppercase();
    db::repositories::insert_candles(&h.database.pool(), &seed_candles(&symbol))
        .await
        .expect("seeding candles must succeed");
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Runtime repair probe", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "plot the reset event" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "the runtime repair did not recover: {body}"
    );
    let revision = &body["revision"];
    assert_eq!(revision["validation"]["attempts"], 2);
    assert!(
        revision["validation"]["repaired_errors"]
            .as_array()
            .is_some_and(|errors| errors.iter().any(|e| {
                e.as_str()
                    .is_some_and(|message| message.contains("runtime preview failed"))
            })),
        "the runtime failure should be recorded with the repaired revision: {revision}"
    );
    assert!(
        revision["source"]
            .as_str()
            .is_some_and(|s| s.contains("plot(e")),
        "the repaired source, not the failed candidate, must be stored: {revision}"
    );
    assert!(
        revision["preview"]["zones"]
            .as_array()
            .is_some_and(|zones| !zones.is_empty()),
        "the repaired revision must include a usable plotted preview: {revision}"
    );

    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::repositories::delete_candles_for_symbol(&h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn unrepaired_runtime_preview_error_does_not_create_a_revision() {
    let Some(h) = common::Harness::with_agent(scripted_agent_with_scripts(&[
        RUNTIME_FAILURE_SCRIPT,
        RUNTIME_FAILURE_SCRIPT,
    ]))
    .await
    else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;
    let symbol = format!("ZZNOREPAIR{}", uuid::Uuid::new_v4().simple()).to_uppercase();
    db::repositories::insert_candles(&h.database.pool(), &seed_candles(&symbol))
        .await
        .expect("seeding candles must succeed");
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Runtime failure probe", "symbol": symbol, "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/messages"),
            json!({ "content": "plot the reset event" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "{body}"
    );
    assert_eq!(body["error"]["code"], "INDICATOR_PREVIEW_FAILED");
    let revisions = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    assert!(
        revisions.as_array().is_some_and(Vec::is_empty),
        "a script that still fails runtime preview must not be persisted: {revisions}"
    );

    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("workspace cleanup");
    db::repositories::delete_candles_for_symbol(&h.database.pool(), &symbol)
        .await
        .expect("candle cleanup");
    user.cleanup(&h.database).await;
}
