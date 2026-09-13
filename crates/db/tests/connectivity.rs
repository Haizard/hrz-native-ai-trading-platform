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

/// Confirms the initial schema created every table from docs/13.
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
