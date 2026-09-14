//! `MarketDataSource` over Postgres.
//!
//! ## Why this adapter lives here and not in a library
//!
//! `ai-agent` cannot depend on `db` (`docs/03`), and `db` should not depend on
//! `ai-agent` -- the storage layer has no business knowing about the agent. So
//! the trait and its implementation sit on opposite sides of a dependency rule
//! and can only be joined from above, which is what this module is.
//!
//! `tools/agent-cli` carries the same ~50 lines. That duplication is
//! deliberate but not permanent: if a third consumer appears, promote this to
//! a shared crate rather than copying it again.

use std::sync::Arc;

use analytics_core::types::{Candle, Trade};
use analytics_core::Timeframe;
use async_trait::async_trait;
use db::repositories::{candles_range, load_candles, load_trades};
use db::Database;

use ai_agent::{AgentError, MarketDataSource};

/// Reads market data for the agent out of Postgres.
pub struct DbMarketData {
    db: Arc<Database>,
}

impl DbMarketData {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MarketDataSource for DbMarketData {
    async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, AgentError> {
        load_candles(self.db.pool(), symbol, timeframe, from_ns, to_ns)
            .await
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: timeframe.to_string(),
                reason: e.to_string(),
            })
    }

    async fn trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, AgentError> {
        load_trades(self.db.pool(), symbol, from_ns, to_ns)
            .await
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: "ticks".into(),
                reason: e.to_string(),
            })
    }

    async fn latest_candle_time(
        &self,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Result<Option<i64>, AgentError> {
        candles_range(self.db.pool(), symbol, timeframe)
            .await
            .map(|range| range.map(|(_, latest)| latest))
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: timeframe.to_string(),
                reason: e.to_string(),
            })
    }
}
