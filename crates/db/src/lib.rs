//! # `db`
//!
//! PostgreSQL access layer: connection pool configuration, embedded migrations
//! and (from Phase 1) the query models.
//!
//! ## Connection
//!
//! The database is a managed PostgreSQL instance (Northflank). The only thing
//! this crate needs is a `DATABASE_URL`; nothing about the host is hardcoded.
//! Load it from the environment (see `.env.example`):
//!
//! ```text
//! DATABASE_URL=postgres://user:password@host:5432/ai_trading?sslmode=require
//! ```
//!
//! ## Status
//!
//! Phase 0: pool wired and health-checkable, not yet used by any feature code.

#![deny(missing_docs)]

pub mod config;
pub mod error;
pub mod loading;
pub mod models;
pub mod paper;
pub mod repositories;
pub mod strategies;
pub mod users;

pub use config::DatabaseConfig;
pub use error::DbError;
pub use loading::{coverage, load_timeframe_series, warn_about_short_series, MIN_COVERAGE};
pub use models::Database;
pub use paper::{
    bot_summary, count_audit_events, count_executed_trades, create_owner, find_or_create_strategy,
    find_user_by_email, insert_audit_events, insert_bot, insert_executed_trades, purge_bot,
    purge_owner, purge_owners_with_prefix, recent_decisions, set_bot_status, AuditEvent,
    BotSummary, ExecutedTrade,
};
pub use repositories::{dt_to_ns, ns_to_dt};
pub use strategies::{
    create_backtest, create_strategy, delete_strategy, get_backtest, get_strategy, list_backtests,
    list_strategies, BacktestRow, StrategyRow,
};
pub use users::{create_user, delete_user, find_by_email, find_by_id, normalize_email, UserRow};

/// Embedded migrations from `crates/db/migrations`.
///
/// `sqlx::migrate!` reads the directory at **compile time**, so a missing or
/// renamed migration file is a build error rather than a runtime surprise.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
