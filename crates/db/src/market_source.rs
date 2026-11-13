//! Stable identities for provider-specific market history.

use std::fmt;

use crate::error::DbError;

/// Provider-neutral identity for one market-history stream.
///
/// IDs identify both the source and market where necessary, for example
/// `binance_spot`, `bybit_spot`, or `research:vendor_x:dataset_y`. They are
/// persisted with trades and book snapshots so distinct venues are never
/// accidentally combined for the same symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarketDataSource(String);

impl MarketDataSource {
    /// Legacy source assigned to rows written before source identities existed.
    pub const LEGACY: &'static str = "legacy";
    /// Binance spot market-history source.
    pub const BINANCE_SPOT: &'static str = "binance_spot";
    /// Bybit spot market-history source.
    pub const BYBIT_SPOT: &'static str = "bybit_spot";

    /// Create a source ID from a stable, lowercase token.
    ///
    /// IDs are limited to 96 ASCII characters from `[a-z0-9._:-]`, with an
    /// alphanumeric first character, so they can be stored and compared
    /// consistently across collectors and external backtest adapters.
    ///
    /// # Errors
    /// Returns [`DbError::InvalidConfig`] when the ID is empty, too long, or
    /// contains unsupported characters.
    pub fn new(value: impl Into<String>) -> Result<Self, DbError> {
        let value = value.into();
        let valid = value.len() <= 96
            && value
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b':' | b'-')
            });
        if !valid {
            return Err(DbError::InvalidConfig(format!(
                "invalid market-data source ID `{value}`; expected 1-96 lowercase ASCII \
                 characters from [a-z0-9._:-], starting with a letter or digit"
            )));
        }
        Ok(Self(value))
    }

    /// Source ID used for data written before this contract existed.
    #[must_use]
    pub fn legacy() -> Self {
        Self(Self::LEGACY.to_owned())
    }

    /// Binance spot history identity.
    #[must_use]
    pub fn binance_spot() -> Self {
        Self(Self::BINANCE_SPOT.to_owned())
    }

    /// Bybit spot history identity.
    #[must_use]
    pub fn bybit_spot() -> Self {
        Self(Self::BYBIT_SPOT.to_owned())
    }

    /// The stable source ID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MarketDataSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_and_external_source_ids() {
        assert_eq!(
            MarketDataSource::new(MarketDataSource::BINANCE_SPOT)
                .unwrap()
                .as_str(),
            "binance_spot"
        );
        assert_eq!(
            MarketDataSource::new("research:vendor_x:dataset-y")
                .unwrap()
                .as_str(),
            "research:vendor_x:dataset-y"
        );
    }

    #[test]
    fn rejects_noncanonical_source_ids() {
        for invalid in ["", "Binance", ".leading", "a/b", &"a".repeat(97)] {
            assert!(MarketDataSource::new(invalid).is_err(), "{invalid:?}");
        }
    }
}
