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
    /// The third anchor's time, for the parity kinds that need one (channel,
    /// arc, triangle). Absent for everything else, same wire rule as `a2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time3_ms: Option<f64>,
    /// The third anchor's price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price3: Option<f64>,
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

/// What the agent states about an object it wants to put on the chart.
///
/// Provenance (`docs/21`) is the difference between an annotation and an
/// intrusion: the user must be able to ask "why is this here?" and get the
/// model's own answer, and to filter AI-drawn objects out of the view without
/// hunting them one by one. The fields mirror the storage columns of
/// `0009_drawing_provenance.sql` one for one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawingProvenance {
    /// The agent's stated confidence in the object, 0..1. Optional because a
    /// rough sketch needs no number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Why the object is being drawn, in the model's own words. This is what
    /// "why did you draw this?" reads before anything is re-asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One drawing the agent wants on the chart.
///
/// The wire shape of a `create_drawing` call. Absolute anchors only: a
/// fraction is a position on a plot and means nothing without the window it
/// was measured in, which is the same rule the HTTP route enforces — the agent
/// gets no shortcut around it because it is a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAgentDrawing {
    /// The kind to draw. Free text here, **validated against the storage's
    /// vocabulary one layer up** — the model must be able to try a kind and
    /// get "that kind does not exist; the kinds are …" back rather than have
    /// a schema enum lie to it about what the deployment can draw.
    pub kind: String,
    /// What to call it. A label the user can read on the chart and in the
    /// object list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// First anchor: milliseconds since the epoch, and the price.
    pub time1_ms: f64,
    /// First anchor's price.
    pub price1: f64,
    /// Second anchor, for the kinds that need two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time2_ms: Option<f64>,
    /// Second anchor's price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price2: Option<f64>,
    /// Third anchor, for the parity kinds that need three (channel, arc,
    /// triangle). The engine's `validate_anchors` is the rule; this struct
    /// only carries what it asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time3_ms: Option<f64>,
    /// Third anchor's price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price3: Option<f64>,
    /// The model's confidence and stated reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<DrawingProvenance>,
}

/// A drawing after storage accepted it.
///
/// The id is what `update_drawing` / `delete_drawing` take, and what the
/// model must quote when it wants to move or remove the object it just drew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredDrawing {
    /// The storage's id for the row.
    pub id: String,
    /// The symbol it landed on, uppercased by storage.
    pub symbol: String,
    /// The kind as stored, echoed so the model can confirm what it drew.
    pub kind: String,
}

/// Where the agent's drawings go, when the host allows writes at all.
///
/// A **separate trait** from [`UserDrawingsSource`], deliberately: read-only
/// is the default posture (`tools.rs`'s header says why), and a host that
/// never attaches a writer makes every write tool report its absence —
/// the same honest-answer rule the read tool follows. Implemented one layer
/// up, against `db::drawings`, with the authenticated user as the opaque key;
/// the agent cannot write into anyone's chart but the asker's.
#[async_trait]
pub trait DrawingWriter: Send + Sync {
    /// Store a new drawing for this user on this symbol.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage refuses — an unknown kind, a
    /// half-populated anchor pair, a non-finite number. The message goes back
    /// to the model, which can correct and retry.
    async fn create(
        &self,
        user_id: &str,
        symbol: &str,
        drawing: &NewAgentDrawing,
    ) -> Result<StoredDrawing, crate::AgentError>;

    /// Move or relabel one of this user's drawings.
    ///
    /// `Ok(false)` means no such drawing for this user — reported as the
    /// neutral fact it is, not an error: the model may be working from a
    /// stale list.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer.
    async fn update(
        &self,
        user_id: &str,
        symbol: &str,
        id: &str,
        drawing: &NewAgentDrawing,
    ) -> Result<bool, crate::AgentError>;

    /// Remove one of this user's drawings.
    ///
    /// `Ok(false)` is likewise not an error: deleting twice leaves the chart
    /// in the state the user asked for.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer.
    async fn delete(
        &self,
        user_id: &str,
        symbol: &str,
        id: &str,
    ) -> Result<bool, crate::AgentError>;
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
            time3_ms: None,
            price3: None,
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
