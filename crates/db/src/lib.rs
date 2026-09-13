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
pub mod models;

pub use config::DatabaseConfig;
pub use error::DbError;
pub use models::Database;

/// Embedded migrations from `crates/db/migrations`.
///
/// `sqlx::migrate!` reads the directory at **compile time**, so a missing or
/// renamed migration file is a build error rather than a runtime surprise.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
