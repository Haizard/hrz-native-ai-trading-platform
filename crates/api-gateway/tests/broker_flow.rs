//! `/brokers`, driven through the real router.
//!
//! ## What this proves, and what it deliberately does not
//!
//! That a user can connect an exchange account, that the key is *sealed* rather
//! than stored, that the connection is checked against the venue before it is
//! trusted, and that only its owner can read, re-check or remove it. The middle
//! claim is the one worth stating: the harness points the live venue client at
//! `http://127.0.0.1:1`, so every check here fails at the transport -- and a
//! credential check that could not reach the exchange must **not** come back
//! `verified`. A test that pointed this at the real Binance would be a test that
//! needs network, a key, and someone else's rate limit.
//!
//! The corresponding positive path -- a venue that answers with a trading-only
//! key, and the signed request that asks it -- is
//! `trading_engine::binance::tests::verify_credentials_signs_the_request_and_parses_the_answer`,
//! against a mock venue standing up `/api/v3/account`. Neither test alone covers
//! the contract; together they do.

mod common;

use axum::http::StatusCode;
use common::{Harness, SIMPLE_STRATEGY};
use serde_json::json;

/// A key/secret pair shaped like a real one, and not one.
///
/// Named so that a reader grepping the repository for a credential finds
/// obviously-fake text, and so that an assertion can prove the value never comes
/// back out.
const KEY: &str = "broker-flow-test-key";
const SECRET: &str = "broker-flow-test-secret";

/// `POST /brokers` for this user, and the body.
async fn connect(
    h: &Harness,
    token: &str,
    label: &str,
    venue: &str,
) -> (StatusCode, serde_json::Value) {
    h.post(
        "/brokers",
        json!({
            "venue": venue,
            "label": label,
            "api_key": KEY,
            "api_secret": SECRET,
        }),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn the_picker_lists_brokers_the_platform_can_connect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let body = h.ok("/brokers/available", Some(&user.token)).await;
    let venues = body.as_array().expect("an array");

    assert_eq!(venues.len(), 1, "one broker is wired up today: {body}");
    assert_eq!(venues[0]["venue"], "binance");
    assert_eq!(venues[0]["name"], "Binance");
    assert!(
        venues[0]["keys_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://")),
        "a picker entry the user cannot follow to create a key is not usable: {body}"
    );
    assert!(
        venues[0]["guidance"]
            .as_str()
            .is_some_and(|text| text.contains("Withdrawals")),
        "the guidance has to name the permission users must not grant: {body}"
    );

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_new_user_has_no_broker_accounts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let body = h.ok("/brokers", Some(&user.token)).await;
    assert_eq!(body, json!([]), "nothing connected yet: {body}");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_unknown_broker_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = connect(&h, &user.token, "somewhere else", "kraken").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "VENUE_UNKNOWN");

    // And nothing was written on the way to the refusal.
    let listed = h.ok("/brokers", Some(&user.token)).await;
    assert_eq!(listed, json!([]), "{listed}");

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_empty_credential_is_refused_before_anything_is_stored() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/brokers",
            json!({ "venue": "binance", "label": "main", "api_key": "", "api_secret": "" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BROKER_CREDENTIALS_INVALID");

    // Both problems at once, so the user fixes the form in one pass.
    let problems = body["error"]["details"]["problems"]
        .as_array()
        .expect("the problems array");
    assert_eq!(problems.len(), 2, "{body}");

    let listed = h.ok("/brokers", Some(&user.token)).await;
    assert_eq!(
        listed,
        json!([]),
        "a rejected request must store nothing: {listed}"
    );

    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_connect_that_cannot_reach_the_venue_is_stored_and_marked_invalid() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = connect(&h, &user.token, "main", "binance").await;

    // 422 rather than 201: the account was stored and *diagnosed*, and saying
    // "created" would claim a working account.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BROKER_CREDENTIALS_REFUSED");

    // The refusal carries the row it wrote, so a UI can render the state it is
    // now in without a second request.
    let account = &body["error"]["details"]["broker_account"];
    assert_eq!(account["status"], "invalid");
    assert_eq!(account["may_trade"], false);
    assert_eq!(account["venue"], "binance");
    assert_eq!(account["label"], "main");
    assert!(
        account["last_error"].is_string(),
        "the diagnosis has to say why, or the user cannot act: {account}"
    );

    // The whole point of the module: a key that went in does not come out.
    let rendered = body.to_string();
    for secret in [KEY, SECRET] {
        assert!(
            !rendered.contains(secret),
            "the response echoed a credential: {rendered}"
        );
    }

    // The row survives the refusal, so the user can re-check it rather than
    // re-enter a key.
    let listed = h.ok("/brokers", Some(&user.token)).await;
    let accounts = listed.as_array().expect("an array");
    assert_eq!(accounts.len(), 1, "{listed}");
    assert_eq!(accounts[0]["status"], "invalid");
    assert_eq!(accounts[0]["may_trade"], false);

    let id = accounts[0]["id"].as_str().expect("an id").to_string();

    // And re-checking re-checks: it does not silently become valid because
    // somebody asked twice.
    let (status, body) = h
        .post(
            &format!("/brokers/{id}/verify"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BROKER_CREDENTIALS_REFUSED");

    // Cleanup has to remove the account first: `users` is referenced by
    // `broker_accounts` with no cascade, on purpose -- a key store should not
    // vanish because a row above it did.
    let (status, body) = h.delete(&format!("/brokers/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn the_same_label_twice_on_one_venue_is_a_conflict() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, _) = connect(&h, &user.token, "main", "binance").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The second one collides on the label before it ever reaches the venue --
    // even spelled with different case, because "My desk" and "my desk" are the
    // same desk.
    let (status, body) = connect(&h, &user.token, "MAIN", "binance").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "BROKER_LABEL_TAKEN");

    let listed = h.ok("/brokers", Some(&user.token)).await;
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");

    let id = listed[0]["id"].as_str().expect("an id").to_string();
    h.delete(&format!("/brokers/{id}"), Some(&user.token)).await;
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn another_user_cannot_read_verify_or_disconnect_an_account() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = h.register().await;
    let other = h.register().await;

    let (_, body) = connect(&h, &owner.token, "main", "binance").await;
    let id = body["error"]["details"]["broker_account"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    // Absent, never forbidden. A 403 would confirm the id exists, which is a
    // fact a caller has no business learning by guessing.
    let (status, body) = h.get(&format!("/brokers/{id}"), Some(&other.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = h
        .post(
            &format!("/brokers/{id}/verify"),
            json!({}),
            Some(&other.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = h
        .delete(&format!("/brokers/{id}"), Some(&other.token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // And it is still the owner's. A delete that answered 404 while removing the
    // row would pass the three assertions above and be the worst possible bug.
    let listed = h.ok("/brokers", Some(&owner.token)).await;
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");

    h.delete(&format!("/brokers/{id}"), Some(&owner.token))
        .await;
    owner.cleanup(&h.database).await;
    other.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_live_bot_names_its_account_and_the_refusal_says_which_setup_is_missing() {
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

    // A live bot with no account named. The track record and the opt-in are
    // missing too, and all three must be in the one refusal -- the whole reason
    // the account check was moved ahead of the gate.
    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id, "mode": "live", "venue": "binance" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "LIVE_GATE_REFUSED");

    let message = body["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("broker account"),
        "the missing account must be named alongside the gate: {message}"
    );
    assert!(
        message.contains("opted in"),
        "and so must the opt-in: {message}"
    );
    assert!(
        message.contains("paper trades"),
        "and so must the track record: {message}"
    );

    // An id that is not a uuid is the caller's mistake, not a policy refusal, so
    // it stays a 422 rather than joining the gate message.
    let (status, body) = h
        .post(
            "/bots",
            json!({
                "strategy_id": strategy_id,
                "mode": "live",
                "venue": "binance",
                "broker_account_id": "not-a-uuid",
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BROKER_ACCOUNT_INVALID");

    // And an account that is not the caller's is a 404, not a gate sentence.
    let stranger = h.register().await;
    let (_, body) = connect(&h, &stranger.token, "theirs", "binance").await;
    let foreign = body["error"]["details"]["broker_account"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let (status, body) = h
        .post(
            "/bots",
            json!({
                "strategy_id": strategy_id,
                "mode": "live",
                "venue": "binance",
                "broker_account_id": foreign,
            }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    h.delete(&format!("/brokers/{foreign}"), Some(&stranger.token))
        .await;
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    stranger.cleanup(&h.database).await;
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn disconnecting_removes_the_account_and_reports_what_it_stopped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (_, body) = connect(&h, &user.token, "main", "binance").await;
    let id = body["error"]["details"]["broker_account"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let (status, body) = h.delete(&format!("/brokers/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], id);
    // Empty is a normal answer, not a failure: nothing was running on it.
    assert_eq!(body["bots_killed"], json!([]));

    let listed = h.ok("/brokers", Some(&user.token)).await;
    assert_eq!(listed, json!([]), "{listed}");

    // A second disconnect is a 404 rather than a 500 or a silent success.
    let (status, _) = h.delete(&format!("/brokers/{id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    user.cleanup(&h.database).await;
}
