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

/// Pivot confirmation half-width for trendline fitting.
///
/// 1 keeps the line honest to recent swings; higher values smooth noise but
/// lag the structure the user is looking at.
pub const TRENDLINE_STRENGTH: usize = 1;

/// Trendlines kept when an output breaches the primitive budget.
pub const MAX_TRENDLINES_KEEP: usize = 16;

/// All visual evidence produced by one validated indicator revision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorOutput {
    /// Immutable source revision that produced the visuals.
    pub revision_id: String,
    /// The document's display name, when the caller knows it. Rendered on the
    /// chart chip; absent leaves the chip to the revision id.
    #[serde(default)]
    pub name: Option<String>,
    /// The document's concepts, when the output was produced by a `kind:
    /// indicator` document. Persisted with the revision so the chart can
    /// re-detect **live** on any symbol and timeframe instead of replaying a
    /// frozen snapshot: the concepts are the definition, the coordinates below
    /// are just the preview the generator saw.
    #[serde(default)]
    pub concepts: Vec<analytics_core::concepts::Concept>,
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
    /// Fitted trendlines, in market coordinates.
    ///
    /// Produced by live re-detection of concepts that declare
    /// `shape: trendline`; the generator's own snapshot preview carries them
    /// the same way, so a stored preview and a live layer draw the same
    /// geometry.
    #[serde(default)]
    pub trendlines: Vec<IndicatorTrendline>,
}

/// One fitted trendline, in market coordinates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorTrendline {
    /// Stable generated id.
    pub id: String,
    /// Ready-to-display label.
    pub label: String,
    /// The fitted points, in time order. Two or more; fewer is not drawn.
    pub points: Vec<TrendPoint>,
}

/// One point of a market-coordinate trendline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrendPoint {
    /// Candle open time.
    pub time: i64,
    /// Price at that time.
    pub price: f64,
}

impl IndicatorOutput {
    /// Validate untrusted generated output before it reaches a chart scene.
    ///
    /// # Errors
    /// Returns a human-readable refusal that can be shown in the workspace chat.
    pub fn validate(&self) -> Result<(), String> {
        non_empty("revision_id", &self.revision_id)?;
        let total = self.evidence.len()
            + self.zones.len()
            + self.markers.len()
            + self.links.len()
            + self.trendlines.len();
        if total > MAX_PRIMITIVES {
            return Err(format!(
                "indicator output has {total} primitives; the chart limit is {MAX_PRIMITIVES}"
            ));
        }
        for trendline in &self.trendlines {
            non_empty("trendline.id", &trendline.id)?;
            non_empty("trendline.label", &trendline.label)?;
            if trendline.points.len() < 2 {
                return Err(format!(
                    "trendline `{}` has {} point(s); a line needs at least two",
                    trendline.id,
                    trendline.points.len()
                ));
            }
            for point in &trendline.points {
                finite("trendline.time", point.time as f64)?;
                finite("trendline.price", point.price)?;
            }
            // Time order is a rendering invariant: a zigzag drawn out of order
            // is a lie about the structure it claims to connect.
            if trendline
                .points
                .windows(2)
                .any(|pair| pair[1].time < pair[0].time)
            {
                return Err(format!(
                    "trendline `{}` is not in time order",
                    trendline.id
                ));
            }
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
        // `name` and `concepts` stay empty here: a replayed setup layer carries
        // neither -- it is positioned output, not a document definition.
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
        let total = self.evidence.len()
            + self.zones.len()
            + self.markers.len()
            + self.links.len()
            + self.trendlines.len();
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
        // Trendlines are rare -- one per trendline-shaped concept -- so they
        // never realistically breach the budget on their own; keep them, and
        // let the newest-matches culling above absorb the pressure.
        self.trendlines.truncate(MAX_TRENDLINES_KEEP);
    }

    /// Detect the output's own concepts over `candles` and **replace** the
    /// evidence/zones/markers with the live result.
    ///
    /// This is what makes a generated indicator portable and current: the
    /// request carries the series (any symbol, any timeframe, the forming bar
    /// included) and this recomputes the layer from the document's concepts --
    /// the definition -- rather than replaying coordinates captured once in the
    /// generator chat. Overlapping same-name bands are merged so a detector
    /// that fires a hundred times in a week reads as the dozen distinct levels
    /// it actually is, and the result is culled to the render budget.
    ///
    /// A concept that fails validation is skipped (never half-drawn); when
    /// **every** concept is refused the output is emptied honestly, which the
    /// caller surfaces as an empty layer rather than a broken chart.
    pub fn refresh_from_concepts(&mut self, candles: &[analytics_core::types::Candle]) {
        self.evidence.clear();
        self.zones.clear();
        self.markers.clear();
        self.links.clear();
        self.trendlines.clear();

        // Validated concepts only; refusals are reported by the caller through
        // the scene note, not drawn half-way.
        let concepts: Vec<_> = self
            .concepts
            .iter()
            .filter(|concept| analytics_core::concepts::validate(concept).is_ok())
            .cloned()
            .collect();

        // One evidence/zone/marker triple per detected band, exactly as the
        // generator's own preview built them -- same id scheme, same label
        // source, same mitigation vocabulary -- so a live layer and its stored
        // preview are the same drawing at different times.
        let mut next = 0usize;
        let mut bands: Vec<(analytics_core::regions::Region, usize)> = Vec::new();
        for concept in &concepts {
            for region in analytics_core::concepts::detect(candles, concept) {
                bands.push((region, next));
                next += 1;
            }
        }

        // Merge overlapping bands **per concept slot**: same label, overlapping
        // in price and time means one level seen through several windows, not
        // several levels. Different concepts stay separate even when they
        // overlap -- a bull FVG and a demand band describing the same prices are
        // two statements, not one.
        bands.sort_by(|a, b| {
            a.0.name
                .cmp(&b.0.name)
                .then(a.0.from.cmp(&b.0.from))
        });
        let mut merged: Vec<analytics_core::regions::Region> = Vec::with_capacity(bands.len());
        for (region, _) in bands {
            match merged.last_mut() {
                // Overlap in both axes, and the same name: extend the band.
                // The widest price span wins; the longest life wins. A merged
                // band's mitigation is the *maximum* of its parts -- any part
                // price fully traded through is a part that is gone.
                Some(last)
                    if last.name == region.name
                        && region.from <= last.to
                        && region.price_low <= last.price_high
                        && region.price_high >= last.price_low =>
                {
                    last.price_low = last.price_low.min(region.price_low);
                    last.price_high = last.price_high.max(region.price_high);
                    last.to = last.to.max(region.to);
                    last.mitigated = last.mitigated.max(region.mitigated);
                }
                _ => merged.push(region),
            }
        }

        // One fitted line per trendline-shaped concept: the swing pivots of
        // the whole series, connected in time order. This is the half of the
        // "draw the trendline" request the band detector could never answer.
        let mut next_line = 0usize;
        for concept in &concepts {
            if concept.shape != analytics_core::concepts::ConceptShape::Trendline {
                continue;
            }
            let points = analytics_core::concepts::trendline_segments(candles, TRENDLINE_STRENGTH)
                .into_iter()
                .map(|p| TrendPoint {
                    time: p.time,
                    price: p.price,
                })
                .collect::<Vec<_>>();
            if points.len() < 2 {
                continue;
            }
            self.trendlines.push(IndicatorTrendline {
                id: format!("live-tl-{next_line}"),
                label: concept.label(),
                points,
            });
            next_line += 1;
        }

        for (index, region) in merged.iter().enumerate() {
            let id = format!("live-{index}");
            self.evidence.push(Evidence {
                id: id.clone(),
                event: region.name.clone(),
                time: region.from,
                price: region.price_low,
                explanation: format!(
                    "detected {} (band {}..{})",
                    region.name, region.price_low, region.price_high
                ),
            });
            self.zones.push(IndicatorZone {
                id: id.clone(),
                start_time: region.from,
                end_time: region.to,
                price_low: region.price_low,
                price_high: region.price_high,
                label: region.name.clone(),
                state: region_zone_state(region),
            });
            self.markers.push(IndicatorMarker {
                id: format!("{id}-marker"),
                evidence_id: id,
                time: region.from,
                price: region.price_low,
                label: region.name.clone(),
                kind: marker_kind(region.side),
            });
        }
        self.cull_to_budget();
    }
}

/// Map a detected region's mitigation to the chart's zone lifecycle.
///
/// The same vocabulary the gateway's preview builder applies; living here too
/// keeps a live layer and its stored preview the same drawing.
fn region_zone_state(region: &analytics_core::regions::Region) -> ZoneState {
    if region.mitigated <= 0.0 {
        ZoneState::Active
    } else if region.mitigated > 1.0 {
        ZoneState::Tapped
    } else if (region.mitigated - 1.0).abs() < 1e-9 {
        ZoneState::Mitigated
    } else {
        ZoneState::Active
    }
}

/// Map a detection side to the marker vocabulary the chart paints.
fn marker_kind(side: analytics_core::types::Side) -> MarkerKind {
    match side {
        analytics_core::types::Side::Buy => MarkerKind::Bullish,
        analytics_core::types::Side::Sell => MarkerKind::Bearish,
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
            name: None,
            concepts: Vec::new(),
            trendlines: Vec::new(),
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

    #[test]
    fn zone_state_wire_strings_are_the_shells_lifecycle_tiers() {
        // docs/25: the shell tiers a zone's paint on these exact strings --
        // "tapped" draws half-strength, "mitigated"/"invalidated" draw a
        // dashed ghost, the rest draw full. A rename compiles everywhere and
        // silently turns the tiers off, so the strings are pinned here.
        let cases = [
            (ZoneState::Created, "\"created\""),
            (ZoneState::Active, "\"active\""),
            (ZoneState::Tapped, "\"tapped\""),
            (ZoneState::Mitigated, "\"mitigated\""),
            (ZoneState::Invalidated, "\"invalidated\""),
        ];
        for (state, wire) in cases {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, wire);
            let back: ZoneState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
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

    /// A five-candle series with one fair-value gap in the middle.
    fn fvg_series() -> Vec<analytics_core::types::Candle> {
        use analytics_core::types::{Candle, Timeframe};
        let width = Timeframe::M5.nanos();
        (0..5)
            .map(|i| {
                let (open, close) = match i {
                    0 => (100.0, 100.2),
                    1 => (105.0, 106.0),
                    2 => (107.0, 108.0),
                    3 => (108.0, 108.5),
                    // Deliberately not another displacement: low(4) must sit
                    // below high(2) so the window [2,3,4] is not a third gap
                    // and the fixture has exactly one merged band.
                    _ => (108.2, 108.4),
                };
                Candle {
                    symbol: "TEST".into(),
                    timeframe: Timeframe::M5,
                    open_time: i * width,
                    open,
                    close,
                    high: open.max(close) + 0.2,
                    low: open.min(close) - 0.2,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect()
    }

    /// The FVG concept matching [`fvg_series`], as a document would declare it.
    fn fvg_concept() -> analytics_core::concepts::Concept {
        serde_json::from_value(serde_json::json!({
            "name": "bullish_gap",
            "label": "fvg",
            "side": "buy",
            "window": 3,
            "lower": { "high": 0 },
            "upper": { "low": 2 },
            "require": [{ "left": { "high": 0 }, "op": "below", "right": { "low": 2 } }]
        }))
        .expect("the fixture concept must parse")
    }

    #[test]
    fn a_live_indicator_is_detected_from_its_concepts() {
        let mut output = IndicatorOutput {
            revision_id: "live".into(),
            name: Some("FVG probe".into()),
            concepts: vec![fvg_concept()],
            ..IndicatorOutput::default()
        };
        output.refresh_from_concepts(&fvg_series());
        output
            .validate()
            .expect("a live layer must satisfy its own contract");
        assert_eq!(output.zones.len(), 1, "{:#?}", output.zones);
        assert_eq!(output.zones[0].label, "fvg");
        assert_eq!(output.evidence.len(), output.zones.len());
        assert_eq!(output.markers.len(), output.zones.len());
        // The marker cites evidence that exists.
        assert_eq!(output.markers[0].evidence_id, output.evidence[0].id);
    }

    #[test]
    fn a_live_indicator_re_detects_when_the_series_changes() {
        let mut output = IndicatorOutput {
            revision_id: "live".into(),
            name: Some("FVG probe".into()),
            concepts: vec![fvg_concept()],
            ..IndicatorOutput::default()
        };
        output.refresh_from_concepts(&fvg_series());
        let first_from = output.zones[0].start_time;

        // The same definition over a series shifted a day later -- a different
        // symbol's data, or a later window -- produces a layer anchored to the
        // new series, not to the one it was generated on.
        let shifted: Vec<_> = fvg_series()
            .into_iter()
            .map(|mut c| {
                c.open_time += 24 * 60 * 60 * 1_000_000_000;
                c
            })
            .collect();
        output.refresh_from_concepts(&shifted);
        assert_eq!(output.zones.len(), 1);
        assert_eq!(
            output.zones[0].start_time,
            first_from + 24 * 60 * 60 * 1_000_000_000,
            "the layer must follow the new series, not the old one"
        );
    }

    #[test]
    fn overlapping_same_name_bands_merge_into_one_zone() {
        // Two back-to-back FVG windows share candles, so their bands overlap
        // in price and time and describe one extended level.
        use analytics_core::types::{Candle, Timeframe};
        let width = Timeframe::M5.nanos();
        let candles: Vec<Candle> = (0..6)
            .map(|i| {
                let (open, close) = match i {
                    0 => (100.0, 100.2),
                    1 => (104.0, 106.0),
                    2 => (107.0, 108.0),
                    3 => (110.0, 112.0),
                    4 => (113.0, 114.0),
                    _ => (114.0, 114.5),
                };
                Candle {
                    symbol: "TEST".into(),
                    timeframe: Timeframe::M5,
                    open_time: i * width,
                    open,
                    close,
                    high: open.max(close) + 0.2,
                    low: open.min(close) - 0.2,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                }
            })
            .collect();
        let mut output = IndicatorOutput {
            revision_id: "live".into(),
            name: Some("merge probe".into()),
            concepts: vec![fvg_concept()],
            ..IndicatorOutput::default()
        };
        output.refresh_from_concepts(&candles);
        assert!(
            output.zones.len() < 3,
            "overlapping windows must merge, got {} zones: {:#?}",
            output.zones.len(),
            output.zones
        );
    }

    #[test]
    fn a_live_indicator_over_a_series_with_no_matches_is_an_honest_empty() {
        let mut output = IndicatorOutput {
            revision_id: "live".into(),
            name: Some("quiet probe".into()),
            concepts: vec![fvg_concept()],
            ..IndicatorOutput::default()
        };
        output.refresh_from_concepts(&[]);
        assert!(output.zones.is_empty());
        assert!(output.validate().is_ok(), "an empty layer is still valid");
    }
}
