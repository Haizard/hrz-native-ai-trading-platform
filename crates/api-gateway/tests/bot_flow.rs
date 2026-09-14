//! `/bots`, driven through the real router with real candles.
//!
//! ## What this proves
//!
//! That `POST /bots` starts something that *runs*: candles go in through the
//! supervisor's feed, the bot decides on them, and the decisions reach Postgres.
//! A test that only checked the row would pass with no task behind it at all,
//! which is the failure mode this route can actually have.
//!
//! The candles are the real BTCUSDT 5m series from the database, published into
//! the bus exactly as the live collector publishes them. No socket, no mock --
//! `BotSupervisor::feed_candle` is the seam the collector itself uses.
//!
//! ## Why the harness flushes fast
//!
//! A running bot flushes every 30 seconds, so a test at that interval would
//! either wait or assert against decisions not yet written. The harness builds
//! the supervisor with a 100ms interval and polls until the rows appear.

mod common;

use std::time::Duration;

use analytics_core::types::{Candle, Timeframe};
use axum::http::StatusCode;
use common::{Harness, SIMPLE_STRATEGY, WINDOW_FROM};
use serde_json::json;

/// Candles from the database, so the bot is fed a real market.
async fn candles(h: &Harness, count: usize) -> Vec<Candle> {
    let from = 1_788_998_400_000_000_000i64; // 2026-09-10T00:00:00Z
    let to = from + 3 * 86_400 * 1_000_000_000;
    let mut series =
        db::repositories::load_candles(h.database.pool(), "BTCUSDT", Timeframe::M5, from, to)
            .await
            .expect("the candle query must succeed");
    series.truncate(count);
    series
}

/// Poll until the bot has written `expected` decisions, or give up.
///
/// The bot runs in its own task, so there is no moment at which "the candles
/// have been processed" is observable from outside. Polling is the honest way
/// to wait for an asynchronous effect, and the timeout is what makes a failure
/// a failed assertion rather than a hung test.
async fn wait_for_decisions(h: &Harness, user_id: uuid::Uuid, expected: i64) -> i64 {
    for _ in 0..100 {
        let count = db::paper::count_audit_events(h.database.pool(), user_id, "bot.decision")
            .await
            .unwrap_or(0);
        if count >= expected {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    db::paper::count_audit_events(h.database.pool(), user_id, "bot.decision")
        .await
        .unwrap_or(0)
}

/// Create a strategy and return its id.
async fn strategy(h: &Harness, token: &str) -> String {
    let (status, body) = h
        .post(
            "/strategies",
            json!({ "source": SIMPLE_STRATEGY }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_bot_starts_runs_on_candles_and_stops() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "running");
    assert_eq!(body["mode"], "paper");
    // The row and the task are two different things, and the response says
    // which one it got.
    assert_eq!(body["supervised_here"], true);
    let bot_id = body["id"].as_str().unwrap().to_string();

    // Feed it a real market.
    let series = candles(&h, 200).await;
    assert!(series.len() > 100, "expected candles to feed");
    for candle in &series {
        h.supervisor.feed_candle(candle);
    }

    let decisions = wait_for_decisions(&h, user.id, 1).await;
    assert!(
        decisions > 0,
        "the bot ran but wrote no decisions: nothing is consuming the feed"
    );

    // It is listed, and it knows what it has done.
    let (status, listed) = h.get("/bots", Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK);
    let listed = listed.as_array().expect("a list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], bot_id);
    assert!(
        listed[0]["activity"]["decisions"].as_i64().unwrap_or(0) > 0,
        "the activity counts must reflect what was written: {}",
        listed[0]
    );

    let (status, fetched) = h.get(&format!("/bots/{bot_id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["supervised_here"], true);

    // Stopping the task writes `bot.stopped`, which is how a crash is told from
    // a clean stop. Asserted *before* the DELETE below, because deleting a bot
    // removes its audit trail -- the event is a fact about the run, and the run
    // is about to stop existing.
    assert!(h.supervisor.stop(bot_id.parse().unwrap()).await);
    let stopped = db::paper::count_audit_events(h.database.pool(), user.id, "bot.stopped")
        .await
        .unwrap();
    assert_eq!(stopped, 1, "a clean stop must be recorded");

    // And DELETE removes the bot and everything it wrote.
    let (status, _) = h
        .delete(&format!("/bots/{bot_id}"), Some(&user.token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = h.get(&format!("/bots/{bot_id}"), Some(&user.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!h.supervisor.is_running(bot_id.parse().unwrap()));
    assert_eq!(
        db::paper::count_audit_events(h.database.pool(), user.id, "bot.decision")
            .await
            .unwrap(),
        0,
        "deleting a bot must take its audit trail with it"
    );

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_paused_bot_stops_deciding_and_resuming_restarts_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;
    let (_, created) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    let bot_id = created["id"].as_str().unwrap().to_string();

    let series = candles(&h, 120).await;
    for candle in &series[..60] {
        h.supervisor.feed_candle(candle);
    }
    let before_pause = wait_for_decisions(&h, user.id, 1).await;
    assert!(before_pause > 0);

    let (status, body) = h
        .post(
            &format!("/bots/{bot_id}/pause"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "paused");

    // Candles keep arriving; a paused bot drains them rather than
    // unsubscribing, so nothing more is written.
    for candle in &series[60..] {
        h.supervisor.feed_candle(candle);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let during_pause = db::paper::count_audit_events(h.database.pool(), user.id, "bot.decision")
        .await
        .unwrap();
    assert_eq!(
        during_pause, before_pause,
        "a paused bot must not decide on candles it receives"
    );

    let (status, body) = h
        .post(
            &format!("/bots/{bot_id}/resume"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "running");

    // Newer candles, because the bot's window ignores anything not newer than
    // what it already holds -- so feeding the same ones again would prove
    // nothing about resuming.
    for candle in &series[60..] {
        h.supervisor.feed_candle(candle);
    }
    let after_resume = wait_for_decisions(&h, user.id, during_pause + 1).await;
    assert!(
        after_resume > during_pause,
        "a resumed bot must decide again: {during_pause} -> {after_resume}"
    );

    // And a candle that is *not* newer is ignored, which is what makes a
    // restarted bot safe: it re-reads its last candle and must not re-run the
    // decision it already made on it.
    for candle in &series[..10] {
        h.supervisor.feed_candle(candle);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_rewind = db::paper::count_audit_events(h.database.pool(), user.id, "bot.decision")
        .await
        .unwrap();
    assert_eq!(
        after_rewind, after_resume,
        "a candle older than the newest one held must not produce a decision"
    );

    h.delete(&format!("/bots/{bot_id}"), Some(&user.token))
        .await;
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn pausing_a_bot_that_is_not_supervised_here_is_a_conflict() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;
    let (_, created) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    let bot_id = created["id"].as_str().unwrap().to_string();

    // Stop the task without touching the row, which is what a process restart
    // looks like from the database's point of view.
    assert!(h.supervisor.stop(bot_id.parse().unwrap()).await);

    let (status, body) = h
        .post(
            &format!("/bots/{bot_id}/pause"),
            json!({}),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "BOT_NOT_SUPERVISED_HERE");

    h.delete(&format!("/bots/{bot_id}"), Some(&user.token))
        .await;
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn live_mode_is_refused_with_the_phase_that_would_enable_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    // docs/11 gates live trading behind a paper track record, per-venue opt-in
    // and an exchange adapter. Quietly running a paper bot would be the worst
    // of both.
    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id, "mode": "live" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(body["error"]["code"], "LIVE_TRADING_NOT_ENABLED");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Phase 8"));

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn a_bot_cannot_be_started_from_someone_elses_strategy() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = h.register().await;
    let stranger = h.register().await;
    let strategy_id = strategy(&h, &owner.token).await;

    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&stranger.token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    owner.cleanup(&h.database).await;
    stranger.cleanup(&h.database).await;
}

#[tokio::test]
async fn another_users_bot_is_absent_not_forbidden() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = h.register().await;
    let stranger = h.register().await;
    let strategy_id = strategy(&h, &owner.token).await;
    let (_, created) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&owner.token),
        )
        .await;
    let bot_id = created["id"].as_str().unwrap().to_string();

    for (method, path) in [
        ("GET", format!("/bots/{bot_id}")),
        ("POST", format!("/bots/{bot_id}/pause")),
        ("POST", format!("/bots/{bot_id}/resume")),
    ] {
        let (status, body) = if method == "GET" {
            h.get(&path, Some(&stranger.token)).await
        } else {
            h.post(&path, json!({}), Some(&stranger.token)).await
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");
    }

    h.delete(&format!("/bots/{bot_id}"), Some(&owner.token))
        .await;
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    owner.cleanup(&h.database).await;
    stranger.cleanup(&h.database).await;
}

#[tokio::test]
async fn the_bot_routes_require_a_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (status, body) = h.get("/bots", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    let (status, _) = h
        .post(
            "/bots",
            json!({ "strategy_id": "00000000-0000-0000-0000-000000000000" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_malformed_strategy_id_is_a_400() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": "not-a-uuid" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "ID_INVALID");

    user.cleanup(&h.database).await;
}

#[test]
fn the_window_the_tests_feed_from_is_the_one_they_claim() {
    // `WINDOW_FROM` is shared with the strategy tests; the candle loader here
    // hardcodes the same instant. If one moves, the other silently starts
    // feeding an empty series and every bot test passes for the wrong reason.
    assert_eq!(WINDOW_FROM, "2026-09-10");
    assert_eq!(
        chrono::NaiveDate::parse_from_str(WINDOW_FROM, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            * 1_000_000_000,
        1_788_998_400_000_000_000i64
    );
}
