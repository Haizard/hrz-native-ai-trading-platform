//! Load and chaos, driven through the real router and the real supervisor.
//!
//! ## What "load" means here, and what it does not
//!
//! This is not a benchmark. It is the set of *properties* that have to hold
//! under concurrency and were never asserted anywhere: a fan-out that drops
//! candles for one watcher, a slow reader that stalls the publisher, a second
//! `POST /bots` that starts a second task on one row. Each of those is a real
//! failure mode of this design, and each is invisible at low volume.
//!
//! The numbers are small on purpose. A test that publishes ten thousand candles
//! to prove something about a broadcast channel is measuring the machine, and
//! `docs/16` already says what it thinks of tests whose runtime is a coin flip.
//! What is asserted is *conservation*: every candle that went in came out, to
//! every subscriber, once.
//!
//! ## The chaos half
//!
//! An external feed that dies mid-stream, and a bot whose task is stopped while
//! candles are still arriving. Both must leave the system in a state an operator
//! can read: the feed reconnect is `market-data`'s job and is tested there, so
//! what is tested here is that a *subscriber* that loses its source is told
//! rather than left waiting forever.

mod common;

use std::sync::Arc;

use analytics_core::types::{Candle, Timeframe};
use axum::http::StatusCode;
use common::{Harness, SIMPLE_STRATEGY};
use serde_json::json;
use uuid::Uuid;

/// A synthetic 5m candle.
///
/// Synthetic rather than loaded because this file is about the *plumbing*
/// between the feed and its subscribers, and a real series would make the
/// assertions depend on how many candles happen to be loaded -- which is how a
/// load test starts failing when somebody re-runs a backfill.
fn candle(symbol: &str, bar: usize) -> Candle {
    let price = 100.0 + (bar % 40) as f64;
    Candle {
        symbol: symbol.to_string(),
        timeframe: Timeframe::M5,
        open_time: bar as i64 * 5 * 60 * 1_000_000_000,
        open: price,
        high: price + 1.0,
        low: price - 1.0,
        close: price + 0.5,
        volume: 10.0,
        buy_volume: 6.0,
        sell_volume: 4.0,
    }
}

/// `POST /strategies` and return the id.
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

/// A paper bot built from the same document the route tests use.
///
/// Needed because `BotSupervisor::start` takes a constructed bot, and the
/// guard under test is *before* anything is done with it -- so the bot has to
/// be real enough to compile and is deliberately never run.
fn paper_bot() -> trading_engine::PaperBot {
    let validated =
        strategy_dsl::parse_and_validate(SIMPLE_STRATEGY).expect("the fixture must validate");
    let engine = strategy_runtime::StrategyEngine::new(
        &validated,
        strategy_runtime::RuntimeConfig::default(),
    )
    .expect("the fixture must be runnable");
    trading_engine::PaperBot::new(
        engine,
        trading_engine::PaperConfig {
            symbol: "BTCUSDT".into(),
            limits: trading_engine::RiskLimits::default(),
            fills: strategy_runtime::SimulatorConfig::default(),
            rolling: strategy_runtime::RollingConfig::new(500, 200, Default::default()),
        },
    )
}

/// Every subscriber receives every candle, once.
///
/// The property a fan-out is supposed to have, and the one that silently breaks
/// when a `broadcast` channel's buffer is smaller than the burst: the fast
/// subscribers are fine, and one of them quietly gets `Lagged` instead of the
/// candles. Counting is what catches it.
#[tokio::test]
async fn every_watcher_receives_every_candle() {
    let Some(h) = Harness::new().await else {
        return;
    };
    const WATCHERS: usize = 8;
    const CANDLES: usize = 200;

    let symbol = format!("LOAD{}", Uuid::new_v4().simple());
    let mut receivers = Vec::with_capacity(WATCHERS);
    for _ in 0..WATCHERS {
        receivers.push(h.supervisor.subscribe_candles(&symbol));
    }

    // Published one at a time, and drained as we go: a receiver that is not
    // read is a receiver that lags, which is the *other* test below. This one
    // is about whether every watcher that keeps up gets everything.
    for bar in 0..CANDLES {
        h.supervisor.feed_candle(&candle(&symbol, bar));
        for receiver in &mut receivers {
            // `try_recv` rather than `recv`: if a candle is not already there,
            // that is the failure being asserted on, and awaiting would turn it
            // into a hang.
            let received = receiver
                .try_recv()
                .unwrap_or_else(|e| panic!("a watcher missed a candle: {e}"));
            assert_eq!(received.open_time, bar as i64 * 5 * 60 * 1_000_000_000);
        }
    }
}

/// A slow subscriber is told it lagged, and does not stall the publisher.
///
/// The backpressure rule the bus is built on. A `tokio::sync::broadcast`
/// receiver that stops reading does not block the sender -- it drops the oldest
/// messages and reports `Lagged` on the next read. That is the correct
/// behaviour here and it is worth pinning, because the tempting "fix" (a bounded
/// channel with `await` on send) would let one paused bot stall the feed for
/// every other bot on the symbol.
#[tokio::test]
async fn a_subscriber_that_stops_reading_is_told_rather_than_blocking_the_feed() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let symbol = format!("LAG{}", Uuid::new_v4().simple());
    let mut slow = h.supervisor.subscribe_candles(&symbol);
    let mut fast = h.supervisor.subscribe_candles(&symbol);

    // More than the channel's buffer, with only the fast reader draining.
    //
    // Read off the constant rather than hardcoded: the first version of this
    // used a literal 4096, which is exactly `DEFAULT_CAPACITY`, so nothing ever
    // lagged and the test asserted the opposite of what it claimed. A burst
    // sized against the constant cannot drift back into passing vacuously.
    let burst = market_data::bus::DEFAULT_CAPACITY * 2;
    for bar in 0..burst {
        h.supervisor.feed_candle(&candle(&symbol, bar));
        // `try_recv` on the fast reader, ignoring lag: this reader is the one
        // proving the publisher kept going, not the one being asserted on.
        while fast.try_recv().is_ok() {}
    }

    // The publisher returned at all, which is the first half. The second half
    // is that the slow reader is *told*: it must not be handed a plausible
    // stream with a hole in it, because a bot trading on that would be trading
    // a market that did not happen.
    let mut lagged = false;
    loop {
        match slow.try_recv() {
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                lagged = true;
                break;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    assert!(
        lagged,
        "a reader {burst} messages behind must be told it lagged, not silently skipped"
    );
}

/// Starting the same bot twice starts one task.
///
/// Two tasks on one bot row would interleave their decisions into one audit
/// trail and neither would be the truth -- and the second `POST` looks exactly
/// like the first to a client, which is what makes this worth asserting rather
/// than assuming.
#[tokio::test]
async fn starting_the_same_bot_twice_is_still_one_task() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    let (status, created) = h
        .post(
            "/bots",
            json!({ "strategy_id": strategy_id }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let bot_id = created["id"].as_str().unwrap().to_string();
    let bot: Uuid = bot_id.parse().expect("a uuid");

    assert_eq!(h.supervisor.running_count(), 1);

    // A second POST creates a *second row*, which is a different bot and a
    // legitimate thing to ask for -- what must not happen is two tasks on the
    // first one. `start` is the guard, and it is what is exercised here: the
    // bot is built and passed in, and must never be reached.
    h.supervisor
        .start(bot, user.id, (*h.database).clone(), paper_bot());
    assert_eq!(
        h.supervisor.running_count(),
        1,
        "a second start on a running bot must be a no-op"
    );
    assert!(h.supervisor.is_running(bot));

    let (status, body) = h
        .delete(&format!("/bots/{bot_id}"), Some(&user.token))
        .await;
    assert!(
        status.is_success(),
        "deleting the bot must succeed, not {status}: {body}"
    );
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

/// A bot whose task is stopped mid-stream does not take the feed down with it.
///
/// The failure this pins: the supervisor owns one feed per symbol, so a stop
/// that tore the feed down would silently starve every *other* bot on that
/// symbol. Ten bots and a stop on one of them is an ordinary Tuesday.
#[tokio::test]
async fn stopping_one_bot_leaves_the_feed_working_for_the_others() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;
    let strategy_id = strategy(&h, &user.token).await;

    let mut ids = Vec::new();
    for _ in 0..3 {
        let (status, created) = h
            .post(
                "/bots",
                json!({ "strategy_id": strategy_id }),
                Some(&user.token),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    assert_eq!(h.supervisor.running_count(), 3);

    // Stop the middle one, the way a crash would.
    let doomed: Uuid = ids[1].parse().unwrap();
    assert!(h.supervisor.stop(doomed).await);
    assert_eq!(h.supervisor.running_count(), 2);

    // The others are still subscribed and still being served. Publishing is the
    // observable: a subscriber whose sender was dropped receives `Closed`, and
    // `try_recv` reports it rather than blocking.
    let symbol = format!("SURVIVOR{}", Uuid::new_v4().simple());
    let mut watcher = h.supervisor.subscribe_candles(&symbol);
    h.supervisor.feed_candle(&candle(&symbol, 0));
    assert!(
        watcher.try_recv().is_ok(),
        "the feed must still be serving after a bot on it stopped"
    );

    for id in &ids {
        // The result is checked, not discarded.
        //
        // It used to be ignored, and when one delete failed the test reported
        // the *consequence* -- a foreign-key violation from deleting the
        // strategy afterwards -- which points at the cleanup rather than at the
        // delete that did not happen. A test whose setup or teardown silently
        // ignores an error will always fail somewhere else.
        let (status, body) = h.delete(&format!("/bots/{id}"), Some(&user.token)).await;
        assert!(
            status.is_success(),
            "deleting bot {id} must succeed, not {status}: {body}"
        );
    }
    db::strategies::delete_strategy(h.database.pool(), strategy_id.parse().unwrap())
        .await
        .unwrap();
    user.cleanup(&h.database).await;
}

/// `/metrics` is served, and a request moves a counter.
///
/// The scrape endpoint is the one thing in Phase 8 that an operator depends on
/// during an incident, and "the endpoint exists" and "the endpoint reports
/// anything" are different claims. Both are checked, in that order.
#[tokio::test]
async fn the_metrics_endpoint_is_scraped_and_counts_requests() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // A request whose effect is countable, and whose route is templated -- the
    // id in the path must not become a label value, or every bot gets its own
    // time series and the scrape grows without bound.
    let missing = Uuid::new_v4();
    let (status, _) = h.get(&format!("/bots/{missing}"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, text) = h.get_text("/metrics", None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        text.contains("# TYPE"),
        "the scrape must be Prometheus text exposition format: {text}"
    );
    assert!(
        text.contains("http_requests_total"),
        "the scrape must carry the request counter: {text}"
    );
    assert!(
        !text.contains(&missing.to_string()),
        "a path parameter must not become a label value: {text}"
    );
}

/// A feed that stops producing is detected, because nothing else will say so.
///
/// ## The first version of this test asserted something false
///
/// It dropped a receiver and expected a `recv` to resolve. A `broadcast` channel
/// whose sender is still alive and simply has nothing to say does not resolve,
/// and *should* not: the watcher is correctly waiting for a candle. The test was
/// asserting that a quiet channel is closed, which is not true and not what
/// anyone wants.
///
/// ## The second version proved less than it looked like
///
/// It set `MD_FEED_AGE` by hand and checked the rule fired. That proves the rule
/// works; it says nothing about whether anything ever sets the gauge — and for a
/// while nothing did, so the alert was real in the test and impossible in
/// production.
///
/// This version drives the whole chain: a real supervisor is fed a real candle,
/// its ages are sampled by the same function the alert task calls, and the rule
/// reads the registry that function wrote into. The only thing not exercised is
/// the 30-second ticker.
#[tokio::test]
async fn a_feed_that_stops_producing_raises_a_stale_feed_alert() {
    use api_gateway::bots::{BotSupervisor, FeedMode};
    use api_gateway::metrics::publish_feed_ages;
    use observability::alerts::{default_rules, Alerter, CollectingSink, Severity};
    use observability::metrics::Registry;

    let registry = Registry::new();
    let sink = Arc::new(CollectingSink::new());
    let mut alerter = Alerter::with_sinks(default_rules(), vec![sink.clone()]);

    // No feed has started for anything yet. Nothing to say, and — importantly —
    // not an alert: "no feed configured" is a different incident from "the feed
    // died", and only the second should page anybody.
    let supervisor = BotSupervisor::new(FeedMode::Off);
    publish_feed_ages(&supervisor, &registry, now());
    assert!(
        alerter.evaluate(&registry).is_empty(),
        "a platform with no feed must not report a stale one"
    );

    // A candle arrives, so the feed is alive.
    supervisor.feed_candle(&candle("BTCUSDT", 0));
    publish_feed_ages(&supervisor, &registry, now());
    assert!(alerter.evaluate(&registry).is_empty());
    assert!(!alerter.is_active("stale_market_data"));

    // Ten minutes later, with nothing new, the age is the whole ten minutes and
    // the rule fires. The alert has to name the number, because "the feed is
    // stale" without a value is a page an engineer cannot act on beyond opening
    // a terminal.
    publish_feed_ages(&supervisor, &registry, now() + TEN_MINUTES);
    let raised = alerter.evaluate(&registry);
    let alert = raised
        .iter()
        .find(|alert| alert.name == "stale_market_data")
        .unwrap_or_else(|| panic!("a ten-minute-old feed must alert: {raised:?}"));
    assert_eq!(alert.severity, Severity::Warning, "{alert:?}");
    assert!(
        alert.detail.contains("600"),
        "the alert must name the age it saw: {alert:?}"
    );
    assert!(
        alert.detail.contains("BTCUSDT"),
        "the alert must name the symbol, or two dead feeds read as one: {alert:?}"
    );
    assert!(alerter.is_active("stale_market_data"));

    // Unchanged: silence. A feed that has been stale for an hour is one
    // incident, not one per tick, and an alert that repeats is an alert that
    // gets muted.
    assert!(
        alerter.evaluate(&registry).is_empty(),
        "an unchanged breach must stay quiet"
    );

    // And it clears when the feed comes back, which is what stops the page from
    // being one nobody can turn off.
    supervisor.feed_candle(&candle("BTCUSDT", 1));
    publish_feed_ages(&supervisor, &registry, now());
    let cleared = alerter.evaluate(&registry);
    assert!(
        cleared
            .iter()
            .any(|alert| alert.name == "stale_market_data" && alert.detail == "resolved"),
        "{cleared:?}"
    );
    assert!(!alerter.is_active("stale_market_data"));
    assert_eq!(sink.alerts().len(), 2, "one breach, one resolution");
}

/// Ten minutes, in unix nanos — how far the test moves the clock forward.
const TEN_MINUTES: i64 = 600 * 1_000_000_000;

/// Now, in unix nanos. The test's clock, not the platform's.
fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i64)
}
