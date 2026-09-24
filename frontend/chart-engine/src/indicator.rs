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

    /// Build a preview from the setups a strategy replay produced.
    ///
    /// This is the one place replayed signals become chart evidence, and it is
    /// deliberately pure: [`ReplaySetup`] is the chart's own vocabulary, so
    /// nothing here knows about `strategy-runtime`, a sandbox or a fill. The
    /// caller replays; the chart describes what the document decided.
    ///
    /// The result always passes [`IndicatorOutput::validate`]: ids are unique by
    /// construction, coordinates are copied from finite replay values, and the
    /// output is capped at [`MAX_PRIMITIVES`] by keeping the **most recent**
    /// setups -- the ones a chart is showing -- and dropping older ones rather
    /// than emitting an output the chart would refuse.
    #[must_use]
    pub fn from_replay(revision_id: impl Into<String>, setups: &[ReplaySetup]) -> Self {
        // Walk backwards so the most recent setups win the primitive budget,
        // then restore chronology for a stable, readable ordering.
        let mut kept: Vec<Fragment> = Vec::new();
        let mut used = 0usize;
        for (index, setup) in setups.iter().enumerate().rev() {
            let fragment = Fragment::of(index, setup);
            if used + fragment.len() > MAX_PRIMITIVES {
                break;
            }
            used += fragment.len();
            kept.push(fragment);
        }
        kept.reverse();

        let mut output = Self {
            revision_id: revision_id.into(),
            ..Self::default()
        };
        for fragment in kept {
            output.evidence.extend(fragment.evidence);
            output.zones.extend(fragment.zones);
            output.markers.extend(fragment.markers);
            output.links.extend(fragment.links);
        }
        output
    }

    /// Trim the output to the chart's primitive budget, keeping the newest
    /// evidence and dropping the oldest.
    ///
    /// A detector over a week of bars can match thousands of windows -- more
    /// than [`MAX_PRIMITIVES`] -- and [`IndicatorOutput::validate`] refuses the
    /// whole layer past the cap, which renders a full week of detections as
    /// nothing at all. Culling to the budget keeps the most recent matches (the
    /// ones a chart is showing) and what remains still passes validate. A no-op
    /// when the output is already within budget.
    pub fn cull_to_budget(&mut self) {
        let total = self.evidence.len() + self.zones.len() + self.markers.len() + self.links.len();
        if total <= MAX_PRIMITIVES {
            return;
        }
        // Evidence, zones and markers are built one-per-match in the same
        // order and linked by id, so truncating all three equally keeps the
        // chain intact: every kept zone and marker cites a kept evidence node.
        // At 3 primitives per match the budget allows MAX_PRIMITIVES / 3 of
        // them; a third of the cap leaves headroom for anything the caller
        // appends after culling.
        let keep = MAX_PRIMITIVES / 3;
        self.evidence
            .drain(..self.evidence.len().saturating_sub(keep));
        self.zones.drain(..self.zones.len().saturating_sub(keep));
        self.markers
            .drain(..self.markers.len().saturating_sub(keep));
        // Links are dropped wholesale when over budget: a detector layer emits
        // none, and a strategy replay has `from_replay` for its own culling.
        self.links.clear();
    }
}

/// Which way a replayed setup trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupDirection {
    /// Buy: the stop sits below the entry.
    Long,
    /// Sell: the stop sits above the entry.
    Short,
}

/// How a replayed setup ended, in the chart's own words.
///
/// A plain `trigger` string rather than `strategy_runtime::ExitTrigger`, for the
/// same reason [`SetupDirection`] is not `strategy_dsl::Direction`: the chart
/// stays a leaf and the mapping lives with the replay that produced it. The
/// string is the trigger's canonical name (`stop`, `target`, `invalidation`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayExit {
    /// Candle close time the position was closed, unix nanos.
    pub time: i64,
    /// Price the position was closed at.
    pub price: f64,
    /// Canonical name of the trigger that closed it.
    pub trigger: String,
}

/// One replayed entry decision, with its reasons and optional exit.
///
/// The neutral bridge between a strategy replay and the chart: the driver fills
/// this from a document's signals, and [`IndicatorOutput::from_replay`] turns it
/// into evidence. It carries prices and times exactly as the interpreter decided
/// them -- the entry is the decision candle's close, not a fill -- so the chart
/// never invents a level the strategy did not use.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplaySetup {
    /// Stable id, unique within one replay.
    pub id: String,
    /// Which way the setup trades.
    pub direction: SetupDirection,
    /// Close time of the decision candle, unix nanos.
    pub decision_time: i64,
    /// Price the stop and target were resolved against (the decision close).
    pub entry_price: f64,
    /// The resolved stop price.
    pub stop_price: f64,
    /// The resolved take-profit price, when the document declares one.
    pub target_price: Option<f64>,
    /// Labels of the conditions that fired, in document order.
    pub reasons: Vec<String>,
    /// How the position ended, when the replay saw an exit.
    pub exit: Option<ReplayExit>,
}

/// The primitives one setup contributes, built as a unit so the budget can be
/// checked before any of it is admitted.
struct Fragment {
    evidence: Vec<Evidence>,
    zones: Vec<IndicatorZone>,
    markers: Vec<IndicatorMarker>,
    links: Vec<EvidenceLink>,
}

impl Fragment {
    fn len(&self) -> usize {
        self.evidence.len() + self.zones.len() + self.markers.len() + self.links.len()
    }

    fn of(index: usize, setup: &ReplaySetup) -> Self {
        // A non-finite entry would fail validation and take the whole preview
        // down with it, so it contributes nothing instead. The interpreter does
        // not produce one; this is the boundary refusing to trust that.
        if !setup.entry_price.is_finite() {
            return Self {
                evidence: Vec::new(),
                zones: Vec::new(),
                markers: Vec::new(),
                links: Vec::new(),
            };
        }

        let signal_id = format!("setup{index}.signal");
        let (event, label, kind) = match setup.direction {
            SetupDirection::Long => ("long_setup", "Long", MarkerKind::Bullish),
            SetupDirection::Short => ("short_setup", "Short", MarkerKind::Bearish),
        };
        let explanation = if setup.reasons.is_empty() {
            "Entry conditions met.".to_string()
        } else {
            format!("Entry conditions met: {}.", setup.reasons.join("; "))
        };

        let mut evidence = vec![Evidence {
            id: signal_id.clone(),
            event: event.to_string(),
            time: setup.decision_time,
            price: setup.entry_price,
            explanation,
        }];
        let mut markers = vec![IndicatorMarker {
            id: format!("setup{index}.marker"),
            evidence_id: signal_id.clone(),
            time: setup.decision_time,
            price: setup.entry_price,
            label: label.to_string(),
            kind,
        }];
        let mut links = Vec::new();

        // Each fired condition is its own evidence node, linked *into* the
        // signal: the chain reads conditions -> signal, which is what the
        // document actually evaluated. A blank label would fail validation, so
        // it is dropped rather than drawn as an empty node.
        for (reason_index, reason) in setup.reasons.iter().enumerate() {
            if reason.trim().is_empty() {
                continue;
            }
            let reason_id = format!("setup{index}.reason{reason_index}");
            evidence.push(Evidence {
                id: reason_id.clone(),
                event: "condition".to_string(),
                time: setup.decision_time,
                price: setup.entry_price,
                explanation: reason.clone(),
            });
            links.push(EvidenceLink {
                id: format!("setup{index}.link{reason_index}"),
                from: reason_id,
                to: signal_id.clone(),
            });
        }

        // The risk band runs from the entry to the stop, ending when the
        // position closed (or at the signal itself while it is still open).
        let mut zones = Vec::new();
        if setup.stop_price.is_finite() && setup.entry_price != setup.stop_price {
            let end_time = setup
                .exit
                .as_ref()
                .map_or(setup.decision_time, |exit| exit.time)
                .max(setup.decision_time);
            zones.push(IndicatorZone {
                id: format!("setup{index}.risk"),
                start_time: setup.decision_time,
                end_time,
                price_low: setup.entry_price.min(setup.stop_price),
                price_high: setup.entry_price.max(setup.stop_price),
                label: "risk".to_string(),
                state: zone_state(setup.exit.as_ref()),
            });
        }

        if let Some(exit) = &setup.exit {
            if exit.price.is_finite() {
                let trigger = exit.trigger.trim();
                let trigger = if trigger.is_empty() { "exit" } else { trigger };
                let exit_id = format!("setup{index}.exit");
                evidence.push(Evidence {
                    id: exit_id.clone(),
                    event: format!("exit_{trigger}"),
                    time: exit.time,
                    price: exit.price,
                    explanation: format!("Position closed by {trigger}."),
                });
                markers.push(IndicatorMarker {
                    id: format!("setup{index}.exit-marker"),
                    evidence_id: exit_id.clone(),
                    time: exit.time,
                    price: exit.price,
                    label: "Exit".to_string(),
                    kind: exit_marker_kind(setup.direction, trigger),
                });
                links.push(EvidenceLink {
                    id: format!("setup{index}.exit-link"),
                    from: signal_id,
                    to: exit_id,
                });
            }
        }

        Self {
            evidence,
            zones,
            markers,
            links,
        }
    }
}

/// The lifecycle state a setup's risk band has reached.
fn zone_state(exit: Option<&ReplayExit>) -> ZoneState {
    match exit {
        None => ZoneState::Active,
        Some(exit) => match exit.trigger.trim() {
            "target" => ZoneState::Mitigated,
            "stop" | "invalidation" => ZoneState::Invalidated,
            _ => ZoneState::Active,
        },
    }
}

/// The marker style for a closing trigger: favourable, adverse, or neither.
fn exit_marker_kind(direction: SetupDirection, trigger: &str) -> MarkerKind {
    match trigger {
        "target" => match direction {
            SetupDirection::Long => MarkerKind::Bullish,
            SetupDirection::Short => MarkerKind::Bearish,
        },
        "stop" | "invalidation" => match direction {
            SetupDirection::Long => MarkerKind::Bearish,
            SetupDirection::Short => MarkerKind::Bullish,
        },
        _ => MarkerKind::Context,
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

    fn setup(exit: Option<ReplayExit>) -> ReplaySetup {
        ReplaySetup {
            id: "setup-1".into(),
            direction: SetupDirection::Long,
            decision_time: 1_700_000_000_000_000_000,
            entry_price: 100.0,
            stop_price: 95.0,
            target_price: Some(110.0),
            reasons: vec![
                "liquidity.swept == \"sell_side\"".into(),
                "delta > 0".into(),
            ],
            exit,
        }
    }

    #[test]
    fn a_replayed_setup_becomes_a_valid_evidence_chain() {
        let output = IndicatorOutput::from_replay(
            "revision-1",
            &[setup(Some(ReplayExit {
                time: 1_700_000_600_000_000_000,
                price: 110.0,
                trigger: "target".into(),
            }))],
        );
        output.validate().expect("a replay builds valid output");

        // One signal node plus one per reason, plus the exit.
        assert_eq!(output.evidence.len(), 4);
        // A signal marker and an exit marker.
        assert_eq!(output.markers.len(), 2);
        // Two reason links into the signal, and one signal link to the exit.
        assert_eq!(output.links.len(), 3);
        // The risk band spans entry to stop and is mitigated by the target.
        assert_eq!(output.zones.len(), 1);
        assert_eq!(output.zones[0].state, ZoneState::Mitigated);
        assert_eq!(output.zones[0].price_low, 95.0);
        assert_eq!(output.zones[0].price_high, 100.0);
    }

    #[test]
    fn an_open_setup_has_an_active_risk_band() {
        let output = IndicatorOutput::from_replay("revision-1", &[setup(None)]);
        output.validate().expect("valid without an exit");
        assert_eq!(output.markers.len(), 1);
        assert_eq!(output.zones[0].state, ZoneState::Active);
    }

    #[test]
    fn the_preview_keeps_the_newest_setups_within_the_budget() {
        // Many setups, each contributing evidence and markers. The builder must
        // stay under the primitive cap by keeping the most recent rather than
        // emitting output the chart would refuse.
        let setups: Vec<ReplaySetup> = (0..MAX_PRIMITIVES)
            .map(|index| ReplaySetup {
                id: format!("setup-{index}"),
                decision_time: 1_700_000_000_000_000_000 + index as i64,
                ..setup(None)
            })
            .collect();
        let output = IndicatorOutput::from_replay("revision-1", &setups);
        output.validate().expect("bounded by construction");
        let total =
            output.evidence.len() + output.zones.len() + output.markers.len() + output.links.len();
        assert!(total <= MAX_PRIMITIVES, "{total} exceeded the cap");
        // The last setup's still-present band proves recency won the budget.
        let newest = 1_700_000_000_000_000_000 + (MAX_PRIMITIVES - 1) as i64;
        assert!(output.zones.iter().any(|zone| zone.start_time == newest));
    }

    #[test]
    fn culling_keeps_the_newest_detections_and_a_valid_chain() {
        // One matched window = one evidence + one zone + one marker, all
        // sharing a chain of ids. Four times the cap must cull to within it.
        let count = MAX_PRIMITIVES * 4;
        let mut output = IndicatorOutput {
            revision_id: "revision-1".into(),
            ..IndicatorOutput::default()
        };
        for index in 0..count {
            let id = format!("concept-{index}");
            output.evidence.push(Evidence {
                id: id.clone(),
                event: "bullish_gap".into(),
                time: 1_700_000_000_000_000_000 + index as i64,
                price: 100.0,
                explanation: "detected".into(),
            });
            output.zones.push(IndicatorZone {
                id: id.clone(),
                start_time: 1_700_000_000_000_000_000 + index as i64,
                end_time: 1_700_000_000_000_000_000 + index as i64 + 1,
                price_low: 100.0,
                price_high: 105.0,
                label: "fvg".into(),
                state: ZoneState::Active,
            });
            output.markers.push(IndicatorMarker {
                id: format!("{id}-marker"),
                evidence_id: id,
                time: 1_700_000_000_000_000_000 + index as i64,
                price: 100.0,
                label: "fvg".into(),
                kind: MarkerKind::Bullish,
            });
        }
        output.cull_to_budget();
        output
            .validate()
            .expect("culled output must still validate");
        assert_eq!(output.evidence.len(), output.zones.len());
        assert_eq!(output.evidence.len(), output.markers.len());
        // The newest match survived; the oldest was dropped.
        let newest = 1_700_000_000_000_000_000 + (count - 1) as i64;
        assert!(output.zones.iter().any(|zone| zone.start_time == newest));
        assert!(!output
            .zones
            .iter()
            .any(|zone| zone.start_time == 1_700_000_000_000_000_000));
        // Every kept marker still cites a kept evidence node.
        let ids: Vec<&str> = output.evidence.iter().map(|e| e.id.as_str()).collect();
        for marker in &output.markers {
            assert!(ids.contains(&marker.evidence_id.as_str()));
        }
    }

    #[test]
    fn culling_within_budget_is_a_no_op() {
        let mut output = IndicatorOutput {
            revision_id: "revision-1".into(),
            ..IndicatorOutput::default()
        };
        output.evidence.push(Evidence {
            id: "sweep".into(),
            event: "liquidity_sweep".into(),
            time: 1_700_000_000_000_000_000,
            price: 100.0,
            explanation: "swept".into(),
        });
        let before = output.evidence.len();
        output.cull_to_budget();
        assert_eq!(output.evidence.len(), before);
    }
}
