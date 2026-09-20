//! Pool wrapper and health checks.
//!
//! Phase 0 delivers the wrapper; the query models (candles, trades, skills,
//! strategies, backtests, bots, audit log) land with their owning phases.

use sqlx::PgPool;
use tracing::{info, warn};

use crate::config::DatabaseConfig;
use crate::error::DbError;
use crate::MIGRATIONS;

/// An open handle to the database.
///
/// Cheap to clone: it wraps an `Arc` around the underlying pool.
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
    config: DatabaseConfig,
}

impl Database {
    /// Connect using configuration read from the environment.
    ///
    /// Verifies connectivity with a `SELECT 1` before returning, so a bad URL
    /// fails at startup rather than on the first query.
    pub async fn from_env() -> Result<Self, DbError> {
        Self::connect(DatabaseConfig::from_env()?).await
    }

    /// Connect using explicit configuration and verify the connection.
    pub async fn connect(config: DatabaseConfig) -> Result<Self, DbError> {
        let pool = config.create_pool().await?;
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&pool)
            .await?;

        info!(
            target: "db",
            url = %config.redacted_url(),
            max_connections = config.max_connections,
            "connected to postgres"
        );

        Ok(Self { pool, config })
    }

    /// Build a `Database` from an existing pool (used in tests).
    #[must_use]
    pub const fn from_parts(pool: PgPool, config: DatabaseConfig) -> Self {
        Self { pool, config }
    }

    /// The underlying pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The configuration this handle was built with.
    #[must_use]
    pub const fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    /// Apply all pending migrations.
    ///
    /// Safe to call repeatedly; sqlx tracks applied migrations in
    /// `_sqlx_migrations`.
    pub async fn migrate(&self) -> Result<(), DbError> {
        MIGRATIONS.run(&self.pool).await?;

        // The count, not "up to date".
        //
        // `run` reports nothing about what it did, so the message this used to
        // log -- "migrations up to date" -- was printed identically after
        // applying four migrations and after applying none. That is not a
        // cosmetic problem: it is a log line that cannot distinguish two
        // opposite states, which is how a reader comes to believe a migration
        // is missing when it is not, or present when it is not. It cost real
        // time here on 2026-09-20 while adding 0004.
        //
        // `embedded` against `applied` answers the question the old message
        // pretended to: if they differ, something did not go in, and which one
        // is short is visible without another query.
        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await?;
        let embedded = MIGRATIONS.iter().count();
        if applied == embedded as i64 {
            info!(target: "db", applied, embedded, "migrations up to date");
        } else {
            warn!(
                target: "db",
                applied,
                embedded,
                "the database is not at the embedded migration count"
            );
        }
        Ok(())
    }

    /// Liveness probe for health endpoints (`docs/18-OBSERVABILITY.md`).
    pub async fn health(&self) -> Result<(), DbError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(())
    }

    /// Close the pool, waiting for in-flight work to finish.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
