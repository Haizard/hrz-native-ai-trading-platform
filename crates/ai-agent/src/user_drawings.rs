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
//! ## A list of points, and never a resample
//!
//! A drawing is its kind plus the points the user placed, in order. Collapsing
//! that to one price (the way a viewport packet must) throws away exactly the
//! half that means "trendline": which end the user is pointing at. Nor does it
//! stop at two: a channel is three points and a polyline is as many as the user
//! clicked, so the agent reads and writes a **list**, which is the same object
//! model the chart engine and the drawings table use.
//!
//! The list is also what makes a malformed drawing unspellable. `time2_ms`
//! without `price2` was a real state the old fields allowed, and both layers
//! had a check for it; a point is one object with two numbers, so the half a
//! model can get wrong is a point that is not a number at all, which is caught
//! where the argument is read.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One point of a drawing, in the two units the chart is measured in.
///
/// The key names are the ones the pattern tool's `anchors` already use, so a
/// model has one spelling for "a place on the chart" rather than two:
/// `{"time_ms": …, "price": …}`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AgentPoint {
    /// Milliseconds since the epoch.
    pub time_ms: f64,
    /// The price.
    pub price: f64,
}

impl AgentPoint {
    /// A point, for the hosts and tests that build a drawing by hand.
    #[must_use]
    pub const fn at(time_ms: f64, price: f64) -> Self {
        Self { time_ms, price }
    }
}

/// One user drawing, normalized for the model.
///
/// Times are milliseconds, the wire unit `db::drawings` already chose for
/// exactly the round-trip reason recorded there — a nanosecond timestamp does
/// not survive a trip through an `f64`. The gateway maps its storage rows onto
/// this type; nothing in `db` leaks here, because `ai-agent` cannot see `db`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserDrawing {
    /// The storage's id, when the source carries one.
    ///
    /// Snapshots freeze drawings, and the compare tool's added/removed diff is
    /// only as good as the identity it compares by: with an id the diff is
    /// exact, without one it falls back to the kind-and-points signature.
    /// `None` for sources that have no ids (tests, hand-built lists).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    /// The mark's kind, in the storage's vocabulary: `trendline`, `hline`,
    /// `rect`, `fib`. Echoed verbatim, never branched on.
    pub kind: String,
    /// What the user called it, when they named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The points, in the order they were placed: one for a level, two for a
    /// segment, three for a channel, however many for a polyline.
    pub points: Vec<AgentPoint>,
}

impl UserDrawing {
    /// The last point's price, when there is a point to read.
    ///
    /// A trendline "at" a price means the end the user last touched, and a
    /// one-point level is just its price. This is the number that folds into the
    /// grounding range, so it has to be a real price of the drawing rather than
    /// whichever point a mapping happened to name first.
    #[must_use]
    pub fn price(&self) -> Option<f64> {
        self.points.last().map(|point| point.price)
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
/// The wire shape of a `create_drawing` call. Absolute points only: a screen
/// fraction means nothing without the window it was measured in, which is the
/// same rule the HTTP route enforces — the agent gets no shortcut around it
/// because it is a model.
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
    /// The points, in placement order. How many a kind takes is the engine's
    /// rule (`DrawingKind::anchors`), applied one layer up where the write
    /// happens; this struct carries what the model asked for and does not
    /// second-guess it, because a second copy of the count is how the two
    /// vocabularies drift apart.
    pub points: Vec<AgentPoint>,
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
    /// point count the kind does not take, a non-finite number. The message
    /// goes back to the model, which can correct and retry.
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

    fn drawing(kind: &str, points: &[f64]) -> UserDrawing {
        UserDrawing {
            id: None,
            kind: kind.into(),
            label: None,
            points: points
                .iter()
                .enumerate()
                .map(|(index, price)| AgentPoint::at(1_767_225_600_000.0 + index as f64, *price))
                .collect(),
        }
    }

    #[test]
    fn a_trendline_cites_its_recent_end() {
        let d = drawing("trendline", &[45_000.0, 45_500.0]);
        assert_eq!(d.price(), Some(45_500.0));
    }

    #[test]
    fn a_horizontal_line_cites_its_one_price() {
        let d = drawing("hline", &[45_000.0]);
        assert_eq!(d.price(), Some(45_000.0));
    }

    #[test]
    fn a_many_point_drawing_cites_the_last_point_it_was_given() {
        // The rule "the end the user last touched" generalised: a five-point
        // polyline's price is its fifth point, not its first and not their mean.
        // This is the number the grounding range walks, so a wrong pick here
        // shifts what the thesis is allowed to cite.
        let d = drawing("polyline", &[1.0, 2.0, 3.0, 4.0, 46_000.0]);
        assert_eq!(d.points.len(), 5);
        assert_eq!(d.price(), Some(46_000.0));
    }

    #[test]
    fn a_drawing_with_no_points_has_no_price_rather_than_a_zero() {
        // Sources are built by hosts, and an empty list is a thing a host can
        // hand over. `Some(0.0)` here would be a price the user never drew,
        // grounding a thesis against a level that does not exist.
        assert_eq!(drawing("trendline", &[]).price(), None);
    }

    #[test]
    fn a_point_that_is_half_a_point_does_not_deserialize() {
        // The wire is why the old half-anchor checks could go. A `time2_ms`
        // without a `price2` was a value the previous shape could spell and both
        // layers had to refuse; a point is one object with two required numbers,
        // so the incomplete version is simply not this type.
        let missing_price: Result<UserDrawing, _> = serde_json::from_str(
            r#"{"kind":"trendline","points":[{"time_ms":1767225600000}]}"#,
        );
        assert!(missing_price.is_err());

        let ok: UserDrawing = serde_json::from_str(
            r#"{"kind":"hline","points":[{"time_ms":1767225600000,"price":45000}]}"#,
        )
        .expect("a whole point parses");
        assert_eq!(ok.points, vec![AgentPoint::at(1_767_225_600_000.0, 45_000.0)]);
    }

    #[test]
    fn clamping_reports_what_it_dropped() {
        let mut many: Vec<UserDrawing> = (0..30)
            .map(|i| drawing("hline", &[i as f64]))
            .collect();
        assert_eq!(clamp_drawings(&mut many, 24), 30, "the original count");
        assert_eq!(many.len(), 24);

        // Under the cap the report is the same as the length: nothing dropped.
        let mut few = vec![drawing("hline", &[1.0])];
        assert_eq!(clamp_drawings(&mut few, 24), 1);
        assert_eq!(few.len(), 1);
    }
}
