//! `/strategies` and `/backtests`, driven through the real router.
//!
//! ## What only a route test can show
//!
//! That a document which validates is actually stored; that a document which
//! does not is refused *with the validator's field paths*; that a strategy
//! belonging to somebody else is a 404 rather than a 403; and that a backtest
//! runs against real candles and comes back with numbers in it. None of that is
//! a property of any one function.
//!
//! Every test registers its own account and deletes it, and deletes any
//! strategy it stored, so the database is left as it was found.

mod common;

use axum::http::StatusCode;
use common::{Harness, INVALID_STRATEGY, SIMPLE_STRATEGY, WINDOW_FROM, WINDOW_TO};
use serde_json::json;

#[tokio::test]
async fn a_valid_document_is_stored_and_readable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "Route test strategy");
    assert_eq!(body["created_by"], "developer_sdk");
    let id = body["id"].as_str().expect("an id").to_string();

    let (status, body) = h.get(&format!("/strategies/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Route test strategy");
    assert_eq!(body["document"]["timeframes"]["entry"], "5m");

    let (status, body) = h.get("/strategies", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.as_array()
            .expect("a list")
            .iter()
            .any(|s| s["id"] == id),
        "the stored strategy must appear in the list: {body}"
    );

    db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_invalid_document_is_refused_with_field_paths() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": INVALID_STRATEGY }),
            Some(&user.token),
        )
        .await;

    // docs/12: the frontend editor and the agent's retry loop both need the
    // per-field detail, not a flattened sentence.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "STRATEGY_VALIDATION_FAILED");
    let issues = body["error"]["details"]["issues"]
        .as_array()
        .expect("the issues array must be present");
    assert!(!issues.is_empty());
    assert!(
        issues
            .iter()
            .all(|i| i["path"].is_string() && i["message"].is_string()),
        "every issue needs a path and a message: {issues:?}"
    );

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn unparseable_text_is_a_different_code_from_a_validation_failure() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": "this: is: not: valid: yaml" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "STRATEGY_PARSE_FAILED");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn validate_checks_text_without_storing_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies/validate",
            json!({ "source": SIMPLE_STRATEGY }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["valid"], true);
    assert_eq!(body["name"], "Route test strategy");
    assert_eq!(body["timeframes"]["entry"], "5m");

    // Nothing was stored, so the list is still empty for this account.
    let (_, listed) = h.get("/strategies", Some(&user.token)).await;
    assert!(listed.as_array().expect("a list").is_empty());

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_stored_strategy_can_be_revalidated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let (_, created) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            &format!("/strategies/{id}/validate"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["valid"], true);

    db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn another_users_strategy_is_absent_not_forbidden() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = h.register().await;
    let stranger = h.register().await;

    let (_, created) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&owner.token),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_string();

    // 404, not 403: a 403 would confirm the id exists, which is a fact a caller
    // has no business learning by guessing.
    let (status, body) = h
        .get(&format!("/strategies/{id}"), Some(&stranger.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, _) = h
        .post(
            &format!("/strategies/{id}/validate"),
            json!({}),
            Some(&stranger.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = h
        .post(
            &format!("/strategies/{id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": WINDOW_FROM, "to": WINDOW_TO }),
            Some(&stranger.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
        .await
        .unwrap();
    owner.cleanup(&h.database).await;
    stranger.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_backtest_runs_against_real_candles_and_is_stored() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let (_, created) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    let strategy_id = created["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            &format!("/strategies/{strategy_id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": WINDOW_FROM, "to": WINDOW_TO }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let backtest_id = body["id"].as_str().expect("a backtest id").to_string();
    let report = &body["report"];
    assert_eq!(report["symbol"], "BTCUSDT");
    assert_eq!(report["strategy"], "Route test strategy");
    assert_eq!(report["decision_timeframe"], "entry");
    // Real numbers, not placeholders: the window has candles, so the engine
    // was actually asked something.
    assert!(
        report["total_trades"].as_u64().is_some(),
        "the report must carry a trade count: {report}"
    );
    assert!(report["win_rate"].as_f64().is_some());
    assert!(
        !report["trades"]
            .as_array()
            .expect("a trade list")
            .is_empty()
            || report["total_trades"] == 0
    );

    // And it can be read back by id.
    let (status, fetched) = h
        .get(&format!("/backtests/{backtest_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["report"]["total_trades"], report["total_trades"]);

    let (status, listed) = h
        .get(
            &format!("/strategies/{strategy_id}/backtests"),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().expect("a list").len(), 1);

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_backtest_over_an_empty_window_is_refused_not_empty() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let (_, created) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    let strategy_id = created["id"].as_str().unwrap().to_string();

    // A window with no candles at all. Returning a report of zero trades would
    // be a valid-looking answer to a question the data cannot answer.
    let (status, body) = h
        .post(
            &format!("/strategies/{strategy_id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": "2019-01-01", "to": "2019-01-05" }),
            Some(&user.token),
        )
        .await;
    // 422 and a code that says "no data", not a 500 that says "try again":
    // the query worked and the answer was that the window is empty.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "NO_MARKET_DATA");

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_bad_window_is_a_400_before_any_work_happens() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let (_, created) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    let strategy_id = created["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            &format!("/strategies/{strategy_id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": "2026-09-11", "to": "2026-09-10" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "WINDOW_INVALID");

    let (status, body) = h
        .post(
            &format!("/strategies/{strategy_id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": "not-a-date", "to": WINDOW_TO }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "DATE_INVALID");

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn the_strategy_routes_require_a_token() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // docs/12: everything that touches someone's strategies needs a token.
    let (status, body) = h.get("/strategies", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    let (status, _) = h
        .post("/strategies", json!({ "source": SIMPLE_STRATEGY }), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = h
        .get("/strategies/00000000-0000-0000-0000-000000000000", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Validation is public: it stores nothing and touches nobody's data, and
    // the editor needs it before the user has an account in some flows.
    let (status, _) = h
        .post(
            "/strategies/validate",
            json!({ "source": SIMPLE_STRATEGY }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_unknown_created_by_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY, "created_by": "a-vibes-based-editor" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "CREATED_BY_INVALID");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_agent_authored_strategy_records_who_wrote_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY, "created_by": "ai_agent" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["created_by"], "ai_agent");
    let id = body["id"].as_str().unwrap().to_string();

    db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

/// [`SIMPLE_STRATEGY`] with a bumped version, for the versioning tests.
fn with_version(source: &str, version: &str) -> String {
    source.replace("version: \"1\"", &format!("version: \"{version}\""))
}

#[tokio::test]
async fn an_edit_stores_a_new_version_and_names_what_it_replaced() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let first = body["id"].as_str().expect("an id").to_string();

    let (status, body) = h
        .put(
            &format!("/strategies/{first}"),
            json!({ "source": with_version(SIMPLE_STRATEGY, "2") }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let second = body["id"].as_str().expect("an id").to_string();

    // A new row, not an edit: the old document is what the old backtests ran.
    assert_ne!(first, second, "an edit must not overwrite the row: {body}");
    assert_eq!(body["version"], "2");
    assert_eq!(
        body["supersedes"], first,
        "the response has to say which row this replaced: {body}"
    );

    // Both are readable, and the new one is the one a listing leads with.
    let (status, body) = h
        .get(&format!("/strategies/{first}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "1");

    let (status, listed) = h.get("/strategies", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    let listed = listed.as_array().expect("a list");
    assert_eq!(listed[0]["id"], second, "newest first: {listed:?}");
    assert!(listed.iter().any(|s| s["id"] == first));

    for id in [&first, &second] {
        db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
            .await
            .unwrap();
    }
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_edit_inherits_who_wrote_the_previous_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY, "created_by": "ai_agent" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let first = body["id"].as_str().unwrap().to_string();

    // Editing a document does not change who wrote it.
    let (status, body) = h
        .put(
            &format!("/strategies/{first}"),
            json!({ "source": with_version(SIMPLE_STRATEGY, "2") }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["created_by"], "ai_agent");
    let second = body["id"].as_str().unwrap().to_string();

    for id in [&first, &second] {
        db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
            .await
            .unwrap();
    }
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_delete_takes_the_strategy_and_its_backtests() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            &format!("/strategies/{id}/backtest"),
            json!({ "symbol": "BTCUSDT", "from": WINDOW_FROM, "to": WINDOW_TO }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let backtest_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .delete(&format!("/strategies/{id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _) = h.get(&format!("/strategies/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A report whose document is gone is a set of numbers with no provenance,
    // so it goes too rather than being orphaned.
    let (status, _) = h
        .get(&format!("/backtests/{backtest_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_strategy_a_bot_is_running_cannot_be_deleted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let strategy_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let bot_id = body["id"].as_str().unwrap().to_string();

    // 409, not a 500 from the foreign key: the caller can act on this one.
    let (status, body) = h
        .delete(&format!("/strategies/{strategy_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "STRATEGY_IN_USE");

    // Delete the bot and the strategy becomes deletable.
    let (status, _) = h
        .delete(&format!("/bots/{bot_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = h
        .delete(&format!("/strategies/{strategy_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn somebody_elses_strategy_is_absent_rather_than_forbidden() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = h.register().await;
    let other = h.register().await;

    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(&owner.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    // 404, not 403: a 403 would confirm that the id exists.
    let (status, _) = h
        .put(
            &format!("/strategies/{id}"),
            json!({ "source": with_version(SIMPLE_STRATEGY, "2") }),
            Some(&other.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = h
        .delete(&format!("/strategies/{id}"), Some(&other.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    db::strategies::delete_strategy(h.database.pool(), id.parse().unwrap())
        .await
        .unwrap();
    owner.cleanup(&h.database).await;
    other.cleanup(&h.database).await;
}
