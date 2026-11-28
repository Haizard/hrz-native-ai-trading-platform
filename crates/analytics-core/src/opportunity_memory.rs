//! Market Opportunity Memory: recall comparable past opportunities.
//!
//! This is the deterministic core of the memory: it stores compact
//! representations and later outcomes, and recalls similar opportunities by
//! signature distance. Persistence belongs to the gateway, exactly as with the
//! outcome ledger; nothing here reads a database or a clock.

use serde::{Deserialize, Serialize};

use crate::hierarchy::Direction;
use crate::neuro_market::{similarity, MarketRepresentation};
use crate::types::Timeframe;

/// A stored opportunity and its eventual outcome, when known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RememberedOpportunity {
    /// Stable local id.
    pub id: u64,
    /// Instrument.
    pub symbol: String,
    /// Decision timeframe.
    pub timeframe: Timeframe,
    /// Direction at decision time.
    pub direction: Direction,
    /// Compact representation signature.
    pub signature: u8,
    /// Evidence strength; not a probability.
    pub evidence_strength: f64,
    /// Forward return once the horizon matured.
    pub realized_return: Option<f64>,
    /// Whether the directional call was correct.
    pub hit: Option<bool>,
}

/// In-memory opportunity store.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpportunityMemory {
    next_id: u64,
    entries: Vec<RememberedOpportunity>,
}

impl OpportunityMemory {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store one opportunity. The signature comes only from completed data.
    pub fn record(
        &mut self,
        symbol: &str,
        representation: &MarketRepresentation,
        direction: Direction,
        evidence_strength: f64,
    ) -> Result<u64, String> {
        if !evidence_strength.is_finite() || !(0.0..=1.0).contains(&evidence_strength) {
            return Err("evidence strength must be finite and within [0, 1]".to_string());
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.entries.push(RememberedOpportunity {
            id,
            symbol: symbol.to_string(),
            timeframe: representation.timeframe,
            direction,
            signature: representation.signature,
            evidence_strength,
            realized_return: None,
            hit: None,
        });
        Ok(id)
    }

    /// Settle a stored opportunity once.
    pub fn settle(&mut self, id: u64, realized_return: f64, hit: bool) -> Result<(), String> {
        if !realized_return.is_finite() {
            return Err("realized return must be finite".to_string());
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| "unknown opportunity".to_string())?;
        if entry.hit.is_some() {
            return Err("opportunity was already settled".to_string());
        }
        entry.realized_return = Some(realized_return);
        entry.hit = Some(hit);
        Ok(())
    }

    /// Recall the closest signatures, highest similarity first.
    #[must_use]
    pub fn recall(&self, signature: u8, limit: usize) -> Vec<(RememberedOpportunity, f64)> {
        let mut found: Vec<(RememberedOpportunity, f64)> = self
            .entries
            .iter()
            .map(|entry| {
                let probe = MarketRepresentation {
                    timeframe: entry.timeframe,
                    direction_bucket: entry.signature & 0x3,
                    volatility_bucket: (entry.signature >> 2) & 0x7,
                    volume_bucket: (entry.signature >> 5) & 0x3,
                    delta_bucket: (entry.signature >> 7) & 0x1,
                    signature,
                    reconstruction_confidence: 1.0,
                    reconstruction_error: 0.0,
                };
                let stored = MarketRepresentation {
                    signature: entry.signature,
                    ..probe.clone()
                };
                (entry.clone(), similarity(&probe, &stored))
            })
            .collect();
        found.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.id.cmp(&a.0.id)));
        found.truncate(limit);
        found
    }

    /// Reliability of a signature from settled outcomes, withheld for tiny samples.
    #[must_use]
    pub fn reliability(&self, signature: u8, min_samples: usize) -> Option<f64> {
        let settled: Vec<&RememberedOpportunity> = self
            .entries
            .iter()
            .filter(|entry| entry.signature == signature && entry.hit.is_some())
            .collect();
        if settled.len() < min_samples {
            return None;
        }
        let hits = settled
            .iter()
            .filter(|entry| entry.hit == Some(true))
            .count();
        Some(hits as f64 / settled.len() as f64)
    }

    /// Number of stored opportunities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn representation(signature: u8) -> MarketRepresentation {
        MarketRepresentation {
            timeframe: Timeframe::M5,
            direction_bucket: signature & 0x3,
            volatility_bucket: 0,
            volume_bucket: 0,
            delta_bucket: 0,
            signature,
            reconstruction_confidence: 0.9,
            reconstruction_error: 0.1,
        }
    }

    #[test]
    fn an_opportunity_is_recorded_and_settled_once() {
        let mut memory = OpportunityMemory::new();
        let id = memory
            .record("BTCUSDT", &representation(0b1010), Direction::Bullish, 0.7)
            .unwrap();
        assert_eq!(memory.len(), 1);
        assert!(memory.reliability(0b1010, 1).is_none());
        memory.settle(id, 0.01, true).unwrap();
        assert_eq!(memory.reliability(0b1010, 1), Some(1.0));
        assert!(memory.settle(id, -0.01, false).is_err());
    }

    #[test]
    fn recall_orders_by_signature_similarity() {
        let mut memory = OpportunityMemory::new();
        memory
            .record("BTCUSDT", &representation(0b1010), Direction::Bullish, 0.7)
            .unwrap();
        memory
            .record("BTCUSDT", &representation(0b1111), Direction::Bearish, 0.6)
            .unwrap();
        let recalled = memory.recall(0b1010, 2);
        assert_eq!(recalled.len(), 2);
        assert!(recalled[0].1 >= recalled[1].1);
        assert_eq!(recalled[0].0.signature, 0b1010);
    }

    #[test]
    fn invalid_records_are_refused() {
        let mut memory = OpportunityMemory::new();
        assert!(memory
            .record("BTCUSDT", &representation(1), Direction::Neutral, 2.0)
            .is_err());
        assert!(memory.settle(99, 0.0, true).is_err());
    }
}
