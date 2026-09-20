//! The safe drawing contract for generated indicator revisions.
//!
//! A generated module names market-time/price evidence; [`crate::scene`] is the
//! only code that turns it into pixels. This prevents both the generated module
//! and the browser shell from owning a second price/time transform.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Hard cap on primitive outputs from one generated revision in one frame.
///
/// The number is deliberately a rendering limit rather than an execution limit:
/// an otherwise valid module must not be able to make the chart unusable by
/// returning a label for every tick in a long history.
pub const MAX_PRIMITIVES: usize = 500;

/// All visual evidence produced by one validated indicator revision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorOutput {
    /// Immutable source revision that produced the visuals.
    pub revision_id: String,
    /// Named evidence nodes. A connector may only reference nodes in this list.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// Stateful zones, such as FVGs or order blocks.
    #[serde(default)]
    pub zones: Vec<IndicatorZone>,
    /// Point-in-time evidence, such as a liquidity sweep or CHoCH.
    #[serde(default)]
    pub markers: Vec<IndicatorMarker>,
    /// Logical links between evidence nodes.
    #[serde(default)]
    pub links: Vec<EvidenceLink>,
}

impl IndicatorOutput {
    /// Validate untrusted generated output before it reaches a chart scene.
    ///
    /// # Errors
    /// Returns a human-readable refusal that can be shown in the workspace chat.
    pub fn validate(&self) -> Result<(), String> {
        non_empty("revision_id", &self.revision_id)?;
        let total = self.evidence.len() + self.zones.len() + self.markers.len() + self.links.len();
        if total > MAX_PRIMITIVES {
            return Err(format!(
                "indicator output has {total} primitives; the chart limit is {MAX_PRIMITIVES}"
            ));
        }

        let mut ids = BTreeSet::new();
        for evidence in &self.evidence {
            validate_evidence(evidence)?;
            if !ids.insert(evidence.id.as_str()) {
                return Err(format!("evidence id `{}` is duplicated", evidence.id));
            }
        }
        for zone in &self.zones {
            non_empty("zone.id", &zone.id)?;
            non_empty("zone.label", &zone.label)?;
            finite("zone.start_time", zone.start_time as f64)?;
            finite("zone.end_time", zone.end_time as f64)?;
            finite("zone.price_low", zone.price_low)?;
            finite("zone.price_high", zone.price_high)?;
            if zone.end_time < zone.start_time {
                return Err(format!("zone `{}` ends before it starts", zone.id));
            }
            if zone.price_low >= zone.price_high {
                return Err(format!(
                    "zone `{}` needs price_low below price_high",
                    zone.id
                ));
            }
        }
        for marker in &self.markers {
            non_empty("marker.id", &marker.id)?;
            non_empty("marker.label", &marker.label)?;
            finite("marker.time", marker.time as f64)?;
            finite("marker.price", marker.price)?;
            if !ids.contains(marker.evidence_id.as_str()) {
                return Err(format!(
                    "marker `{}` references missing evidence `{}`",
                    marker.id, marker.evidence_id
                ));
            }
        }
        for link in &self.links {
            non_empty("link.id", &link.id)?;
            if link.from == link.to {
                return Err(format!("link `{}` cannot point to itself", link.id));
            }
            for endpoint in [&link.from, &link.to] {
                if !ids.contains(endpoint.as_str()) {
                    return Err(format!(
                        "link `{}` references missing evidence `{endpoint}`",
                        link.id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// One named reason a generated indicator reached a conclusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// Stable id within this output.
    pub id: String,
    /// Event name, for example `liquidity_sweep` or `bullish_choch`.
    pub event: String,
    /// Candle time in nanoseconds since the epoch.
    pub time: i64,
    /// Price where the event occurred.
    pub price: f64,
    /// Client-readable explanation of the fulfilled rule.
    pub explanation: String,
}

/// A stateful band produced by an indicator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorZone {
    /// Stable id within this output.
    pub id: String,
    /// Time in nanoseconds at the zone's left edge.
    pub start_time: i64,
    /// Time in nanoseconds at the zone's right edge.
    pub end_time: i64,
    /// Lower boundary of the zone.
    pub price_low: f64,
    /// Upper boundary of the zone.
    pub price_high: f64,
    /// Client-readable zone name.
    pub label: String,
    /// Its latest market lifecycle state.
    pub state: ZoneState,
}

/// A zone's market lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneState {
    /// The pattern formed on this candle.
    Created,
    /// The zone has not yet been mitigated.
    Active,
    /// Price entered the zone.
    Tapped,
    /// Price mitigated the zone.
    Mitigated,
    /// The pattern's invalidation condition fired.
    Invalidated,
}

/// A point marker that represents one evidence node on the chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorMarker {
    /// Stable id within this output.
    pub id: String,
    /// Evidence node this makes visible.
    pub evidence_id: String,
    /// Candle time in nanoseconds since the epoch.
    pub time: i64,
    /// Price where the marker belongs.
    pub price: f64,
    /// Short, chart-readable label.
    pub label: String,
    /// Semantic marker style.
    pub kind: MarkerKind,
}

/// The marker vocabulary understood by the chart's visual language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    /// A bullish structural or setup event.
    Bullish,
    /// A bearish structural or setup event.
    Bearish,
    /// Context that is neither bullish nor bearish by itself.
    Context,
    /// A final setup or confirmation signal.
    Signal,
}

/// A causal edge in the evidence graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLink {
    /// Stable id within this output.
    pub id: String,
    /// Earlier evidence id.
    pub from: String,
    /// Later evidence id.
    pub to: String,
}

fn validate_evidence(evidence: &Evidence) -> Result<(), String> {
    non_empty("evidence.id", &evidence.id)?;
    non_empty("evidence.event", &evidence.event)?;
    non_empty("evidence.explanation", &evidence.explanation)?;
    finite("evidence.time", evidence.time as f64)?;
    finite("evidence.price", evidence.price)
}

fn non_empty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(())
}

fn finite(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be finite"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> IndicatorOutput {
        IndicatorOutput {
            revision_id: "revision-7".into(),
            evidence: vec![Evidence {
                id: "sweep".into(),
                event: "liquidity_sweep".into(),
                time: 1_700_000_000_000_000_000,
                price: 100.0,
                explanation: "Price swept the prior low.".into(),
            }],
            zones: vec![],
            markers: vec![IndicatorMarker {
                id: "sweep-marker".into(),
                evidence_id: "sweep".into(),
                time: 1_700_000_000_000_000_000,
                price: 100.0,
                label: "Sweep".into(),
                kind: MarkerKind::Context,
            }],
            links: vec![],
        }
    }

    #[test]
    fn accepts_a_bounded_evidence_graph() {
        output().validate().expect("valid output");
    }

    #[test]
    fn refuses_a_marker_without_its_evidence() {
        let mut value = output();
        value.markers[0].evidence_id = "missing".into();
        assert!(value.validate().unwrap_err().contains("missing evidence"));
    }

    #[test]
    fn refuses_inside_out_zones() {
        let mut value = output();
        value.zones.push(IndicatorZone {
            id: "gap".into(),
            start_time: 10,
            end_time: 11,
            price_low: 101.0,
            price_high: 100.0,
            label: "FVG".into(),
            state: ZoneState::Active,
        });
        assert!(value.validate().unwrap_err().contains("price_low"));
    }
}
