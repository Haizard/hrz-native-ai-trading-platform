//! Exchange credentials (`docs/15`).
//!
//! ## Where they come from and where they never go
//!
//! `docs/15` asks for keys to be encrypted at rest, scoped to trading-only
//! permissions, and never logged. The first of those assumes the platform
//! stores them; **this platform does not**. Keys are injected as environment
//! variables by the hosting platform (`docs/17` says exactly that) and are
//! read here at startup. Nothing in the repository, the database or the audit
//! trail ever receives one.
//!
//! That is a real decision rather than an oversight, and it has a cost worth
//! naming: a per-user key typed into a settings page would need encryption with
//! a key we would also have to store, and doing that badly is worse than not
//! offering it. If the product ever needs user-supplied keys, it needs a
//! purpose-built secret store -- not a column with a password in front of it.
//!
//! What is enforced here regardless of where a key came from:
//!
//! * [`ExchangeCredentials`] has a redacting `Debug`, because the single most
//!   likely way a key escapes is `tracing::debug!(?credentials)`;
//! * it is not `Serialize`, so it cannot be put in an audit payload by
//!   accident;
//! * the secret is only reachable through [`ExchangeCredentials::secret`],
//!   which exists solely to sign a request.

use crate::error::ExecutionError;

/// A venue's API key and secret.
pub struct ExchangeCredentials {
    venue: String,
    key: String,
    secret: String,
}

impl ExchangeCredentials {
    /// From parts, for tests and for a future secret-store reader.
    #[must_use]
    pub fn from_parts(venue: &str, key: &str, secret: &str) -> Self {
        Self {
            venue: venue.to_string(),
            key: key.to_string(),
            secret: secret.to_string(),
        }
    }

    /// Read `<VENUE>_API_KEY` and `<VENUE>_API_SECRET` from the environment.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Credentials`] naming the *variables*, never
    /// their contents, when either is missing.
    pub fn from_env(venue: &str) -> Result<Self, ExecutionError> {
        let prefix = venue.to_ascii_uppercase();
        let key_var = format!("{prefix}_API_KEY");
        let secret_var = format!("{prefix}_API_SECRET");

        let key = std::env::var(&key_var).map_err(|_| ExecutionError::Credentials {
            venue: venue.to_string(),
            hint: format!("set {key_var} and {secret_var}"),
        })?;
        let secret = std::env::var(&secret_var).map_err(|_| ExecutionError::Credentials {
            venue: venue.to_string(),
            hint: format!("{key_var} is set but {secret_var} is not"),
        })?;

        if key.is_empty() || secret.is_empty() {
            return Err(ExecutionError::Credentials {
                venue: venue.to_string(),
                hint: format!("{key_var} and {secret_var} must not be empty"),
            });
        }

        Ok(Self {
            venue: venue.to_string(),
            key,
            secret,
        })
    }

    /// The venue these credentials are for.
    #[must_use]
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The public key, sent as a header on every signed request.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The secret, **for signing one request only**.
    ///
    /// Do not log it, do not clone it into a struct that derives `Debug`, do
    /// not put it in an error. Every one of those has happened to somebody.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for ExchangeCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redacted rather than hidden: an engineer debugging a 401 needs to see
        // that credentials *were* loaded, and must not see which ones.
        formatter
            .debug_struct("ExchangeCredentials")
            .field("venue", &self.venue)
            .field("key", &"<redacted>")
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A venue no test could have configured, so `from_env` needs no mutation
    /// of the process environment -- which is global, and therefore unsafe to
    /// touch from one test while another is reading it.
    const ABSENT: &str = "no_such_venue_xyz";

    #[test]
    fn a_debug_line_never_contains_a_secret() {
        // The failure this prevents: `tracing::debug!(?credentials)` putting a
        // live key into a log aggregator.
        let credentials =
            ExchangeCredentials::from_parts("binance", "KEY-abc123", "SECRET-supersecret");
        let rendered = format!("{credentials:?}");

        assert!(rendered.contains("binance"), "{rendered}");
        assert!(!rendered.contains("KEY-abc123"), "{rendered}");
        assert!(!rendered.contains("SECRET-supersecret"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn the_parts_are_reachable_when_signing_needs_them() {
        let credentials = ExchangeCredentials::from_parts("binance", "k", "s");
        assert_eq!(credentials.key(), "k");
        assert_eq!(credentials.secret(), "s");
        assert_eq!(credentials.venue(), "binance");
    }

    #[test]
    fn a_missing_key_names_the_variables_and_not_their_contents() {
        let error = ExchangeCredentials::from_env(ABSENT).expect_err("must be missing");
        let message = error.to_string();
        assert!(message.contains("NO_SUCH_VENUE_XYZ_API_KEY"), "{message}");
        assert!(
            message.contains("NO_SUCH_VENUE_XYZ_API_SECRET"),
            "{message}"
        );
        assert!(message.contains(ABSENT), "{message}");
        assert!(matches!(error, ExecutionError::Credentials { .. }));
    }
}
