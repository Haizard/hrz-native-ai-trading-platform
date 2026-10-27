//! Community indicators (`docs/53`) against the real router and database.

mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};

fn preview(name: &str) -> Value {
    json!({
        "revision_id": name,
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

/// A workspace with one validated revision, owned by `token`.
async fn workspace_with_revision(h: &common::Harness, token: &str) -> (String, String) {
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Community source", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(token),
        )
        .await;
    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: community\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m",
                "summary": "A sweep followed by displacement.",
                "change_summary": "Initial",
                "preview": preview("community-rev"),
            }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "revision create failed: {body}");
    (workspace_id, body["id"].as_str().unwrap().to_string())
}

async fn publish(h: &common::Harness, token: &str, title: &str) -> Value {
    let (workspace_id, revision_id) = workspace_with_revision(h, token).await;
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({
                "workspace_id": workspace_id,
                "revision_id": revision_id,
                "title": title,
                "summary": "Sweep of a prior low, then displacement.",
                "tags": ["smc", "fvg"],
                "author_name": "Test Author",
            }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "publish failed: {body}");
    body
}

#[tokio::test]
async fn free_cannot_browse_and_core_can() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let free = h.register().await;
    let (status, body) = h.get("/community/indicators", Some(&free.token)).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_browse");

    let core = h.register_with_plan("core").await;
    let (status, body) = h.get("/community/indicators", Some(&core.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The catalog is global, so the assertion is about the shape and the
    // gate, not about a count some earlier test run could have left behind.
    assert!(body["items"].is_array());
    assert!(body["total"].as_i64().unwrap() >= 0);

    free.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn core_cannot_publish_and_elite_can() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let core = h.register_with_plan("core").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &core.token).await;
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Nope" }),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_publish");

    let elite = h.register_with_plan("elite").await;
    let item = publish(&h, &elite.token, "Liquidity sweep + FVG").await;
    assert_eq!(item["author_tier"], "publish");
    assert_eq!(
        item["source"],
        "name: community\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m"
    );

    core.cleanup(&h.database).await;
    elite.cleanup(&h.database).await;
}

#[tokio::test]
async fn publishing_someone_elses_workspace_is_404() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let alice = h.register_with_plan("elite").await;
    let bob = h.register_with_plan("elite").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &alice.token).await;
    let (status, _) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Stolen" }),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_rejected_revision_cannot_be_published() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let elite = h.register_with_plan("elite").await;
    let workspace_id = h
        .created(
            "/indicator-workspaces",
            json!({ "name": "Reject", "symbol": "BTCUSDT", "timeframe": "5m" }),
            Some(&elite.token),
        )
        .await;
    let (status, body) = h
        .post(
            &format!("/indicator-workspaces/{workspace_id}/revisions"),
            json!({
                "source": "name: bad\nversion: '1'\nkind: indicator\nmarket: BTCUSDT\ntimeframes:\n  entry: 5m",
                "summary": "Bad",
                "change_summary": "Bad",
                "preview": { "revision_id": "", "evidence": [], "zones": [], "markers": [], "links": [] },
            }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "rejected");
    let revision_id = body["id"].as_str().unwrap();

    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Bad item" }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_REVISION_NOT_VALIDATED");

    elite.cleanup(&h.database).await;
}

#[tokio::test]
async fn install_forks_once_and_update_moves_the_pin() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Sweep fork").await;
    let slug = item["slug"].as_str().unwrap();

    let (status, body) = h
        .post(
            &format!("/community/indicators/{slug}/install"),
            json!({}),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["already_installed"], false);
    let workspace_id = body["workspace_id"].as_str().unwrap().to_string();

    let (status, again) = h
        .post(
            &format!("/community/indicators/{slug}/install"),
            json!({}),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_installed"], true);
    assert_eq!(again["workspace_id"], workspace_id);

    let installed = h
        .ok("/community/indicators/installed", Some(&core.token))
        .await;
    assert_eq!(installed["items"].as_array().unwrap().len(), 1);

    let detail = h
        .ok(&format!("/community/indicators/{slug}"), Some(&core.token))
        .await;
    assert_eq!(detail["installed"]["workspace_id"], workspace_id);
    assert_eq!(detail["update_available"], false);

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_new_version_shows_update_available_and_update_appends() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Versioned").await;
    let slug = item["slug"].as_str().unwrap().to_string();
    let (_, installed) = h
        .post(
            &format!("/community/indicators/{slug}/install"),
            json!({}),
            Some(&core.token),
        )
        .await;

    let (workspace_id, _) = workspace_with_revision(&h, &author.token).await;
    let (status, second) = h
        .post(
            &format!("/community/indicators/{slug}/versions"),
            json!({
                "workspace_id": workspace_id,
                "summary": "Version two.",
            }),
            Some(&author.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");

    let detail = h
        .ok(&format!("/community/indicators/{slug}"), Some(&core.token))
        .await;
    assert_eq!(detail["update_available"], true);

    let (status, updated) = h
        .post(
            &format!("/community/indicators/{slug}/update"),
            json!({}),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let revision_id = updated["revision_id"].as_str().unwrap().to_string();

    let revisions = h
        .ok(
            &format!(
                "/indicator-workspaces/{}/revisions",
                installed["workspace_id"].as_str().unwrap()
            ),
            Some(&core.token),
        )
        .await;
    let numbers: Vec<i64> = revisions
        .as_array()
        .unwrap()
        .iter()
        .map(|revision| revision["revision_number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, vec![2, 1]);
    assert_eq!(revisions[0]["id"], revision_id);

    let (status, body) = h
        .post(
            &format!("/community/indicators/{slug}/update"),
            json!({}),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_ALREADY_CURRENT");

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn unlisting_hides_the_item_but_keeps_installed_copies() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Unlisted").await;
    let slug = item["slug"].as_str().unwrap();
    h.post(
        &format!("/community/indicators/{slug}/install"),
        json!({}),
        Some(&core.token),
    )
    .await;

    let (status, body) = h
        .patch(
            &format!("/community/indicators/{slug}"),
            json!({ "status": "unlisted" }),
            Some(&author.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = h
        .get(&format!("/community/indicators/{slug}"), Some(&core.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let installed = h
        .ok("/community/indicators/installed", Some(&core.token))
        .await;
    assert_eq!(installed["items"].as_array().unwrap().len(), 1);

    let (status, body) = h
        .post(
            &format!("/community/indicators/{slug}/update"),
            json!({}),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_NOT_LISTED");

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn reports_are_idempotent() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let core = h.register_with_plan("core").await;
    let item = publish(&h, &author.token, "Reported").await;
    let slug = item["slug"].as_str().unwrap();

    let (status, first) = h
        .post(
            &format!("/community/indicators/{slug}/report"),
            json!({ "reason": "spam", "note": "looks wrong" }),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["reported"], true);
    assert_eq!(first["already_reported"], false);

    let (status, second) = h
        .post(
            &format!("/community/indicators/{slug}/report"),
            json!({ "reason": "spam" }),
            Some(&core.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["already_reported"], true);

    author.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn desk_priority_sorts_first() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let elite = h.register_with_plan("elite").await;
    let desk = h.register_with_plan("desk").await;
    publish(&h, &elite.token, "Ordinary item").await;
    let priority = publish(&h, &desk.token, "Priority item").await;
    assert_eq!(priority["author_tier"], "priority");

    let core = h.register_with_plan("core").await;
    let listed = h
        .ok("/community/indicators?sort=featured", Some(&core.token))
        .await;
    assert_eq!(listed["items"][0]["author_tier"], "priority");

    elite.cleanup(&h.database).await;
    desk.cleanup(&h.database).await;
    core.cleanup(&h.database).await;
}

#[tokio::test]
async fn free_cannot_install() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let free = h.register().await;
    let item = publish(&h, &author.token, "Paid only").await;
    let slug = item["slug"].as_str().unwrap();
    let (status, body) = h
        .post(
            &format!("/community/indicators/{slug}/install"),
            json!({}),
            Some(&free.token),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["feature"], "community_browse");

    author.cleanup(&h.database).await;
    free.cleanup(&h.database).await;
}

#[tokio::test]
async fn only_the_author_can_add_versions_or_patch_metadata() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let author = h.register_with_plan("elite").await;
    let other = h.register_with_plan("elite").await;
    let item = publish(&h, &author.token, "Mine").await;
    let slug = item["slug"].as_str().unwrap();
    // The other user's own workspace must not let them add a version to the
    // author's item: ownership of the item is checked first.
    let (other_workspace, _) = workspace_with_revision(&h, &other.token).await;

    let (status, _) = h
        .post(
            &format!("/community/indicators/{slug}/versions"),
            json!({ "workspace_id": other_workspace }),
            Some(&other.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = h
        .patch(
            &format!("/community/indicators/{slug}"),
            json!({ "title": "Stolen" }),
            Some(&other.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    author.cleanup(&h.database).await;
    other.cleanup(&h.database).await;
}

#[tokio::test]
async fn metadata_and_source_bounds_are_named() {
    let Some(h) = common::Harness::new().await else {
        return;
    };
    let elite = h.register_with_plan("elite").await;
    let (workspace_id, revision_id) = workspace_with_revision(&h, &elite.token).await;

    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "x" }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_METADATA_INVALID");

    let tags: Vec<String> = (0..6).map(|n| format!("tag{n}")).collect();
    let (status, body) = h
        .post(
            "/community/indicators",
            json!({ "workspace_id": workspace_id, "revision_id": revision_id, "title": "Valid title", "tags": tags }),
            Some(&elite.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNITY_METADATA_INVALID");

    elite.cleanup(&h.database).await;
}
