//! Workspace persistence (`docs/54`): one opaque, owner-scoped snapshot.

mod common;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn a_workspace_round_trips_and_is_owner_scoped() {
    let Some(h) = common::Harness::new().await else { return };
    let alice = h.register().await;
    let bob = h.register().await;

    let empty = h.ok("/workspace", Some(&alice.token)).await;
    assert!(empty["state"].is_null());

    let state = json!({
        "version": 1,
        "layout": { "mode": "grid", "columns": 3 },
        "activePane": 1,
        "activeTab": "community",
        "indicatorWorkspaceId": null,
        "panes": [
            { "symbol": "BTCUSDT", "timeframe": "5m", "mode": "candles", "limit": "500", "indicator": null, "scripts": [] },
            { "symbol": "ETHUSDT", "timeframe": "15m", "mode": "line", "limit": "1000", "indicator": null, "scripts": [] }
        ]
    });
    let (status, body) = h.put("/workspace", json!({ "state": state }), Some(&alice.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saved"], true);

    let read = h.ok("/workspace", Some(&alice.token)).await;
    assert_eq!(read["state"], state);

    let other = h.ok("/workspace", Some(&bob.token)).await;
    assert!(other["state"].is_null(), "another user must not see Alice's state");

    alice.cleanup(&h.database).await;
    bob.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_invalid_state_is_refused() {
    let Some(h) = common::Harness::new().await else { return };
    let user = h.register().await;

    let too_many_panes = json!({ "version": 1, "panes": vec![json!({}); 101] });
    for bad in [
        json!({ "state": "not an object" }),
        json!({ "state": { "version": 2, "panes": [] } }),
        json!({ "state": { "version": 1, "panes": [] } }),
        json!({ "state": too_many_panes }),
    ] {
        let (status, body) = h.put("/workspace", bad, Some(&user.token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "WORKSPACE_STATE_INVALID");
    }

    user.cleanup(&h.database).await;
}
