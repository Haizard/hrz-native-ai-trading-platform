//! End-to-end persistence, against a real database.
//!
//! ## Why this is an integration test and not a unit test
//!
//! `docs/11` requires every simulated trade and every `on_candle` decision to be
//! persisted. Every part of that can be unit-tested in isolation -- the payload
//! shape, the draining, the SQL string -- and all of it can still be wrong,
//! because the thing that is actually being asserted is "a row exists in
//! Postgres". So this test connects, runs a real bot over real candles, and
//! then asks the database what it has.
//!
//! ## It cleans up after itself
//!
//! `audit_log` is append-only by design (`docs/15`), and a test that leaves rows
//! behind in a shared database is a test that makes the next run's numbers
//! wrong. So it writes under a uniquely-named owner and purges that owner at
//! the end, whether or not the assertions passed.
//!
//! Without `DATABASE_URL` it prints why and returns, rather than failing: the
//! suite must still be runnable on a machine with no database.

use std::collections::BTreeMap;

use analytics_core::types::{Candle, Timeframe};
use db::Database;
use strategy_runtime::{RuntimeConfig, SimulatorConfig, StrategyEngine};
use trading_engine::{
    BotSession, PaperBot, PaperConfig, RiskLimits, DECISION_EVENT, NOTIFICATION_EVENT, RISK_EVENT,
    STARTED_EVENT, STOPPED_EVENT,
};

/// A permissive document, so the bot reliably produces trades on real candles
/// and the `trades_executed` half of the criterion is actually exercised.
///
/// The reference strategy is too selective for a short window: it needs a 4h
/// trend and a swept level to line up, and a two-week window may contain none.
/// A test that asserts nothing was written because nothing happened would pass
/// for the wrong reason.
const DOCUMENT: &str = r#"
name: "Persistence harness"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: close > vwap
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: "risk_multiple"
    value: 1.0
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
"#;

const NANOS_PER_DAY: i64 = 86_400 * 1_000_000_000;

/// Every owner this file creates is named with this prefix, so a previous run
/// that panicked before cleaning up can be swept away before it is counted.
const TEST_PREFIX: &str = "paper-bot-test-";

async fn database() -> Option<Database> {
    let _ = dotenvy::dotenv();
    if std::env::var("DATABASE_URL").is_err() {
        return None;
    }
    let database = Database::from_env().await.ok()?;
    database.migrate().await.ok()?;

    // Sweep up after any previous run that panicked mid-assertion and so never
    // reached its own cleanup. The age filter matters: cargo runs these tests in
    // parallel, and a sweep with no window would delete the fixtures of tests
    // that are still running.
    let swept = db::paper::purge_owners_with_prefix(database.pool(), TEST_PREFIX, 60)
        .await
        .ok()?;
    if swept > 0 {
        eprintln!("swept {swept} stale test owner(s) from an earlier run");
    }

    Some(database)
}

fn engine() -> StrategyEngine {
    let validated = strategy_dsl::parse_and_validate(DOCUMENT).expect("the document must validate");
    StrategyEngine::new(&validated, RuntimeConfig::default())
        .expect("the document must be runnable")
}

fn bot(limits: RiskLimits) -> PaperBot {
    PaperBot::new(
        engine(),
        PaperConfig {
            symbol: "BTCUSDT".into(),
            limits,
            fills: SimulatorConfig::default(),
            rolling: strategy_runtime::RollingConfig::new(500, 200, Default::default()),
        },
    )
}

/// Real 5m candles from the database, over a window that is known to be loaded.
async fn candles(database: &Database, days: i64) -> Vec<Candle> {
    let to = 1_789_344_000_000_000_000i64; // 2026-09-14T00:00:00Z
    let from = to - days * NANOS_PER_DAY;
    db::repositories::load_candles(database.pool(), "BTCUSDT", Timeframe::M5, from, to)
        .await
        .expect("the candle query must succeed")
}

/// Wide enough that the risk engine never intervenes, so the test measures
/// persistence rather than the limits.
fn wide() -> RiskLimits {
    RiskLimits {
        daily_loss_limit_r: 1_000_000.0,
        weekly_loss_limit_r: 1_000_000.0,
        ..RiskLimits::default()
    }
}

#[tokio::test]
async fn every_decision_and_trade_reaches_the_database() {
    let Some(database) = database().await else {
        eprintln!("DATABASE_URL is not set; skipping the persistence test");
        return;
    };

    let candles = candles(&database, 12).await;
    assert!(
        candles.len() > 1_000,
        "expected a loaded window, got {} candles",
        candles.len()
    );

    let owner = format!("{TEST_PREFIX}{}@local", uuid::Uuid::new_v4());
    let user_id = db::paper::create_owner(database.pool(), &owner)
        .await
        .expect("the owner must be created");

    let document = serde_json::to_value(strategy_dsl::parse(DOCUMENT).unwrap()).unwrap();
    let mut session = BotSession::start(
        &database,
        user_id,
        "Persistence harness",
        "1",
        &document,
        "paper",
        None,
    )
    .await
    .expect("the bot must be registered");

    let bot_id = session.bot_id();
    let mut bot = bot(wide());

    // Flush the way a runner does: periodically, not once at the end.
    let mut flushed_decisions = 0usize;
    for (index, candle) in candles.iter().enumerate() {
        bot.on_candle(candle);
        if index % 250 == 0 {
            session.flush(&mut bot).await.expect("a flush must succeed");
            flushed_decisions += 1;
        }
    }
    session
        .finish(&mut bot)
        .await
        .expect("the bot must be finalised");

    // --- what the database actually holds ---
    let decisions = db::paper::count_audit_events(database.pool(), user_id, DECISION_EVENT)
        .await
        .unwrap();
    let trades = db::paper::count_executed_trades(database.pool(), bot_id)
        .await
        .unwrap();
    let started = db::paper::count_audit_events(database.pool(), user_id, STARTED_EVENT)
        .await
        .unwrap();
    let stopped = db::paper::count_audit_events(database.pool(), user_id, STOPPED_EVENT)
        .await
        .unwrap();

    assert!(flushed_decisions > 0);
    assert_eq!(
        decisions as usize,
        candles.len(),
        "every decision candle must have a row, including the ones that did nothing"
    );
    assert!(
        trades > 0,
        "the fixture must trade, or the trade half of the criterion is untested"
    );
    assert_eq!(
        trades as usize,
        bot.trades().len(),
        "every completed trade must have a row"
    );
    assert_eq!(started, 1, "a run must record that it started");
    assert_eq!(
        stopped, 1,
        "a run must record that it stopped, or a crash is indistinguishable"
    );

    // And the decisions that did nothing are genuinely in there, not just the
    // interesting ones. Read back from the database rather than from the bot:
    // `finish` already drained it, and a check against an empty vector would
    // pass for the wrong reason.
    let recent = db::paper::recent_decisions(database.pool(), bot_id, 50)
        .await
        .unwrap();
    assert_eq!(recent.len(), 50, "the decisions must be readable back");
    assert!(
        recent.iter().any(|payload| payload["kind"] == "no_signal"),
        "the quiet bars must be recorded too, not only the ones that traded"
    );
    assert!(
        recent
            .iter()
            .any(|payload| payload["bot_id"] == bot_id.to_string()),
        "every row must name the bot it came from"
    );

    db::paper::purge_bot(database.pool(), bot_id).await.unwrap();
    db::paper::purge_owner(database.pool(), user_id)
        .await
        .unwrap();

    let left = db::paper::count_executed_trades(database.pool(), bot_id)
        .await
        .unwrap();
    assert_eq!(left, 0, "the test must leave nothing behind");
}

#[tokio::test]
async fn a_breach_writes_a_notification_the_ui_can_list() {
    let Some(database) = database().await else {
        eprintln!("DATABASE_URL is not set; skipping the notification test");
        return;
    };

    let candles = candles(&database, 12).await;
    if candles.len() < 100 {
        eprintln!("not enough candles to run; skipping");
        return;
    }

    let owner = format!("{TEST_PREFIX}{}@local", uuid::Uuid::new_v4());
    let user_id = db::paper::create_owner(database.pool(), &owner)
        .await
        .unwrap();
    let document = serde_json::to_value(strategy_dsl::parse(DOCUMENT).unwrap()).unwrap();
    let mut session = BotSession::start(
        &database,
        user_id,
        "Persistence harness",
        "1",
        &document,
        "paper",
        None,
    )
    .await
    .unwrap();
    let bot_id = session.bot_id();

    // A zero daily budget: the tightest limit there is, and the one that proves
    // the breach path without needing the market to cooperate.
    let mut bot = bot(RiskLimits {
        daily_loss_limit_r: 0.0,
        ..wide()
    });
    for candle in &candles {
        bot.on_candle(candle);
    }
    session.finish(&mut bot).await.unwrap();

    assert!(bot.is_halted(), "a zero budget must halt the bot");

    let notifications = db::paper::count_audit_events(database.pool(), user_id, NOTIFICATION_EVENT)
        .await
        .unwrap();
    let breaches = db::paper::count_audit_events(database.pool(), user_id, RISK_EVENT)
        .await
        .unwrap();
    assert!(
        notifications > 0,
        "docs/11 asks for the user to be notified, not just for a row to exist"
    );
    assert!(breaches > 0, "the breach must be its own greppable event");

    db::paper::purge_bot(database.pool(), bot_id).await.unwrap();
    db::paper::purge_owner(database.pool(), user_id)
        .await
        .unwrap();
}

/// The purge really does remove everything a run wrote.
///
/// Worth its own test because the other two depend on it to leave the database
/// as they found it, and a cleanup that silently half-works would let rows
/// accumulate until the counts in those tests stopped meaning anything.
#[tokio::test]
async fn purging_a_bot_removes_its_trades_and_its_audit_rows() {
    let Some(database) = database().await else {
        eprintln!("DATABASE_URL is not set; skipping the purge test");
        return;
    };

    let owner = format!("{TEST_PREFIX}{}@local", uuid::Uuid::new_v4());
    let user_id = db::paper::create_owner(database.pool(), &owner)
        .await
        .unwrap();
    let document = serde_json::to_value(strategy_dsl::parse(DOCUMENT).unwrap()).unwrap();
    let session = BotSession::start(
        &database,
        user_id,
        "Persistence harness",
        "1",
        &document,
        "paper",
        None,
    )
    .await
    .unwrap();
    let bot_id = session.bot_id();
    drop(session);

    let candles = candles(&database, 2).await;
    let mut bot = bot(wide());
    for candle in &candles {
        bot.on_candle(candle);
    }
    let mut session = BotSession::start(
        &database,
        user_id,
        "Persistence harness",
        "1",
        &document,
        "paper",
        None,
    )
    .await
    .unwrap();
    session.flush(&mut bot).await.unwrap();
    let decisions = db::paper::count_audit_events(database.pool(), user_id, DECISION_EVENT)
        .await
        .unwrap();
    assert!(decisions > 0, "there must be something to purge");

    db::paper::purge_bot(database.pool(), session.bot_id())
        .await
        .unwrap();
    let after = db::paper::count_audit_events(database.pool(), user_id, DECISION_EVENT)
        .await
        .unwrap();
    assert_eq!(after, 0, "the bot's audit rows must be gone");

    // The other bot's rows are untouched, which is what makes the match on
    // `bot_id` load-bearing rather than decorative.
    let other = db::paper::count_audit_events(database.pool(), user_id, STARTED_EVENT)
        .await
        .unwrap();
    assert_eq!(
        other, 1,
        "purging one bot must not touch the other's audit rows"
    );

    db::paper::purge_bot(database.pool(), bot_id).await.unwrap();
    db::paper::purge_owner(database.pool(), user_id)
        .await
        .unwrap();
}

/// Every declared timeframe a bot needs, for wiring a subscription.
#[test]
fn a_document_with_no_timeframes_is_rejected_before_a_bot_exists() {
    // Cheap guard on the constructor's assumptions, kept next to the
    // integration tests because it is the same harness.
    let validated = strategy_dsl::parse_and_validate(DOCUMENT).unwrap();
    let engine = StrategyEngine::new(&validated, RuntimeConfig::default()).unwrap();
    let declared: BTreeMap<String, Timeframe> = engine.document().timeframes.clone();
    assert_eq!(declared.get("entry"), Some(&Timeframe::M5));
}
