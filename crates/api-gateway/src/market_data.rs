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

// The engine's own vocabulary and anchor rule, shared with `drawing_routes`:
// both doors into the drawings table validate the same way.
use chart_engine::{Anchor, Drawing, DrawingKind};

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

/// The asking user's drawings, over the same Postgres the drawing routes use.
///
/// One adapter, two consumers: `POST /agent/ask` and `/ws/agent` both attach it
/// to an [`ai_agent::AskRequest`] with the user they authenticated — so the
/// agent's `get_user_drawings` reads exactly what `GET /drawings` would render,
/// through the same `db::drawings` functions and the same scoping. There is no
/// third reading of a drawing to disagree about.
#[derive(Debug, Clone)]
pub struct DbUserDrawings {
    db: Arc<db::Database>,
}

impl DbUserDrawings {
    /// Wrap the shared database handle.
    #[must_use]
    pub fn new(db: Arc<db::Database>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ai_agent::UserDrawingsSource for DbUserDrawings {
    async fn drawings(
        &self,
        user_id: &str,
        symbol: &str,
    ) -> Result<Vec<ai_agent::UserDrawing>, AgentError> {
        // The id arrives as the authenticated identity's string form; a value
        // that does not parse cannot belong to any user, which is an empty
        // answer rather than a lookup with a fabricated key.
        let Ok(id) = uuid::Uuid::parse_str(user_id) else {
            return Ok(Vec::new());
        };
        let symbol = symbol.to_uppercase();
        let rows = db::drawings::list_drawings(self.db.pool(), id, &symbol)
            .await
            .map_err(|e| AgentError::ToolFailed {
                tool: "get_user_drawings".into(),
                reason: format!("storage could not answer: {e}"),
            })?;

        Ok(rows
            .iter()
            .map(|row| ai_agent::UserDrawing {
                kind: row.kind.clone(),
                label: row.label.clone(),
                time1_ms: row.a1_time_ms,
                price1: row.a1_price,
                // The CHECK keeps the pair whole, so a half-populated one is a
                // row edited by hand; reporting it as absent is the honest
                // reading, the same one `drawing_routes::describe` gives it.
                time2_ms: row.a2_time_ms,
                price2: row.a2_price,
            })
            .collect())
    }
}

/// The [`DrawingKind`] a stored name refers to — the route's own mapping,
/// reused rather than copied.
fn kind_from_name(name: &str) -> Option<DrawingKind> {
    DrawingKind::ALL
        .into_iter()
        .find(|kind| kind.name() == name)
}

/// The agent's write path, over the same Postgres the drawing routes use.
///
/// ## Why the engine's validator runs here
///
/// The HTTP route validates an incoming drawing with
/// `chart_engine::Drawing::validate_anchors` before storing it, and this
/// adapter is a **second door into the same table**. A door that skipped the
/// check would let the model store a shape the chart refuses to draw, and
/// the disagreement would surface as a drawing the user cannot see but the
/// agent believes exists. So the same enum's own rule is applied here, not a
/// copy of it — one vocabulary, one anchor rule, two callers.
///
/// Provenance is stamped `created_by = "ai"` unconditionally: this adapter
/// exists only for the agent, so a caller cannot forge human provenance by
/// going through it (`0009_drawing_provenance.sql`).
#[derive(Debug, Clone)]
pub struct DbDrawingWriter {
    db: Arc<db::Database>,
}

impl DbDrawingWriter {
    /// Wrap the shared database handle.
    #[must_use]
    pub fn new(db: Arc<db::Database>) -> Self {
        Self { db }
    }

    /// The kinds the storage accepts, for an error the model can act on.
    fn valid_kinds() -> String {
        db::drawings::KINDS.join(", ")
    }

    /// The engine's anchor rule, applied to what the model sent.
    fn validate(drawing: &ai_agent::NewAgentDrawing) -> Result<(), AgentError> {
        const TOOL: &str = "create_drawing";
        let unknown = || AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: format!(
                "`{}` is not a drawing kind. The kinds are: {}.",
                drawing.kind,
                Self::valid_kinds()
            ),
        };
        if !db::drawings::KINDS.contains(&drawing.kind.as_str()) {
            return Err(unknown());
        }
        let second = match (drawing.time2_ms, drawing.price2) {
            (Some(time), Some(price)) => Some(Anchor::Absolute { time, price }),
            (None, None) => None,
            // `new_drawing_args` already refuses the half pair; this is the
            // same rule stated where the write happens, because the adapter
            // is a public door and not every caller goes through the tool.
            _ => {
                return Err(AgentError::InvalidToolArgs {
                    tool: TOOL.into(),
                    reason: "a second anchor needs both time and price".into(),
                })
            }
        };
        let check = Drawing {
            id: String::new(),
            kind: kind_from_name(&drawing.kind).ok_or_else(unknown)?,
            a1: Anchor::Absolute {
                time: drawing.time1_ms,
                price: drawing.price1,
            },
            a2: second,
            label: drawing.label.clone(),
            selected: false,
        };
        check
            .validate_anchors()
            .map_err(|reason| AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason,
            })
    }

    /// The storage row from what the model sent, provenance stamped.
    fn to_new(drawing: &ai_agent::NewAgentDrawing) -> db::drawings::NewDrawing {
        db::drawings::NewDrawing {
            kind: drawing.kind.clone(),
            a1_time_ms: drawing.time1_ms,
            a1_price: drawing.price1,
            a2_time_ms: drawing.time2_ms,
            a2_price: drawing.price2,
            label: drawing.label.clone(),
            provenance: Some(db::drawings::Provenance {
                created_by: "ai".into(),
                agent: Some("agent".into()),
                confidence: drawing.provenance.as_ref().and_then(|p| p.confidence),
                reason: drawing.provenance.as_ref().and_then(|p| p.reason.clone()),
            }),
        }
    }
}

#[async_trait]
impl ai_agent::DrawingWriter for DbDrawingWriter {
    async fn create(
        &self,
        user_id: &str,
        symbol: &str,
        drawing: &ai_agent::NewAgentDrawing,
    ) -> Result<ai_agent::StoredDrawing, AgentError> {
        const TOOL: &str = "create_drawing";
        Self::validate(drawing)?;
        let Ok(id) = uuid::Uuid::parse_str(user_id) else {
            return Err(AgentError::ToolFailed {
                tool: TOOL.into(),
                reason: "the request identity does not name a user".into(),
            });
        };
        let stored_id =
            db::drawings::create_drawing(self.db.pool(), id, symbol, &Self::to_new(drawing))
                .await
                .map_err(|e| AgentError::ToolFailed {
                    tool: TOOL.into(),
                    reason: format!("storage refused the drawing: {e}"),
                })?;
        Ok(ai_agent::StoredDrawing {
            id: stored_id.to_string(),
            symbol: symbol.to_uppercase(),
            kind: drawing.kind.clone(),
        })
    }

    async fn update(
        &self,
        user_id: &str,
        _symbol: &str,
        id: &str,
        drawing: &ai_agent::NewAgentDrawing,
    ) -> Result<bool, AgentError> {
        Self::validate(drawing)?;
        let (Some(uid), Some(row_id)) = (
            uuid::Uuid::parse_str(user_id).ok(),
            uuid::Uuid::parse_str(id).ok(),
        ) else {
            return Ok(false);
        };
        db::drawings::update_drawing(self.db.pool(), uid, row_id, &Self::to_new(drawing))
            .await
            .map_err(|e| AgentError::ToolFailed {
                tool: "update_drawing".into(),
                reason: format!("storage could not update the drawing: {e}"),
            })
    }

    async fn delete(&self, user_id: &str, _symbol: &str, id: &str) -> Result<bool, AgentError> {
        let (Some(uid), Some(row_id)) = (
            uuid::Uuid::parse_str(user_id).ok(),
            uuid::Uuid::parse_str(id).ok(),
        ) else {
            return Ok(false);
        };
        db::drawings::delete_drawing(self.db.pool(), uid, row_id)
            .await
            .map_err(|e| AgentError::ToolFailed {
                tool: "delete_drawing".into(),
                reason: format!("storage could not delete the drawing: {e}"),
            })
    }
}

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
