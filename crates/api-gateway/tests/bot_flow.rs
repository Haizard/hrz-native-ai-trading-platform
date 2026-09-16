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

/// How many decisions the user's bot has written so far.
async fn decisions(h: &Harness, user_id: uuid::Uuid) -> i64 {
    db::paper::count_audit_events(h.database.pool(), user_id, "bot.decision")
        .await
        .unwrap_or(0)
}

/// Poll until the bot has written `expected` decisions, or give up.
///
/// The bot runs in its own task, so there is no moment at which "the candles
/// have been processed" is observable from outside. Polling is the honest way
/// to wait for an asynchronous effect, and the timeout is what makes a failure
/// a failed assertion rather than a hung test.
async fn wait_for_decisions(h: &Harness, user_id: uuid::Uuid, expected: i64) -> i64 {
    for _ in 0..100 {
        let count = decisions(h, user_id).await;
        if count >= expected {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    decisions(h, user_id).await
}

/// Feed a batch, wait for the bot to have decided on all of it, and return how
/// many decisions the **database** now holds.
///
/// Two waits, because there are two things to synchronise with and they are not
/// the same. The event stream says the batch has been *decided*; the table says
/// it has been *written*, and the bot decides in memory and flushes on its own
/// tick. Returning the stream's count would compare a number that runs ahead of
/// the table against numbers read from it later.
async fn feed_and_settle(
    h: &Harness,
    series: &[Candle],
    bot_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> i64 {
    // Read the table *before* feeding. The event count is a delta for this
    // batch; the table is a running total. Waiting for the total to reach the
    // delta works only for the first batch -- after that the previous batch has
    // already put the table above it, and the wait returns without the flush
    // having happened at all. Adding the two makes the wait mean "this batch's
    // rows are committed", which is what the callers assert on.
    let before = decisions(h, user_id).await;
    let decided = feed_and_drain(h, series, bot_id).await as i64;
    wait_for_decisions(h, user_id, before + decided).await
}

/// Feed a batch and wait until the bot has decided on the last candle of it.
///
/// Returns how many decisions the bot made on the way.
///
/// This synchronises on the bot's **own event stream** rather than on the
/// database or on a timer, and that is the point. Polling for quiescence is a
/// guess about how long a flush takes, and it is wrong exactly when the machine
/// is busy -- which is when tests fail for no reason. The event stream is
/// ordered, it is what a watching client sees, and each decision names the
/// candle it was made on, so "the batch is drained" becomes an observation.
///
/// The last candle is the signal: once its ladder is warm the bot decides on
/// every candle it receives, so a decision carrying the last candle's close
/// time means there is nothing left in flight behind it.
async fn feed_and_drain(h: &Harness, series: &[Candle], bot_id: uuid::Uuid) -> usize {
    // Subscribed before anything is published: a broadcast only delivers from
    // the point of subscription, so subscribing afterwards would wait forever
    // for events that had already gone by.
    let mut events = h.supervisor.subscribe_events();
    let last = series.last().map_or(0, |candle| candle.open_time);

    let mut decisions = 0;
    let mut drained = false;
    let note = |event: api_gateway::bots::BotEvent, decisions: &mut usize, drained: &mut bool| {
        if let api_gateway::bots::BotEvent::Decision { bot_id: id, record } = event {
            if id == bot_id {
                *decisions += 1;
                // `at` is the decision candle's close time, so it is strictly
                // after the candle it was made on.
                if record.at > last {
                    *drained = true;
                }
            }
        }
    };

    for candle in series {
        h.supervisor.feed_candle(candle);
        // Drained as we go so the broadcast buffer cannot fill behind us. The
        // one event that must not be skipped is the last decision, and a lagged
        // receiver skips events.
        while let Ok(event) = events.try_recv() {
            note(event, &mut decisions, &mut drained);
        }
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !drained {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(event)) => note(event, &mut decisions, &mut drained),
            // Lagged or closed. Returning what was seen lets the caller's own
            // assertion do the reporting rather than a bare timeout here.
            Ok(Err(_)) | Err(_) => break,
        }
    }
    decisions
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
    let bot = bot_id.parse::<uuid::Uuid>().expect("a uuid");

    // Feed it a real market, and wait for the bot to have decided on the last
    // candle of it and flushed. Everything asserted below is about a finished
    // batch.
    let series = candles(&h, 200).await;
    assert!(series.len() > 100, "expected candles to feed");
    let decisions = feed_and_settle(&h, &series, bot, user.id).await;
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
    //
    // This is also the assertion that `stop` means it: it returns `true` only
    // once the task is gone, so the row it wrote is committed by the time the
    // count below runs.
    assert!(h.supervisor.stop(bot).await);
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
    let bot_id = h
        .created(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    let bot = bot_id.parse::<uuid::Uuid>().expect("a uuid");

    // Three batches, and the third has to be candles the bot has *not* seen.
    //
    // The bot ignores anything not newer than the newest candle it holds, so
    // re-feeding the paused batch after resuming proves nothing: those candles
    // were already consumed while paused. The test used to do exactly that, and
    // passed only because its "before" count was stale -- the decisions it read
    // as new were the first batch still arriving.
    let series = candles(&h, 180).await;
    assert!(series.len() >= 180, "expected three batches of candles");

    // Batch one: decided and flushed before anything is paused. Capturing a
    // partial count here is what made this test intermittent, because the rest
    // of the batch arrived after the pause and looked exactly like a paused bot
    // that had carried on deciding.
    let before_pause = feed_and_settle(&h, &series[..60], bot, user.id).await;
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
    //
    // A fixed wait rather than a poll, and deliberately: this asserts that
    // nothing happened, so there is no event to wait for. The wait is the
    // opportunity for the bot to have decided, and it is set well above the
    // harness's flush interval so that "it had the chance and did not" is what
    // is being asserted rather than "it had not got there yet".
    //
    // These are candles the bot has never seen, which is the whole point: if the
    // pause were ignored they would decide, so the count staying put is evidence
    // about the pause and not about the feed.
    for candle in &series[60..120] {
        h.supervisor.feed_candle(candle);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let during_pause = decisions(&h, user.id).await;
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

    // The third batch, and it has to be candles the bot has *not* seen. The
    // window ignores anything not newer than the newest candle it holds, so
    // re-feeding the paused batch here would prove nothing -- those were already
    // consumed above, while paused. The test used to do exactly that and passed
    // only because its "before" count was stale: the decisions it read as new
    // were the first batch still arriving.
    //
    // Drained for the same reason as the first batch: the rewind assertion below
    // compares against this count, and a count taken mid-batch would make the
    // *old* candles look like the ones that decided.
    let after_resume = feed_and_settle(&h, &series[120..], bot, user.id).await;
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
    // Same shape as the pause assertion above: nothing should happen, so the
    // wait is the opportunity rather than a thing being waited for.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_rewind = decisions(&h, user.id).await;
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
    let bot_id = h
        .created(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    let bot = bot_id.parse::<uuid::Uuid>().expect("a uuid");

    // Stop the task without touching the row, which is what a process restart
    // looks like from the database's point of view.
    assert!(h.supervisor.stop(bot).await);

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
async fn live_mode_without_a_venue_is_a_422() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    // The opt-in is per venue, so there is no default to fall back to. A live
    // bot with no venue is not a request that can be completed with an
    // assumption.
    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id, "mode": "live" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "VENUE_REQUIRED");

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn live_mode_without_an_opt_in_is_refused_with_every_reason() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    // docs/11 and docs/15 gate live trading behind a paper track record, a
    // per-venue opt-in and configured risk limits. None of the three holds
    // here, and the refusal must name all of them: an operator should not need
    // one round trip per requirement.
    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id, "mode": "live", "venue": "binance" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "LIVE_GATE_REFUSED");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("opted in"),
        "the opt-in is missing: {message}"
    );
    assert!(
        message.contains("paper trades"),
        "the track record is missing: {message}"
    );
    assert!(
        !message.contains("does not exist yet"),
        "the adapter exists now, so the refusal must not still claim otherwise: {message}"
    );

    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

#[tokio::test]
async fn an_unknown_mode_is_a_422_rather_than_a_silent_paper_bot() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    // Silently running a paper bot for someone who asked for a live one is the
    // worst answer available: they believe they have a position and they do not.
    let (status, body) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id, "mode": "paperr" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "MODE_UNKNOWN");

    // And nothing was started.
    let listed = h.ok("/bots", Some(&user.token)).await;
    assert_eq!(listed.as_array().map(Vec::len), Some(0), "{listed}");

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
    let bot_id = h
        .created(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&owner.token),
        )
        .await;

    for (method, path) in [
        ("GET", format!("/bots/{bot_id}")),
        ("POST", format!("/bots/{bot_id}/pause")),
        ("POST", format!("/bots/{bot_id}/resume")),
        ("POST", format!("/bots/{bot_id}/kill")),
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
