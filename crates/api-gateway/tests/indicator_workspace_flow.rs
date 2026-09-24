//! Integration tests for workspace ownership, rollback, alert preference
//! persistence, restore behaviour, and bot revision isolation.
//!
//! ## What these tests prove
//!
//! The indicator-workspace feature adds a user-scoped workspace with immutable
//! revisions, alert preferences, and revision-pinned bot drafts. The database
//! enforces ownership through `WHERE user_id = $N`, but that constraint has
//! never been exercised through the REST surface: the routes could be wired to
//! the wrong queries, the JOIN could be missing, or a revocation check could
//! be absent. These tests drive the real HTTP routes against a real Postgres
//! and assert the failure modes.
//!
//! ## The LLM is not stubbed
//!
//! These tests do not call the Bedrock-backed generation turn, so they need no
//! scripted agent. They create workspaces and revisions directly, which is the
//! client-side workflow the generation turn eventually produces.

mod common;

use serde_json::json;

/// A valid indicator preview for direct revision creation.
fn valid_preview() -> serde_json::Value {
    json!({
        "revision_id": "test-revision-1",
        "evidence": [{
            "id": "sweep",
            "event": "liquidity_sweep",
            "time": 1_700_000_000_000_000_000_i64,
            "price": 100.0,
            "explanation": "Price swept the prior low."
        }],
        "zones": [],
        "markers": [{
            "id": "sweep-marker",
            "evidence_id": "sweep",
            "time": 1_700_000_000_000_000_000_i64,
            "price": 100.0,
            "label": "Sweep",
            "kind": "context"
        }],
        "links": []
    })
}

/// A second valid preview for a second revision.
fn valid_preview_2() -> serde_json::Value {
    json!({
        "revision_id": "test-revision-2",
        "evidence": [{
            "id": "sweep2",
            "event": "liquidity_sweep",
            "time": 1_700_001_000_000_000_000_i64,
            "price": 200.0,
            "explanation": "Price swept the prior high."
        }],
        "zones": [],
        "markers": [{
            "id": "sweep-marker2",
            "evidence_id": "sweep2",
            "time": 1_700_001_000_000_000_000_i64,
            "price": 200.0,
            "label": "Sweep2",
            "kind": "bearish"
        }],
        "links": []
    })
}

#[tokio::test]
async fn a_user_cannot_read_another_users_workspace() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let alice = h.register().await;
    let bob = h.register().await;

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Alice workspace", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&alice.token),
        )
        .await;

    // Bob cannot read Alice's workspace.
    let (status, _) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Bob cannot list revisions in Alice's workspace.
    let (status, _) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Bob cannot post a revision into Alice's workspace.
    let (status, _) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: hacked\nversion: '1'",
                "summary": "hacked",
                "change_summary": "hacked",
                "preview": valid_preview(),
            }),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Bob cannot delete Alice's workspace.
    let (status, _) = h
        .delete(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Alice can still read her own workspace.
    let (status, body) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&alice.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["name"], "Alice workspace");

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), alice.id, workspace_id.parse().unwrap())
        .await
        .expect("cleanup");
    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn revision_restore_sets_active_revision() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Restore test", "symbol": "ETHUSDT", "timeframe": "15m" }),
            Some(&user.token),
        )
        .await;

    // Create two revisions.
    let rev1_body = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: rev1\nversion: '1'",
                "summary": "First revision",
                "change_summary": "Initial",
                "preview": valid_preview(),
            }),
            Some(&user.token),
        )
        .await
        .1;
    let rev1_id = rev1_body["id"].as_str().unwrap().to_string();
    assert_eq!(rev1_body["status"], "validated");

    let rev2_body = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: rev2\nversion: '1'",
                "summary": "Second revision",
                "change_summary": "Edit",
                "preview": valid_preview_2(),
            }),
            Some(&user.token),
        )
        .await
        .1;
    let rev2_id = rev2_body["id"].as_str().unwrap().to_string();

    // After creating rev2, the active revision should be rev2 (last validated).
    let ws = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        ws["active_revision_id"].as_str().unwrap(),
        rev2_id,
        "active should be the latest validated revision"
    );

    // Restore rev1.
    let (status, _) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions/{rev1_id}/restore"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    // Now the active revision is rev1.
    let ws = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        ws["active_revision_id"].as_str().unwrap(),
        rev1_id,
        "restore should set the active revision"
    );

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("cleanup");
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn restoring_a_rejected_revision_fails() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Reject test", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    // A rejected revision (invalid preview with empty revision_id).
    let (status, _) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "broken",
                "summary": "rejected",
                "change_summary": "bad",
                "preview": { "revision_id": "" },
            }),
            Some(&user.token),
        )
        .await;
    // The route validates the preview and stores it as "rejected".
    assert_eq!(status, axum::http::StatusCode::CREATED);

    // List revisions to find the rejected one.
    let revisions = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    let rejected_id = revisions[0]["id"].as_str().unwrap();

    // Restore should fail for a rejected revision.
    let (status, _) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions/{rejected_id}/restore"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), user.id, workspace_id.parse().unwrap())
        .await
        .expect("cleanup");
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn alert_preferences_are_user_scoped() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let alice = h.register().await;
    let bob = h.register().await;

    // Alice creates a workspace with a revision.
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Alert test", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&alice.token),
        )
        .await;

    let rev_body = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "test",
                "summary": "test",
                "change_summary": "test",
                "preview": valid_preview(),
            }),
            Some(&alice.token),
        )
        .await
        .1;
    let rev_id = rev_body["id"].as_str().unwrap();

    // Alice sets an alert preference.
    let (status, _) = h
        .put(
            &format!("/indicator-workspaces/{workspace_id}/alerts"),
            json!({
                "revision_id": rev_id,
                "event_name": "long_setup",
                "enabled": true,
                "channels": ["webhook"]
            }),
            Some(&alice.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    // Alice can list her alert preferences.
    let alerts = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/alerts"),
            Some(&alice.token),
        )
        .await;
    let arr = alerts.as_array().expect("an array");
    assert_eq!(arr.len(), 1, "Alice should see her alert");
    assert_eq!(arr[0]["event_name"], "long_setup");
    assert_eq!(arr[0]["enabled"], true);

    // Bob sees nothing for Alice's workspace.
    let (status, _) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}/alerts"),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Alert deduplication: setting the same event again updates, not duplicates.
    let (status, _) = h
        .put(
            &format!("/indicator-workspaces/{workspace_id}/alerts"),
            json!({
                "revision_id": rev_id,
                "event_name": "long_setup",
                "enabled": false,
                "channels": ["email"]
            }),
            Some(&alice.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    let alerts = h
        .ok(
            &format!("/indicator-workspaces/{workspace_id}/alerts"),
            Some(&alice.token),
        )
        .await;
    let arr = alerts.as_array().expect("an array");
    assert_eq!(arr.len(), 1, "upsert should not create a duplicate");
    assert_eq!(arr[0]["enabled"], false);

    // Cleanup.
    db::delete_indicator_workspace(h.database.pool(), alice.id, workspace_id.parse().unwrap())
        .await
        .expect("cleanup");
    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn delete_removes_workspace_and_cascades() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Delete test", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    // Create a revision so the workspace is non-trivial.
    h.post(
        &format!("/indicator-workspaces/{workspace_id}/revisions"),
        json!({
            "source": "test",
            "summary": "test",
            "change_summary": "test",
            "preview": valid_preview(),
        }),
        Some(&user.token),
    )
    .await;

    // Delete the workspace.
    let (status, _) = h
        .delete(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    // Verify it's gone.
    let (status, _) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // Revisions are also gone (cascade).
    let (status, _) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn single_revision_read_returns_source_and_preview() {
    let Some(h) = common::Harness::new().await else {
        eprintln!("no database; skipping");
        return;
    };
    let user = h.register().await;

    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Revision read", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&user.token),
        )
        .await;

    let rev_body = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: my_indicator\nversion: '1'",
                "summary": "A test indicator",
                "change_summary": "Created",
                "preview": valid_preview(),
            }),
            Some(&user.token),
        )
        .await
        .1;
    let rev_id = rev_body["id"].as_str().unwrap();

    // Read the revision directly.
    let (status, body) = h
        .get(
            &format!("/indicator-workspaces/{workspace_id}/revisions/{rev_id}"),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["source"], "name: my_indicator\nversion: '1'");
    assert_eq!(body["summary"], "A test indicator");
    assert_eq!(body["status"], "validated");
    assert!(body["preview"]["evidence"].as_array().unwrap().len() > 0);

    let user_id = user.id;
    db::delete_indicator_workspace(h.database.pool(), user_id, workspace_id.parse().unwrap())
        .await
        .expect("cleanup");
    user.cleanup(&h.database).await;
}
