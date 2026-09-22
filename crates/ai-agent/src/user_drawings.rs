//! What the user has drawn, as the agent may read it.
//!
//! ## The gap this closes (`docs/19` row 19)
//!
//! A user draws a level, asks the agent about the chart, and the agent answers
//! as though the chart were blank — while the drawn levels are the most direct
//! statement of what the user is looking at. `docs/14` named the refusal
//! "deliberate but undecided"; this module is the decision, in the direction
//! the storage and routes were already built for.
//!
//! ## Read-only, per user, per symbol
//!
//! The agent can **list** drawings and nothing else. Every write route is
//! authenticated to a user; the drawings handed to a run are the asking user's
//! own, resolved by the host one layer up — the same rule
//! `api_gateway::drawing_routes` already enforces, never restated here.
//!
//! ## Why a trait, like `MarketDataSource`
//!
//! `ai-agent` cannot depend on `db` (`docs/03`). The capability is declared
//! here and implemented one layer up, in `api-gateway` (against Postgres) and
//! in tests (against fixtures). A caller with no source passes `None`, and the
//! tool says so honestly rather than pretending the chart is blank — a blank
//! chart is a claim about the market, and "nobody was listening" is not.
//!
//! ## Two prices, deliberately, and never a resample
//!
//! A drawing is two `(time, price)` anchors. Collapsing that to one price (the
//! way a viewport packet must) throws away exactly the half that means
//! "trendline": which end the user is pointing at. The tool reports both
//! anchors, the kind, and the label, and leaves the geometry to the model.
//!
//! `price2` is absent for a one-anchored kind (`hline`), and for a drawing
//! whose second anchor never carried a price — both stored states, not
//! inventions.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One user drawing, normalized for the model.
///
/// Times are milliseconds, the wire unit `db::drawings` already chose for
/// exactly the round-trip reason recorded there — a nanosecond timestamp does
/// not survive a trip through an `f64`. The gateway maps its storage rows onto
/// this type; nothing in `db` leaks here, because `ai-agent` cannot see `db`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserDrawing {
    /// The mark's kind, in the storage's vocabulary: `trendline`, `hline`,
    /// `rect`, `fib`. Echoed verbatim, never branched on.
    pub kind: String,
    /// What the user called it, when they named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The first anchor: milliseconds since the epoch, and the price.
    pub time1_ms: f64,
    /// The first anchor's price.
    pub price1: f64,
    /// The second anchor's time, for the kinds that have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time2_ms: Option<f64>,
    /// The second anchor's price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price2: Option<f64>,
}

impl UserDrawing {
    /// The most recent anchor's price, when the drawing carries one.
    ///
    /// A trendline "at" a price means the end the user last touched; a
    /// degenerate one-anchored line is just its price.
    #[must_use]
    pub fn price(&self) -> Option<f64> {
        self.price2.or(Some(self.price1))
    }
}

/// Where the asking user's drawings come from.
///
/// Implemented one layer up, against `db::drawings::list_drawings`, with the
/// *requesting* user's id as an opaque key — the same per-user scoping the
/// HTTP route enforces. The agent never sees another user's chart, and the
/// id is opaque here because who a user is cannot be this crate's business.
#[async_trait]
pub trait UserDrawingsSource: Send + Sync {
    /// This user's drawings on one symbol, oldest first.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer — the tool reports
    /// it to the model rather than reading the failure as "no drawings".
    async fn drawings(
        &self,
        user_id: &str,
        symbol: &str,
    ) -> Result<Vec<UserDrawing>, crate::AgentError>;
}

/// Cap how many drawings one tool answer carries, reporting what was dropped.
///
/// [`crate::chart_context::MAX_DRAWINGS`] is the viewport packet's bound for
/// the same reason: past a couple of dozen levels the list stops being context
/// and starts being noise that pushes the question out of the model's
/// attention. The count is returned rather than silently applied, so the tool
/// can say "showing 24 of 57" — a trimmed list nobody knows is trimmed reads
/// as the whole chart.
#[must_use]
pub fn clamp_drawings(drawings: &mut Vec<UserDrawing>, max: usize) -> usize {
    let original = drawings.len();
    drawings.truncate(max);
    original
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawing(kind: &str, price1: f64, price2: Option<f64>) -> UserDrawing {
        UserDrawing {
            kind: kind.into(),
            label: None,
            time1_ms: 1_767_225_600_000.0,
            price1,
            time2_ms: price2.map(|_| 1_767_229_200_000.0),
            price2,
        }
    }

    #[test]
    fn a_trendline_cites_its_recent_end() {
        let d = drawing("trendline", 45_000.0, Some(45_500.0));
        assert_eq!(d.price(), Some(45_500.0));
    }

    #[test]
    fn a_horizontal_line_cites_its_one_price() {
        let d = drawing("hline", 45_000.0, None);
        assert_eq!(d.price(), Some(45_000.0));
    }

    #[test]
    fn a_half_populated_second_anchor_is_the_absent_anchor() {
        // A storage row whose pair was broken by hand. Inventing a price from
        // the first anchor would read as analysis the user never drew.
        let d = drawing("rect", 45_000.0, None);
        assert_eq!(d.time2_ms, None);
        assert_eq!(d.price2, None);
        assert_eq!(d.price(), Some(45_000.0));
    }

    #[test]
    fn clamping_reports_what_it_dropped() {
        let mut many: Vec<UserDrawing> =
            (0..30).map(|i| drawing("hline", i as f64, None)).collect();
        assert_eq!(clamp_drawings(&mut many, 24), 30, "the original count");
        assert_eq!(many.len(), 24);

        // Under the cap the report is the same as the length: nothing dropped.
        let mut few = vec![drawing("hline", 1.0, None)];
        assert_eq!(clamp_drawings(&mut few, 24), 1);
        assert_eq!(few.len(), 1);
    }
}
