//! `MarketDataSource` over the unified RAM + venue window.
//!
//! ## Why this adapter lives here and not in a library
//!
//! `ai-agent` cannot depend on `db` (`docs/03`), and `db` should not depend on
//! `ai-agent` -- the storage layer has no business knowing about the agent. So
//! the trait and its implementation sit on opposite sides of a dependency rule
//! and can only be joined from above, which is what this module is.
//!
//! ## Why it is no longer `DbMarketData`
//!
//! It used to read candles out of Postgres. That was the last surviving trace
//! of the assumption that market data is stored, and it produced a real defect:
//! the chart drew a symbol perfectly well from RAM plus the venue while the
//! agent answered "no data" for the same symbol at the same moment, because
//! nobody had happened to backfill that symbol into the database by hand.
//! Two sources of truth for one question, and the stale one was the one the AI
//! was reasoning about.
//!
//! Now both read the same [`WindowService`], so the chart, the bot and the
//! agent cannot disagree about what a candle was.
//!
//! ## What it costs
//!
//! A window deeper than the RAM buffer pays a venue round trip, where the old
//! path paid a query. That is the honest trade: `docs/04` forbids persisting
//! market data, so the alternative was not "a cheap read", it was "a read that
//! works only for symbols someone backfilled". The buffer covers the recent
//! window with no network at all, and `/candles` already reports which part
//! came from where -- this path reports the same accounting through
//! [`Window::provenance`].

use std::sync::Arc;

use analytics_core::types::{Candle, Trade};
use analytics_core::Timeframe;
use async_trait::async_trait;

use market_data::{Window, WindowService};

use ai_agent::{AgentError, MarketDataSource};

/// Reads market data for the agent from RAM plus the venue.
#[derive(Debug, Clone)]
pub struct WindowMarketData {
    windows: WindowService,
}

impl WindowMarketData {
    /// Wrap a window service.
    #[must_use]
    pub fn new(windows: WindowService) -> Self {
        Self { windows }
    }

    /// The window service, for a caller that wants the richer API.
    #[must_use]
    pub fn windows(&self) -> &WindowService {
        &self.windows
    }

    /// Log what a window actually covered.
    ///
    /// Called on every successful read rather than only on a failure, because
    /// the interesting case is not "the read threw" -- it is "the read returned
    /// twelve bars out of three hundred, and nothing said so". The agent
    /// reasons over whatever it is handed, so a thin window it cannot see is a
    /// confident answer built on a sample nobody checked.
    fn note(&self, window: &Window) {
        if window.gaps > 0 || window.source.venue > 0 {
            tracing::debug!(provenance = %window.provenance(), "agent market read");
        }
    }

    /// The error a failed read produces, in the agent's own vocabulary.
    fn unavailable(
        symbol: &str,
        timeframe: impl Into<String>,
        reason: impl Into<String>,
    ) -> AgentError {
        AgentError::DataUnavailable {
            symbol: symbol.into(),
            timeframe: timeframe.into(),
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl MarketDataSource for WindowMarketData {
    async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, AgentError> {
        let window = self
            .windows
            .candles(symbol, timeframe, from_ns, to_ns)
            .await
            .map_err(|e| Self::unavailable(symbol, timeframe.to_string(), e.to_string()))?;
        self.note(&window);
        Ok(window.candles)
    }

    async fn trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, AgentError> {
        // Never an error, and never a venue call: the tape is the only place
        // trades exist (`docs/04`), it is bounded, and a window older than its
        // span legitimately holds none. Returning empty is the honest answer,
        // and `ai-agent` already reports `NO_TICK_DATA` for it rather than
        // reading an empty tape as "the book was balanced".
        Ok(self.windows.trades(symbol, from_ns, to_ns))
    }

    async fn latest_candle_time(
        &self,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Result<Option<i64>, AgentError> {
        let symbol = symbol.to_uppercase();
        // The buffer first, because that is the newest bar this process knows
        // about and it includes the forming one. The venue is only consulted
        // when the buffer has nothing, so the common case costs no network.
        if let Some(newest) = self.windows.history().newest(&symbol, timeframe) {
            return Ok(Some(newest));
        }

        self.windows
            .latest(&symbol, timeframe, 1)
            .await
            .map(|window| window.candles.last().map(|c| c.open_time))
            .map_err(|e| Self::unavailable(&symbol, timeframe.to_string(), e.to_string()))
    }
}

/// The window service as a shared handle, for the gateway to hold.
pub type SharedWindowService = Arc<WindowService>;

#[cfg(test)]
mod tests {
    use super::*;
    use market_data::{BackfillClient, HistoryRegistry, LiveRegistry};

    fn service() -> WindowService {
        WindowService::new(
            Arc::new(HistoryRegistry::new()),
            Arc::new(LiveRegistry::new()),
            BackfillClient::binance(),
        )
    }

    #[tokio::test]
    async fn an_empty_tape_is_an_empty_answer_not_an_error() {
        // The distinction the agent's `NO_TICK_DATA` depends on. A read that
        // errored here would be reported as a failure to fetch, and a read that
        // returned a fabricated trade would be worse: `docs/04` forbids storing
        // trades, so "no ticks in this window" is a fact about the platform's
        // retention, never about the market.
        let data = WindowMarketData::new(service());
        let trades = data
            .trades("BTCUSDT", 0, 60_000_000_000)
            .await
            .expect("an empty tape is not an error");
        assert!(trades.is_empty());
    }

    #[tokio::test]
    async fn the_newest_bar_comes_from_the_buffer_when_it_holds_one() {
        use analytics_core::Candle;

        let service = service();
        service.history().record_closed(&Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: 300_000_000_000,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            buy_volume: 0.6,
            sell_volume: 0.4,
        });

        let data = WindowMarketData::new(service);
        let newest = data
            .latest_candle_time("BTCUSDT", Timeframe::M5)
            .await
            .expect("read");
        assert_eq!(
            newest,
            Some(300_000_000_000),
            "the buffer's bar must win, and no venue call is needed to find it"
        );
    }
}
