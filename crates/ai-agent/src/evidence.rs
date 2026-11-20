//! Host-provided Markov evidence journal.
//!
//! `ai-agent` cannot depend on `db`, so the write path is a trait implemented
//! by the gateway. The tool records at source, once, and the host decides
//! whose evidence it is. A missing writer is not an error: the tool still
//! returns the evidence, it just cannot measure it later.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AgentError;

/// Evidence about to be persisted by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvidence {
    /// Instrument the evidence describes.
    pub symbol: String,
    /// Resolution the evidence was computed on.
    pub timeframe: String,
    /// Full serialized `MarkovResult`.
    pub evidence: Value,
    /// Engine version for replay and reliability grouping.
    pub model_version: String,
    /// Forecast horizon in bars.
    pub horizon_bars: Option<f64>,
    /// Observation time, unix nanoseconds.
    pub observed_at: i64,
}

/// Receipt returned by the host after a successful write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReceipt {
    /// Host-side record id.
    pub id: String,
    /// When the host stored it, unix nanoseconds.
    pub created_at: i64,
}

/// A write-only journal for Markov evidence.
#[async_trait]
pub trait MarkovEvidenceWriter: Send + Sync {
    /// Persist one evidence record for an opaque user id.
    async fn record(
        &self,
        user_id: &str,
        evidence: &NewEvidence,
    ) -> Result<EvidenceReceipt, AgentError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_dtos_round_trip_through_json() {
        let evidence = NewEvidence {
            symbol: "BTCUSDT".into(),
            timeframe: "5m".into(),
            evidence: serde_json::json!({"current_regime": "bull_trend"}),
            model_version: "markov-engine-v1".into(),
            horizon_bars: Some(4.0),
            observed_at: 1_700_000_000_000_000_000,
        };
        let encoded = serde_json::to_string(&evidence).unwrap();
        let decoded: NewEvidence = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, evidence);
    }
}
