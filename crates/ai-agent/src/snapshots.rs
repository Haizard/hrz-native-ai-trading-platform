//! Chart snapshots: what the chart showed at a moment, kept (`docs/45`).
//!
//! ## Why a trait, like the drawings store
//!
//! `ai-agent` cannot depend on `db` (`docs/03`). The snapshot capability is
//! declared here and implemented one layer up -- `api-gateway` against
//! Postgres, tests against fixtures -- with the asking user's id as an opaque
//! key, exactly like [`crate::user_drawings`]. A host that attaches no store
//! makes the tools report "no snapshot store is attached" rather than pretend
//! a capture happened: a snapshot the model believes exists and nothing can
//! retrieve is worse than a refusal the model can report.
//!
//! ## What a snapshot carries
//!
//! A frozen copy of the chart's state: the last close, the user's drawings on
//! the symbol, and a structure digest -- as JSON values, not typed rows,
//! because a snapshot is a *point-in-time copy* of state whose homes are
//! elsewhere. Typed columns would claim the snapshot tracks its sources; the
//! whole point is that it does not. `compare_snapshots` is the reader that
//! turns two frozen copies into "what changed".

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A snapshot after storage accepted it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSnapshot {
    /// The storage's id for the row.
    pub id: String,
    /// The instrument, uppercase.
    pub symbol: String,
    /// The timeframe the chart was showing.
    pub timeframe: String,
    /// Last close at capture.
    pub price: f64,
    /// The user's drawings on the symbol, frozen as the tool reported them.
    pub drawings: serde_json::Value,
    /// The structure digest at capture: trend, swings, the recent few named.
    pub structure: serde_json::Value,
    /// What the capturer said about it, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Retrieval tags.
    pub tags: Vec<String>,
    /// `user` or `ai`.
    pub created_by: String,
    /// Unix nanoseconds.
    pub created_at: i64,
}

/// A snapshot about to be written.
///
/// Separate from [`ChartSnapshot`] because a row has an id and a timestamp the
/// caller does not supply, and because the capture site -- the tool -- is a
/// deliberate construction: it assembles the digest from tools it just ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSnapshot {
    /// The instrument.
    pub symbol: String,
    /// The timeframe.
    pub timeframe: String,
    /// Last close at capture.
    pub price: f64,
    /// Frozen drawings.
    pub drawings: serde_json::Value,
    /// Frozen structure digest.
    pub structure: serde_json::Value,
    /// The capturer's note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Retrieval tags.
    pub tags: Vec<String>,
    /// `user` or `ai`.
    pub created_by: String,
}

/// Where snapshots live, when the host allows them at all.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Write a snapshot for this user.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage refuses. The message goes back to
    /// the model rather than being read as "captured".
    async fn capture(
        &self,
        user_id: &str,
        snapshot: &NewSnapshot,
    ) -> Result<ChartSnapshot, crate::AgentError>;

    /// One snapshot, when it belongs to this user.
    ///
    /// `Ok(None)` means no such snapshot for this user -- the neutral fact,
    /// not an error: the model may be working from a stale list.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer.
    async fn get(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<ChartSnapshot>, crate::AgentError>;

    /// This user's snapshots of a symbol, newest first, capped.
    ///
    /// # Errors
    /// Any [`crate::AgentError`] when storage cannot answer.
    async fn list(
        &self,
        user_id: &str,
        symbol: &str,
        limit: usize,
    ) -> Result<Vec<ChartSnapshot>, crate::AgentError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_shape_pins_the_keys_the_tools_read() {
        // The compare tool diffs `drawings` and `structure` by key, and the
        // route serializes the same shape. A rename here is a compile error
        // nowhere and a diff that silently reports nothing.
        let snapshot = ChartSnapshot {
            id: "8f3a".into(),
            symbol: "BTCUSDT".into(),
            timeframe: "1h".into(),
            price: 84_250.0,
            drawings: serde_json::json!([]),
            structure: serde_json::json!({"trend": "up"}),
            note: Some("before the open".into()),
            tags: vec!["ny-open".into()],
            created_by: "ai".into(),
            created_at: 1_700_000_000_000_000_000,
        };
        let wire = serde_json::to_value(&snapshot).expect("serializes");
        for key in [
            "id",
            "symbol",
            "timeframe",
            "price",
            "drawings",
            "structure",
            "note",
            "tags",
            "created_by",
            "created_at",
        ] {
            assert!(wire.get(key).is_some(), "the wire lost `{key}`: {wire}");
        }
    }
}
