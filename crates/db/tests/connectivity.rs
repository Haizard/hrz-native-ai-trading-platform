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
/// Three properties, and each one is load-bearing:
///
/// 1. An opt-in followed by a revoke leaves the venue **off** — the newest row
///    wins, so history can't resurrect a revoked venue.
/// 2. Inserting the same `client_order_id` twice reports `false` the second
///    time. That is the database half of the idempotency guarantee; if this
///    returned `true` twice, a retry would look like a fresh placement.
/// 3. A `FILLED` order drops out of `open_live_orders`, which is the query
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
    db::live::record_venue_opt_in(pool, user_id, "binance", "opt_in", Some("initial"))
        .await
        .expect("opt in");
    assert_eq!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read"),
        vec!["binance".to_string()],
        "an opt-in should enable the venue"
    );

    db::live::record_venue_opt_in(pool, user_id, "binance", "revoke", Some("drill"))
        .await
        .expect("revoke");
    assert!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read")
            .is_empty(),
        "a revoke must win over the earlier opt-in"
    );

    // Venue names are normalised, so `BINANCE` and `binance` are one venue.
    db::live::record_venue_opt_in(pool, user_id, "BINANCE", "opt_in", None)
        .await
        .expect("opt in again");
    assert_eq!(
        db::live::opted_in_venues(pool, user_id)
            .await
            .expect("read"),
        vec!["binance".to_string()],
        "case must not create a second venue"
    );

    // 2. The same id twice is one order.
    let order = db::live::LiveOrderRow {
        client_order_id: "p8bot_BTCUSDT_abc_entry".to_string(),
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

    // 3. Open orders exclude terminal ones.
    assert_eq!(
        db::live::open_live_orders(pool, bot_id)
            .await
            .expect("open")
            .len(),
        1,
        "a NEW order is open"
    );

    db::live::update_live_order(
        pool,
        &order.client_order_id,
        "FILLED",
        0.01,
        Some(60_000.0),
        None,
    )
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
