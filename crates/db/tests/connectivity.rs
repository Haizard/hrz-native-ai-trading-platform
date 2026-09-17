//! Live-database integration tests.
//!
//! These are `#[ignore]`d by default so CI stays green without credentials.
//! Run them once you have a real `DATABASE_URL` (e.g. the Northflank instance):
//!
//! ```bash
//! export DATABASE_URL='postgres://...'
//! cargo test -p db -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use db::{Database, MIGRATIONS};
use uuid::Uuid;

/// Load `.env` so `DATABASE_URL` is visible to the test process.
fn load_env() {
    let _ = dotenvy::dotenv();
}

/// How many rows the trail holds for one `(user, venue)` pair.
///
/// The count is the observable that distinguishes "recorded a transition" from
/// "recorded a click": the state is the newest row, so only the count can tell
/// whether a repeat added noise.
async fn rows_for(pool: &sqlx::PgPool, user_id: Uuid, venue: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM venue_opt_ins WHERE user_id = $1 AND venue = $2")
        .bind(user_id)
        .bind(venue)
        .fetch_one(pool)
        .await
        .expect("count rows")
}

/// Verifies the connection string works end to end.
#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn connects_and_applies_migrations() {
    load_env();
    let database = Database::from_env()
        .await
        .expect("DATABASE_URL must point at a reachable Postgres instance");

    MIGRATIONS
        .run(database.pool())
        .await
        .expect("migrations should apply cleanly");

    database
        .health()
        .await
        .expect("health probe should succeed after migrating");
}

/// Confirms the initial schema created every table from docs/13, plus the
/// Phase 8 live-trading tables from migration `0002`.
#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn initial_schema_has_expected_tables() {
    load_env();
    let database = Database::from_env().await.expect("DATABASE_URL");
    MIGRATIONS.run(database.pool()).await.expect("migrate");

    let expected = [
        "candles",
        "trades",
        "orderbook_snapshots",
        "users",
        "skills",
        "strategies",
        "backtests",
        "bots",
        "trades_executed",
        "audit_log",
        // Phase 8 — live trading.
        "venue_opt_ins",
        "live_orders",
    ];

    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .fetch_all(database.pool())
    .await
    .expect("should be able to list tables");

    for table in expected {
        assert!(rows.iter().any(|r| r == table), "missing table `{table}`");
    }
}

/// The Phase 8 live path, against a real database.
///
/// Five properties, and each one is load-bearing:
///
/// 1. An opt-in followed by a revoke leaves the venue **off** — the newest row
///    wins, so history can't resurrect a revoked venue.
/// 2. Repeating an action records nothing and reports `false`. That is what
///    makes the caller's log line truthful: two overlapping requests from one
///    double-click must not both claim that live trading *was enabled*.
/// 3. A call that cannot take the `(user, venue)` lock writes nothing. This is
///    what stops the check in (2) from being a race between two requests that
///    overlap, which is the case it exists for.
/// 4. Inserting the same `client_order_id` twice reports `false` the second
///    time. That is the database half of the idempotency guarantee; if this
///    returned `true` twice, a retry would look like a fresh placement.
/// 5. A `FILLED` order drops out of `open_live_orders`, which is the query
///    reconciliation and the UI both build on.
#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn live_orders_and_venue_opt_in_round_trip() {
    load_env();
    let database = Database::from_env().await.expect("DATABASE_URL");
    MIGRATIONS.run(database.pool()).await.expect("migrate");
    let pool = database.pool();

    // A user -> strategy -> bot chain, since both new tables are keyed off it.
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(format!("phase8-{}@example.test", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    let strategy_id: Uuid = sqlx::query_scalar(
        "INSERT INTO strategies (user_id, name, version, document, created_by) \
         VALUES ($1, 'phase8', '1.0.0', '{}'::jsonb, 'developer_sdk') RETURNING id",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert strategy");

    let bot_id: Uuid = sqlx::query_scalar(
        "INSERT INTO bots (user_id, strategy_id, mode, status, venue) \
         VALUES ($1, $2, 'live', 'running', 'binance') RETURNING id",
    )
    .bind(user_id)
    .bind(strategy_id)
    .fetch_one(pool)
    .await
    .expect("insert bot");

    // 1. Opt-in, then revoke. Newest row wins.
    let first = db::live::record_venue_opt_in(pool, user_id, "binance", "opt_in", Some("initial"))
        .await
        .expect("opt in");
    assert!(first, "the first opt-in is a transition");
    assert_eq!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read"),
        vec!["binance".to_string()],
        "an opt-in should enable the venue"
    );

    // 2. Repeating it changes nothing and records nothing. This is the contract
    // the log line depends on: two "live trading was enabled" warnings for one
    // decision is what an operator reads as a bug.
    let repeat = db::live::record_venue_opt_in(pool, user_id, "binance", "opt_in", None)
        .await
        .expect("opt in again");
    assert!(!repeat, "a repeated opt-in is not a transition");
    assert_eq!(
        rows_for(pool, user_id, "binance").await,
        1,
        "a repeat must not add a second row to the trail"
    );

    let revoked = db::live::record_venue_opt_in(pool, user_id, "binance", "revoke", Some("drill"))
        .await
        .expect("revoke");
    assert!(revoked, "a revoke after an opt-in is a transition");
    assert!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read")
            .is_empty(),
        "a revoke must win over the earlier opt-in"
    );

    // Venue names are normalised, so `BINANCE` and `binance` are one venue --
    // which also means the repeat check must compare the normalised name.
    let cased = db::live::record_venue_opt_in(pool, user_id, "BINANCE", "opt_in", None)
        .await
        .expect("opt in again");
    assert!(cased, "an opt-in after a revoke is a transition");
    assert_eq!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read"),
        vec!["binance".to_string()],
        "case must not create a second venue"
    );

    let cased_repeat = db::live::record_venue_opt_in(pool, user_id, "BiNaNcE", "opt_in", None)
        .await
        .expect("opt in a third time");
    assert!(
        !cased_repeat,
        "case must not make a repeat look like a new decision"
    );
    assert_eq!(
        rows_for(pool, user_id, "binance").await,
        3,
        "two opt-ins and one revoke -- three transitions, and the two repeats wrote nothing"
    );

    // 3. The check and the insert are serialised.
    //
    // This is the double-click, tested by its *mechanism* rather than by racing
    // two calls and hoping they overlap. They often do not: an earlier version
    // of this test used `tokio::join!` and passed with the advisory lock
    // deleted, which made it worse than no guard at all. So the lock is taken
    // here, from a connection of our own, and the call under test must be
    // unable to write while it is held.
    //
    // `pg_advisory_lock` is session-scoped and `pg_advisory_xact_lock` is
    // transaction-scoped, but both index the same lock space by the same key,
    // so one blocks the other.
    let mut blocker = pool.acquire().await.expect("a connection to hold the lock");
    let key = format!("{user_id}:binance");
    sqlx::query("SELECT pg_advisory_lock(hashtext($1)::bigint)")
        .bind(key.as_str())
        .execute(&mut *blocker)
        .await
        .expect("take the lock");

    let waiting = tokio::spawn({
        let pool = sqlx::PgPool::clone(pool);
        async move { db::live::record_venue_opt_in(&pool, user_id, "binance", "revoke", None).await }
    });

    // Long enough that an *unblocked* call would certainly have finished: a
    // begin, a lock, a select, an insert and a commit, against a managed
    // instance that costs roughly a second per statement.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    assert_eq!(
        rows_for(pool, user_id, "binance").await,
        3,
        "a call waiting on the (user, venue) lock must not have written anything"
    );

    sqlx::query("SELECT pg_advisory_unlock(hashtext($1)::bigint)")
        .bind(key.as_str())
        .execute(&mut *blocker)
        .await
        .expect("release the lock");

    assert!(
        waiting.await.expect("task").expect("revoke"),
        "once the lock is released, the waiting revoke is a transition"
    );
    assert_eq!(
        rows_for(pool, user_id, "binance").await,
        4,
        "and it writes exactly one row"
    );

    // 4. The same id twice is one order.
    //
    // The id carries the bot's fresh UUID because nothing here cleans up after
    // itself, and `live_orders.client_order_id` is the primary key. A hardcoded
    // id makes the "first insert reports new" assertion true only on a database
    // that has never run this test -- it fails on the second run, for a reason
    // that has nothing to do with the code under test. `docs/16`'s rule: a test
    // owns its fixture.
    let client_order_id = format!("p8bot_BTCUSDT_{}_entry", bot_id.simple());
    let order = db::live::LiveOrderRow {
        client_order_id: client_order_id.clone(),
        bot_id,
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: "BUY".to_string(),
        order_type: "MARKET".to_string(),
        quantity: 0.01,
        status: "NEW".to_string(),
        exchange_order_id: Some("12345".to_string()),
        filled_qty: 0.0,
        avg_price: None,
        placed_at: 1_700_000_000_000_000_000,
    };

    assert!(
        db::live::record_live_order(pool, &order)
            .await
            .expect("first insert"),
        "the first insert of a client order id must report that it was new"
    );
    assert!(
        !db::live::record_live_order(pool, &order)
            .await
            .expect("second insert"),
        "a repeated client order id must report that nothing was inserted"
    );

    // 5. Open orders exclude terminal ones.
    assert_eq!(
        db::live::open_live_orders(pool, bot_id)
            .await
            .expect("open")
            .len(),
        1,
        "a NEW order is open"
    );

    db::live::update_live_order(pool, &client_order_id, "FILLED", 0.01, Some(60_000.0), None)
        .await
        .expect("update");

    assert!(
        db::live::open_live_orders(pool, bot_id)
            .await
            .expect("open")
            .is_empty(),
        "a FILLED order is not open"
    );

    let listed = db::live::list_live_orders(pool, bot_id, 10)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "FILLED");
    assert_eq!(listed[0].filled_qty, 0.01);
    // `update_live_order` uses COALESCE on the venue id, so passing None must
    // not erase the id we already learned.
    assert_eq!(
        listed[0].exchange_order_id.as_deref(),
        Some("12345"),
        "an update without a venue id must not clear the stored one"
    );
}
