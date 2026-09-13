//! Database configuration, loaded from the environment.

use std::time::Duration;

use crate::error::DbError;

/// Default maximum size of the connection pool.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;

/// Default timeout when acquiring a connection from the pool.
pub const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 10;

/// Default threshold above which a statement is logged as slow, in milliseconds.
///
/// sqlx's own default is one second. That is calibrated for a local database;
/// against a managed instance over the network, a 500-row chunked upsert
/// routinely exceeds it, and **every** slow-statement warning logs the whole
/// statement -- hundreds of kilobytes for a chunked insert. Five seconds keeps
/// the log readable while still surfacing queries that are genuinely slow.
pub const DEFAULT_SLOW_STATEMENT_MS: u64 = 5_000;

/// Everything needed to build a [`sqlx::PgPool`].
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Postgres connection URL (the Northflank connection string).
    pub url: String,
    /// Maximum number of connections in the pool.
    pub max_connections: u32,
    /// How long to wait for a free connection before erroring.
    pub acquire_timeout: Duration,
    /// How long a statement may run before it is logged as slow.
    pub slow_statement_threshold: Duration,
}

impl DatabaseConfig {
    /// Read configuration from the environment.
    ///
    /// Required:
    /// * `DATABASE_URL`
    ///
    /// Optional:
    /// * `DB_MAX_CONNECTIONS` (default 10)
    /// * `DB_ACQUIRE_TIMEOUT_SECS` (default 10)
    /// * `DB_SLOW_STATEMENT_MS` (default 5000)
    pub fn from_env() -> Result<Self, DbError> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| DbError::MissingEnv("DATABASE_URL".to_string()))?;

        if url.trim().is_empty() {
            return Err(DbError::MissingEnv("DATABASE_URL".to_string()));
        }

        let max_connections = parse_env_u32("DB_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
        let acquire_timeout =
            parse_env_u64("DB_ACQUIRE_TIMEOUT_SECS", DEFAULT_ACQUIRE_TIMEOUT_SECS)?;
        let slow_statement_ms = parse_env_u64("DB_SLOW_STATEMENT_MS", DEFAULT_SLOW_STATEMENT_MS)?;

        if max_connections == 0 {
            return Err(DbError::InvalidConfig(
                "DB_MAX_CONNECTIONS must be >= 1".to_string(),
            ));
        }

        Ok(Self {
            url,
            max_connections,
            acquire_timeout: Duration::from_secs(acquire_timeout),
            slow_statement_threshold: Duration::from_millis(slow_statement_ms),
        })
    }

    /// Build a pool from this configuration. Does not connect eagerly -- see
    /// [`Database::connect`](crate::models::Database::connect) for the variant
    /// that verifies connectivity.
    ///
    /// # Errors
    /// Returns [`DbError::Pool`] if the pool cannot be created (usually a
    /// malformed URL).
    pub async fn create_pool(&self) -> Result<sqlx::PgPool, DbError> {
        // `log_slow_statements` lives on the `ConnectOptions` trait, which has to
        // be in scope for the method to resolve.
        use sqlx::ConnectOptions as _;

        let options: sqlx::postgres::PgConnectOptions = self.url.parse()?;
        // sqlx takes the `log` crate's `LevelFilter`, not `tracing`'s, even
        // though `tracing` re-exports the former as `tracing::log::LevelFilter`.
        let options = options.log_slow_statements(
            tracing::log::LevelFilter::Warn,
            self.slow_statement_threshold,
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(self.acquire_timeout)
            .connect_with(options)
            .await?;
        Ok(pool)
    }

    /// A redacted form of the URL, safe to log.
    ///
    /// Credentials never appear in logs (`docs/15-RISK-COMPLIANCE.md`).
    #[must_use]
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }
}

/// Replace the password portion of a Postgres URL with `***`.
fn redact_url(url: &str) -> String {
    match url.find("://") {
        Some(scheme_end) => {
            let after_scheme = &url[scheme_end + 3..];
            match after_scheme.rfind('@') {
                // Everything before the last '@' is userinfo; drop its secret.
                Some(at) => {
                    let userinfo = &after_scheme[..at];
                    let host = &after_scheme[at..];
                    let user = userinfo.split(':').next().unwrap_or("");
                    format!("{}://{}:***{}", &url[..scheme_end], user, host)
                }
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    }
}

fn parse_env_u32(key: &str, default: u32) -> Result<u32, DbError> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .map_err(|_| DbError::InvalidConfig(format!("{key} must be an integer, got `{v}`"))),
        Err(_) => Ok(default),
    }
}

fn parse_env_u64(key: &str, default: u64) -> Result<u64, DbError> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .map_err(|_| DbError::InvalidConfig(format!("{key} must be an integer, got `{v}`"))),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_password_from_url() {
        let redacted = redact_url("postgres://trader:hunter2@db.example.com:5432/ai_trading");
        assert!(redacted.contains("***"));
        assert!(!redacted.contains("hunter2"));
        assert!(redacted.ends_with("@db.example.com:5432/ai_trading"));
    }

    #[test]
    fn redact_is_safe_without_credentials() {
        let url = "postgres://db.example.com:5432/ai_trading";
        assert_eq!(redact_url(url), url);
    }
}
