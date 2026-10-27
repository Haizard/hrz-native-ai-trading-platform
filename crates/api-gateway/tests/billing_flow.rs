//! `/billing/*` and the plan enforcement behind it, driven through the router.
//!
//! ## What this proves
//!
//! That the catalog is readable signed out, that a new account starts on Free,
//! that a paid plan starts its one trial and replays idempotently, that cancel
//! and resume move the renewal flag rather than the plan, and -- the part the
//! plan matrix exists for -- that the Free rungs are actually refused at the
//! route boundary: order flow, a fifth pane, a sixth backtest.
//!
//! The harness builds the gateway in **internal** billing mode, which is the
//! deployment's own mode: a subscription is granted directly and there is no
//! provider to charge. The provider seam's signature and replay properties are
//! unit-tested in `billing::provider`, where they do not need a database.

mod common;

use axum::http::StatusCode;
use common::{Harness, SIMPLE_STRATEGY, WINDOW_FROM, WINDOW_TO};
use serde_json::json;

fn error_code(body: &serde_json::Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn the_catalog_is_public_and_a_new_account_starts_on_free() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, catalog) = h.get("/billing/plans", None).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let plans = catalog["plans"].as_array().expect("a plan list");
    assert_eq!(plans.len(), 5, "five rungs: {catalog}");
    assert_eq!(plans[0]["id"], "free");
    assert_eq!(catalog["trial_days"], 14);
    assert_eq!(catalog["billing_available"], true);
    assert_eq!(catalog["mode"], "internal");
    assert_eq!(catalog["credit_costs"]["chart_analysis"], 3);
    assert_eq!(catalog["community_indicators_available"], true);
    assert_eq!(plans[0]["features"]["community_indicators"], "included");
    assert_eq!(plans[1]["features"]["community_indicators"], "included");
    assert_eq!(plans[4]["features"]["community_indicators"], "priority");

    let user = h.register().await;
    let entitlements = h.ok("/billing/subscription", Some(&user.token)).await;
    assert_eq!(entitlements["plan"], "free");
    assert_eq!(entitlements["subscription"]["status"], "free");
    assert_eq!(entitlements["limits"]["max_ai_credits"], 25);
    assert_eq!(entitlements["limits"]["max_backtests"], 5);
    assert_eq!(entitlements["usage"]["ai_credits"]["used"], 0);
    assert_eq!(entitlements["usage"]["ai_credits"]["limit"], 25);
    assert_eq!(entitlements["usage"]["backtests"]["remaining"], 5);
    assert_eq!(entitlements["community_indicators_available"], true);

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_paid_plan_starts_one_trial_and_a_replayed_key_does_not_extend_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let subscribe =
        json!({ "plan": "core", "cycle": "monthly", "idempotency_key": "billing-flow-1" });
    let (status, first) = h
        .post("/billing/subscribe", subscribe.clone(), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["plan"], "core");
    assert_eq!(first["subscription"]["status"], "trialing");
    assert_eq!(first["subscription"]["trial_used"], true);
    assert!(
        first["subscription"]["trial_end"].is_string(),
        "a trial has an end date: {first}"
    );
    assert_eq!(first["limits"]["max_ai_credits"], 250);
    let period_end = first["subscription"]["period_end"].clone();

    // Same key: the retry returns the same period. A second period would be a
    // second charge in a deployment that had a provider.
    let (status, replay) = h
        .post("/billing/subscribe", subscribe, Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(
        replay["subscription"]["period_end"], period_end,
        "an idempotent replay must not start a new period"
    );

    // Cancel keeps the plan through the period; resume clears the flag.
    let (status, canceled) = h
        .post("/billing/cancel", json!({}), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{canceled}");
    assert_eq!(canceled["plan"], "core");
    assert_eq!(canceled["subscription"]["cancel_at_period_end"], true);

    let (status, resumed) = h
        .post("/billing/resume", json!({}), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert_eq!(resumed["subscription"]["cancel_at_period_end"], false);
    assert_eq!(resumed["plan"], "core");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn free_is_not_a_subscription_and_cancel_without_one_is_a_conflict() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "free" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "BILLING_REQUEST_INVALID");

    let (status, body) = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "platinum" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "UNKNOWN_PLAN");

    let (status, body) = h
        .post("/billing/cancel", json!({}), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "SUBSCRIPTION_REQUIRED");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn order_flow_climbs_from_free_to_core_to_pro() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let basic = "/delta-by-size?symbol=BTCUSDT&timeframe=5m&limit=10";
    let advanced = "/vpin?symbol=BTCUSDT&timeframe=5m&limit=10";

    let (status, body) = h.get(basic, Some(&user.token)).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(error_code(&body), "PLAN_FEATURE_REQUIRED");
    assert_eq!(body["error"]["details"]["required_plan"], "core");
    assert_eq!(body["error"]["details"]["plan"], "free");

    let _ = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "core" }),
            Some(&user.token),
        )
        .await;
    let (status, body) = h.get(basic, Some(&user.token)).await;
    assert_ne!(
        status,
        StatusCode::PAYMENT_REQUIRED,
        "Core carries live order flow: {body}"
    );

    // Advanced reads are the next rung, not the same one.
    let (status, body) = h.get(advanced, Some(&user.token)).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["details"]["required_plan"], "pro");

    let _ = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "pro" }),
            Some(&user.token),
        )
        .await;
    let (status, body) = h.get(advanced, Some(&user.token)).await;
    assert_ne!(
        status,
        StatusCode::PAYMENT_REQUIRED,
        "Pro carries the advanced reads: {body}"
    );

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn the_free_plan_caps_a_layout_at_four_panes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let panels: Vec<_> = (0..5)
        .map(|_| json!({ "symbol": "BTCUSDT", "timeframe": "5m" }))
        .collect();

    let (status, body) = h
        .post(
            "/chart-sessions",
            json!({ "name": "five panes", "panels": panels }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(error_code(&body), "PLAN_LIMIT_REACHED");
    assert_eq!(body["error"]["details"]["resource"], "panes");
    assert_eq!(body["error"]["details"]["limit"], 4);

    let _ = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "core" }),
            Some(&user.token),
        )
        .await;
    let (status, body) = h
        .post(
            "/chart-sessions",
            json!({
                "name": "five panes",
                "panels": (0..5)
                    .map(|_| json!({ "symbol": "BTCUSDT", "timeframe": "5m" }))
                    .collect::<Vec<_>>(),
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "Core raises the pane cap to 16: {body}"
    );
    db::chart_sessions::delete_chart_session(
        h.database.pool(),
        user.id,
        body["id"].as_str().expect("a session id").parse().unwrap(),
    )
    .await
    .unwrap();

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn the_free_plan_spends_five_backtests_and_refuses_the_sixth() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = h
        .created(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    let run = json!({ "symbol": "BTCUSDT", "from": WINDOW_FROM, "to": WINDOW_TO });

    for run_number in 1..=5 {
        let (status, body) = h
            .post(
                &format!("/strategies/{strategy_id}/backtest"),
                run.clone(),
                Some(&user.token),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "backtest {run_number} of the free allowance: {body}"
        );
    }

    let (status, body) = h
        .post(
            &format!("/strategies/{strategy_id}/backtest"),
            run,
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(error_code(&body), "BACKTEST_QUOTA_EXCEEDED");
    assert_eq!(body["error"]["details"]["limit"], 5);

    let entitlements = h.ok("/billing/subscription", Some(&user.token)).await;
    assert_eq!(entitlements["usage"]["backtests"]["used"], 5);
    assert_eq!(entitlements["usage"]["backtests"]["remaining"], 0);

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn history_past_the_plan_is_clamped_and_unlimited_on_elite() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let to_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_millis();
    let path = format!("/candles?symbol=BTCUSDT&timeframe=5m&from=0&to={to_ms}");

    let (status, body) = h.get(&path, Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["history_days"], 31);
    assert_eq!(
        body["history_clamped"], true,
        "a window from 1970 is past every plan's depth: {body}"
    );

    let _ = h
        .post(
            "/billing/subscribe",
            json!({ "plan": "elite" }),
            Some(&user.token),
        )
        .await;
    let (status, body) = h.get(&path, Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["history_days"], -1);
    assert_eq!(body["history_clamped"], false);

    user.cleanup(&h.database).await;
}
