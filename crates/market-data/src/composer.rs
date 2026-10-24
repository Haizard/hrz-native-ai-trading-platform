//! The Strategy Composer: declarative strategies over the Concept Registry.
//!
//! A composition names *what* (registered concepts, their directions, the
//! order and timing between them), never *how* -- the registry owns the
//! detection, this file owns the combination. The same document evaluates
//! the same way on every surface that reads it: a backtest replay here, the
//! bot's decision loop next, and the AI's validation before either.
//!
//! Semantics are deliberately small and deterministic:
//!
//! - Events arrive per bar (bar-close facts; a tick never makes one).
//! - A leaf holds when a matching event occurred within its lookback.
//! - `all` holds when every child does; `any` when one does; `not` when the
//!   child does not (within its own lookback -- a stale event cannot veto).
//! - `sequence` holds when its steps satisfied *in order*, each within
//!   `max_bars` of the previous step's satisfaction.
//! - The composition fires on the *edge*: one entry per satisfaction
//!   episode, not one per bar while it stays true.

use serde::{Deserialize, Serialize};

use crate::concepts::{ConceptEvent, Direction};

/// A composition document: the whole declarative strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    /// The strategy's name.
    pub name: String,
    /// The document's own version (`1.0.0` when written by hand).
    pub version: String,
    /// The resolution it evaluates on (`15m`).
    pub timeframe: String,
    /// The market it trades, when it is meant to run as a bot (`BTCUSDT`,
    /// `DERIV:VOLATILITY 75`). Absent for analysis-only compositions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
    /// Which side it trades.
    pub side: CompositionSide,
    /// The condition tree.
    pub conditions: Condition,
    /// The risk block.
    pub risk: RiskBlock,
    /// Per-concept parameter overrides (trader A's BOS is not trader B's):
    /// `{ "smc.bos": { "swing_lookback": 5 } }`. Absent means the registry's
    /// defaults, which is what every existing document already means.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub parameters: std::collections::HashMap<String, std::collections::HashMap<String, f64>>,
}

/// The side a composition trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompositionSide {
    /// Long entries (bullish readings).
    Long,
    /// Short entries (bearish readings).
    Short,
}

/// A leaf's direction requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectionReq {
    /// Must read bullish.
    Bullish,
    /// Must read bearish.
    Bearish,
    /// Either direction holds.
    Any,
}

/// The condition tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Condition {
    /// One concept, one direction: `{"type":"concept","concept":"smc.bos","direction":"bullish"}`.
    Concept {
        /// The registry id.
        concept: String,
        /// The required reading.
        direction: DirectionReq,
        /// How far back a matching event still counts (default 10 bars).
        #[serde(default)]
        lookback_bars: Option<usize>,
        /// The timeframe this leaf reads, when it differs from the
        /// document's own (`"4h"` on a `"1m"` document -- the
        /// multi-timeframe rule).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeframe: Option<String>,
        /// The event must be at least this many bars old (a confirmation
        /// delay: "at least 5 bars after the sweep"). Default 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_bars_since: Option<usize>,
    },
    /// Price retraced INTO the newest zone of a concept (the FVG to come
    /// back to, the order block to revisit): holds when the newest close
    /// sits inside the concept's newest zone, which fired within lookback.
    Retrace {
        /// The zone-producing concept id.
        into: String,
        /// Its timeframe when it differs from the document's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeframe: Option<String>,
        /// How far back the zone may have formed (default 20 bars).
        #[serde(default)]
        lookback_bars: Option<usize>,
    },
    /// The newest close against a leaf's own level (the swing, the zone
    /// edge, the pool's price): "price above the swing", "close below the
    /// distal edge", with an optional percent tolerance.
    PriceVs {
        /// Which side of the level the close must sit on.
        side: PriceSide,
        /// The concept whose newest event supplies the level.
        concept: String,
        /// Its timeframe when it differs from the document's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeframe: Option<String>,
        /// How far back the level's event may be (default 20 bars).
        #[serde(default)]
        lookback_bars: Option<usize>,
        /// The tolerance as a fraction of the level (0.002 = 0.2%).
        #[serde(default)]
        tolerance: f64,
    },
    /// The bar's own UTC hour must sit inside the window: "only in the New
    /// York killzone", never an entry at 03:00.
    TimeWindow {
        /// The window's start hour UTC (inclusive).
        start_hour: u32,
        /// The window's end hour UTC (exclusive; may wrap past midnight).
        end_hour: u32,
    },
    /// Every child must hold.
    All {
        /// The children.
        conditions: Vec<Condition>,
    },
    /// At least one child must hold.
    Any {
        /// The children.
        conditions: Vec<Condition>,
    },
    /// The child must not hold (within its own lookback).
    Not {
        /// The negated condition.
        condition: Box<Condition>,
    },
    /// Ordered satisfaction: each step within `max_bars` of the last.
    Sequence {
        /// The steps, in required order.
        steps: Vec<SequenceStep>,
        /// The widest gap between consecutive steps.
        max_bars: usize,
        /// The whole sequence must complete within this many bars of its
        /// first step satisfying ("cancel if unfilled after 2 hours"). None
        /// means no expiry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_after_bars: Option<usize>,
    },
}

/// The side of a level a `PriceVs` condition reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSide {
    /// The close is at or above the level (plus tolerance).
    Above,
    /// The close is at or below the level (minus tolerance).
    Below,
    /// The close crossed the level this bar (either direction).
    Cross,
}

/// One sequence step: a condition plus an optional per-step window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceStep {
    /// What must satisfy at this step.
    pub condition: Condition,
    /// Per-step lookback override (bars).
    #[serde(default)]
    pub lookback_bars: Option<usize>,
}

/// How far back a leaf's event still counts when nothing says otherwise.
pub const DEFAULT_LOOKBACK_BARS: usize = 10;

/// The risk block: stops and sizing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskBlock {
    /// Where the protective stop goes.
    pub stop: StopKind,
    /// How size is chosen.
    pub sizing: SizeKind,
    /// The exit's own conditions (a concept-based exit: "exit at the
    /// opposite pool", "out when RSI hits 70"). Evaluated per bar while a
    /// position is open; the protective stop always rides underneath.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Condition>,
    /// Partial exits at R multiples of the entry's own stop distance:
    /// `[{at_r: 1.0, close_pct: 50}]` closes half at +1R.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partials: Vec<PartialExit>,
    /// How the entry fills: the next bar's open (default), or a limit at the
    /// satisfying zone's proximal edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryMode>,

    /// The position-management rules (break-even and the ATR trail). Absent
    /// means the stop sits where it was placed.
    #[serde(default)]
    pub management: Option<analytics_core::risk::ManagementRules>,
    /// The account guards (daily/weekly loss, drawdown, streak, positions).
    #[serde(default)]
    pub guards: Option<analytics_core::risk::RiskGuards>,
}

/// One partial exit: at this R multiple, close this percent of the position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PartialExit {
    /// The R multiple from entry (1.0 = one stop-distance in profit).
    pub at_r: f64,
    /// The percent of the ORIGINAL position to close (50 = half).
    pub close_pct: f64,
}

/// How the entry fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EntryMode {
    /// The next bar's open (the default everywhere).
    MarketNextOpen,
    /// A limit at the satisfying zone's proximal edge (the edge nearest the
    /// signal's price).
    LimitAtProximal,
}

/// Stop placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopKind {
    /// At the satisfying event's own level (the swept price, the broken
    /// swing): the structure that made the entry is the thing that
    /// invalidates it.
    Structure,
    /// A fixed multiple of the bar's ATR(14) from the entry.
    AtrMultiple {
        /// The multiple.
        multiple: f64,
    },
}

/// Position sizing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SizeKind {
    /// A fixed fraction of equity per trade (percent).
    FixedFractional {
        /// Equity percent at risk-notional per trade.
        percent: f64,
    },
    /// A fixed stake/quantity every time.
    Fixed {
        /// The quantity (stake for contracts).
        quantity: f64,
    },
    /// Risk exactly this equity fraction per trade; the size is the risk
    /// divided by the stop's own distance (the stop carries the volatility,
    /// the size carries the risk).
    VolatilityAdjusted {
        /// Equity fraction at risk (0.01 = 1%).
        risk_fraction: f64,
    },
    /// The Kelly fraction from the run's own statistics, capped (the
    /// two-pass honest form: the first pass measures, the second bets it).
    Kelly {
        /// The cap as an equity fraction (0.2 = half-Kelly by the desk's
        /// convention).
        cap: f64,
    },
}

/// One validation error, named for the document author.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    /// Where in the tree (`conditions.steps[0].condition`).
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Validate a document against the registry: every concept id must exist,
/// every window must be positive, and the tree's depth is bounded so a
/// pathological document cannot wedge the evaluator.
/// The conflict hints a document gets without failing: contradictions and
/// duplicate measurements the validator can see before the backtest proves
/// them slowly. Warnings never block; errors still do.
pub fn warnings(doc: &Composition) -> Vec<String> {
    let mut warnings = Vec::new();
    // Direction contradictions within one `all` tree: a concept required
    // bullish in one branch and bearish in another can never hold together.
    fn collect_directions(cond: &Condition, out: &mut Vec<(String, DirectionReq, String)>, path: &str) {
        match cond {
            Condition::Concept { concept, direction, .. } => out.push((concept.clone(), *direction, path.to_string())),
            Condition::All { conditions } | Condition::Any { conditions } => {
                for (i, c) in conditions.iter().enumerate() {
                    collect_directions(c, out, &format!("{path}.conditions[{i}]"));
                }
            }
            Condition::Not { condition } => collect_directions(condition, out, &format!("{path}.condition")),
            Condition::Sequence { steps, .. } => {
                for (i, step) in steps.iter().enumerate() {
                    collect_directions(&step.condition, out, &format!("{path}.steps[{i}]"));
                }
            }
            _ => {}
        }
    }
    let mut directions = Vec::new();
    collect_directions(&doc.conditions, &mut directions, "conditions");
    if let Condition::All { .. } = doc.conditions {
        for a in &directions {
            for b in &directions {
                if a.0 == b.0 && a.1 != b.1 && a.1 != DirectionReq::Any && b.1 != DirectionReq::Any {
                    warnings.push(format!(
                        "contradiction: `{}` is required {:?} and {:?} in one `all` -- it can never hold",
                        a.0, a.1, b.1
                    ));
                }
            }
        }
    }
    // Duplicate measurements: conditions from this known-overlap table measure
    // the same quantity under two names.
    const OVERLAP: &[(&[&str], &str)] = &[
        (&["indicators.rsi", "indicators.stochastic", "indicators.williams_r", "indicators.mfi"], "the same oscillator zone under different names"),
        (&["indicators.ema", "indicators.sma"], "two moving-average families measuring the same trend"),
        (&["smc.fvg", "smc.bpr"], "two imbalance measurements with heavy overlap"),
        (&["indicators.atr", "indicators.bollinger_percent"], "two volatility-width measurements"),
    ];
    let mut concept_ids: Vec<String> = Vec::new();
    fn collect_ids(cond: &Condition, out: &mut Vec<String>) {
        match cond {
            Condition::Concept { concept, .. } => out.push(concept.clone()),
            Condition::PriceVs { concept, .. } => out.push(concept.clone()),
            Condition::Retrace { into, .. } => out.push(into.clone()),
            Condition::All { conditions } | Condition::Any { conditions } => {
                for c in conditions {
                    collect_ids(c, out);
                }
            }
            Condition::Not { condition } => collect_ids(condition, out),
            Condition::Sequence { steps, .. } => {
                for step in steps {
                    collect_ids(&step.condition, out);
                }
            }
            _ => {}
        }
    }
    collect_ids(&doc.conditions, &mut concept_ids);
    for (group, reason) in OVERLAP {
        let hits: Vec<&&str> = group.iter().filter(|id| concept_ids.iter().any(|c| c == **id)).collect();
        if hits.len() > 1 {
            warnings.push(format!(
                "duplicate measurement: {} in one tree -- {}",
                hits.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(" and "),
                reason
            ));
        }
    }
    // The sequence gap trap: a step's own lookback longer than the
    // sequence's max_bars means the step can outlive the sequence itself.
    if let Condition::Sequence { steps, max_bars, .. } = &doc.conditions {
        for (i, step) in steps.iter().enumerate() {
            if step.lookback_bars.is_some_and(|lb| lb > *max_bars) {
                warnings.push(format!(
                    "steps[{i}]'s lookback exceeds the sequence's max_bars -- the step can hold past the sequence's own window"
                ));
            }
        }
    }
    warnings
}

/// Validate a document against the registry: every concept id must exist,
/// the tree must be shallow, the numbers must be positive.
pub fn validate(doc: &Composition) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    if doc.name.trim().is_empty() {
        errors.push(ValidationError {
            path: "name".to_string(),
            message: "a composition needs a name".to_string(),
        });
    }
    if doc.timeframe.parse::<analytics_core::types::Timeframe>().is_err() {
        errors.push(ValidationError {
            path: "timeframe".to_string(),
            message: format!("`{}` is not a timeframe this platform serves", doc.timeframe),
        });
    }
    validate_condition(&doc.conditions, "conditions", 0, &mut errors);
    // Parameter overrides must name a registered concept, one of ITS
    // parameters, and a value inside the declared range -- a mistyped knob
    // must not silently become a different strategy.
    for (concept_id, params) in &doc.parameters {
        match crate::concepts::concept(concept_id) {
            None => errors.push(ValidationError {
                path: format!("parameters.{concept_id}"),
                message: format!("`{concept_id}` is not a registered concept"),
            }),
            Some(spec) => {
                for (name, value) in params {
                    match spec.parameters.iter().find(|p| p.name == name.as_str()) {
                        None => errors.push(ValidationError {
                            path: format!("parameters.{concept_id}.{name}"),
                            message: format!("`{name}` is not a parameter of {concept_id}"),
                        }),
                        Some(param) if *value < param.min || *value > param.max => errors.push(ValidationError {
                            path: format!("parameters.{concept_id}.{name}"),
                            message: format!("{value} is outside {name}'s range {}..{}", param.min, param.max),
                        }),
                        _ => {}
                    }
                }
            }
        }
    }
    if let StopKind::AtrMultiple { multiple } = &doc.risk.stop {
        if !(*multiple > 0.0 && multiple.is_finite()) {
            errors.push(ValidationError {
                path: "risk.stop".to_string(),
                message: "the ATR multiple must be a positive number".to_string(),
            });
        }
    }
    match &doc.risk.sizing {
        SizeKind::FixedFractional { percent } if !(*percent > 0.0 && *percent <= 100.0) => {
            errors.push(ValidationError {
                path: "risk.sizing".to_string(),
                message: "the fraction must be within (0, 100] percent".to_string(),
            });
        }
        SizeKind::Fixed { quantity } if !(*quantity > 0.0) => {
            errors.push(ValidationError {
                path: "risk.sizing".to_string(),
                message: "the quantity must be positive".to_string(),
            });
        }
        _ => {}
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

/// The deepest a tree may nest (sequence steps inside groups inside groups).
const MAX_DEPTH: usize = 8;

fn validate_condition(cond: &Condition, path: &str, depth: usize, errors: &mut Vec<ValidationError>) {
    if depth > MAX_DEPTH {
        errors.push(ValidationError {
            path: path.to_string(),
            message: "the condition tree nests too deeply".to_string(),
        });
        return;
    }
    match cond {
        Condition::TimeWindow { start_hour, end_hour } => {
            if *start_hour > 23 || *end_hour > 24 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "hours must be 0..23 (start) and 0..24 (end), UTC".to_string(),
                });
            }
            if start_hour == end_hour {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "a zero-width time window never holds".to_string(),
                });
            }
        }
        Condition::PriceVs { concept, lookback_bars, tolerance, .. } => {
            if crate::concepts::concept(concept).is_none() {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: format!("`{concept}` is not a registered concept (GET /concepts)"),
                });
            }
            if *tolerance < 0.0 || *tolerance > 0.5 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "tolerance must be between 0 and 0.5 (a fraction of the level)".to_string(),
                });
            }
            if lookback_bars.is_some_and(|w| w == 0) {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "lookback_bars must be positive".to_string(),
                });
            }
        }
        Condition::Concept { concept, lookback_bars, .. } => {
            if crate::concepts::concept(concept).is_none() {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: format!("`{concept}` is not a registered concept (GET /concepts)"),
                });
            }
            if lookback_bars.is_some_and(|w| w == 0) {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "lookback_bars must be positive".to_string(),
                });
            }
        }
        Condition::Retrace { into, .. } => {
            if crate::concepts::concept(into).is_none() {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: format!("`{into}` is not a registered concept (GET /concepts)"),
                });
            }
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            if conditions.is_empty() {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "a group needs at least one condition".to_string(),
                });
            }
            for (i, child) in conditions.iter().enumerate() {
                validate_condition(child, &format!("{path}.conditions[{i}]"), depth + 1, errors);
            }
        }
        Condition::Not { condition } => {
            validate_condition(condition, &format!("{path}.condition"), depth + 1, errors);
        }
        Condition::Sequence { steps, max_bars, expires_after_bars } => {
            if expires_after_bars.is_some_and(|w| w == 0) {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "expires_after_bars must be positive".to_string(),
                });
            }
            if steps.len() < 2 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "a sequence needs at least two steps".to_string(),
                });
            }
            if *max_bars == 0 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "max_bars must be positive".to_string(),
                });
            }
            for (i, step) in steps.iter().enumerate() {
                validate_condition(&step.condition, &format!("{path}.steps[{i}]"), depth + 1, errors);
            }
        }
    }
}

/// Build the engine's config from a document's parameter overrides.
///
/// Known parameters map onto the engine's own knobs; anything not named here
/// keeps its default (validation already refused unknown names, so a param
/// that lands here is always a known one).
#[must_use]
pub fn event_config_for(
    parameters: &std::collections::HashMap<String, std::collections::HashMap<String, f64>>,
) -> analytics_core::events::EventConfig {
    let mut config = analytics_core::events::EventConfig::default();
    for params in parameters.values() {
        for (name, value) in params {
            match name.as_str() {
                "swing_lookback" => config.structure_lookback = (*value as usize).max(3),
                "spike_multiple" => {
                    config.volume_multiplier = *value;
                    config.delta_multiplier = *value;
                }
                "fvg_min_fraction" => config.fvg_min_fraction = *value,
                _ => {}
            }
        }
    }
    config
}

/// One fired signal: bar index, the direction, the structure level the stop
/// keys on, and the evidence chain (the concept ids that satisfied, in
/// order) -- a signal a consumer can explain without re-running anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// The bar the composition fired on.
    pub bar: usize,
    /// Long or short, from the document's side.
    pub long: bool,
    /// The structure stop level when the document's stop is structure-based
    /// (the satisfying event's price level).
    pub stop_level: Option<f64>,
    /// The concepts that satisfied, newest step last.
    pub evidence: Vec<String>,
}

/// Per-leaf state: the last (bar, price) a matching event satisfied it.
#[derive(Debug, Clone, Default)]
struct LeafState {
    zone: Option<(f64, f64)>,
    satisfied_at: Option<usize>,
    level: Option<f64>,
}

/// The evaluator: state over the event stream, edge-triggered. Owns its
/// document: a bot's composer lives as long as the bot does.
#[derive(Debug, Clone)]
pub struct Composer {
    doc: Composition,
    leaves: Vec<(LeafKey, LeafState)>,
    fired: bool,
    last_close: Option<f64>,
    prev_close: Option<f64>,
    bar_time: Option<i64>,
}

impl Composer {
    /// Bind to a document, by value (the evaluator outlives the caller's copy).
    #[must_use]
    pub fn new(doc: Composition) -> Self {
        let mut leaves = Vec::new();
        collect_leaves(&doc.conditions, &mut leaves);
        Self {
            doc,
            leaves: leaves.into_iter().map(|key| (key, LeafState::default())).collect(),
            fired: false,
            last_close: None,
            prev_close: None,
            bar_time: None,
        }
    }

    /// The document this composer evaluates.
    #[must_use]
    pub fn document(&self) -> &Composition {
        &self.doc
    }

    /// The newest bar's own time, for the TimeWindow condition.
    pub fn set_bar_time(&mut self, open_time: i64) {
        self.bar_time = Some(open_time);
    }

    /// The newest close the retrace and PriceVs conditions read, set by
    /// the caller with each bar's events. The previous close is tracked for
    /// the cross rule.
    pub fn set_last_close(&mut self, close: f64) {
        self.prev_close = self.last_close;
        self.last_close = Some(close);
    }

    /// Feed the events of one bar (bar index `bar`) for one timeframe
    /// (None = the document's own), then evaluate. Returns a signal exactly
    /// on the satisfaction edge.
    pub fn on_bar(&mut self, bar: usize, events: &[ConceptEvent], timeframe: Option<&str>) -> Option<Signal> {
        for event in events {
            let key = (timeframe.map(str::to_string), event.concept_id.clone());
            if let Some((_, state)) = self.leaves.iter_mut().find(|(id, _)| id == &key) {
                // The newest matching event wins the slot; an older one must
                // not overwrite it (events of one bar arrive unordered).
                state.satisfied_at = Some(bar);
                state.level = event.price_level.or(event.zone_low);
                if let (Some(lo), Some(hi)) = (event.zone_low, event.zone_high) {
                    state.zone = Some((lo, hi));
                }
            }
        }
        // The retrace latch: when the close sits inside a retrace leaf's
        // zone this bar, the leaf counts as satisfied from now on -- "after
        // price came back to the zone", latched, never "hold it every bar".
        // The retrace latch reads the CONCEPT leaf's zone and writes the
        // RETRACE leaf's satisfaction: the concept event never satisfies
        // the retrace itself.
        let mut retrace_latches: Vec<(LeafKey, usize)> = Vec::new();
        for (key, state) in self.leaves.iter() {
            if key.1.starts_with("retrace:") {
                continue;
            }
            if let Some((lo, hi)) = state.zone {
                if let Some(close) = self.last_close {
                    if close >= lo && close <= hi {
                        retrace_latches.push(((key.0.clone(), format!("retrace:{}", key.1)), bar));
                    }
                }
            }
        }
        for (key, at) in retrace_latches {
            if let Some((_, state)) = self.leaves.iter_mut().find(|(id, _)| id == &key) {
                if state.satisfied_at.is_none() {
                    state.satisfied_at = Some(at);
                }
            }
        }
        let holds = self.eval(&self.doc.conditions, bar);
        if holds && !self.fired {
            self.fired = true;
            return Some(Signal {
                bar,
                long: matches!(self.doc.side, CompositionSide::Long),
                stop_level: self.structure_level(),
                evidence: self.evidence_chain(),
            });
        }
        if !holds {
            self.fired = false;
        }
        None
    }

    /// The leaf states as the tree needs them: (concept id, satisfied,
    /// level) with the lookback applied per condition at eval time.
    fn leaf(&self, key: &LeafKey, lookback: usize, bar: usize) -> Option<(usize, Option<f64>)> {
        self.leaves
            .iter()
            .find(|(id, _)| id == key)
            .and_then(|(_, state)| state.satisfied_at.filter(|at| bar - at <= lookback).map(|at| (at, state.level)))
    }

    /// The newest zone of a leaf: (its bar, low, high) when it has one.
    fn leaf_zone(&self, key: &LeafKey, lookback: usize, bar: usize) -> Option<(usize, f64, f64)> {
        self.leaves
            .iter()
            .find(|(id, _)| id == key)
            .and_then(|(_, state)| {
                state
                    .satisfied_at
                    .filter(|at| bar - at <= lookback)
                    .and_then(|at| state.zone.map(|(lo, hi)| (at, lo, hi)))
            })
    }

    fn direction_matches(event_dir: Direction, req: DirectionReq) -> bool {
        match req {
            DirectionReq::Any => true,
            DirectionReq::Bullish => event_dir == Direction::Bullish,
            DirectionReq::Bearish => event_dir == Direction::Bearish,
        }
    }

    fn eval(&self, cond: &Condition, bar: usize) -> bool {
        match cond {
            Condition::Concept { concept, timeframe, lookback_bars, min_bars_since, .. } => {
                let lookback = lookback_bars.unwrap_or(DEFAULT_LOOKBACK_BARS);
                let min_age = min_bars_since.unwrap_or(0);
                match self.leaf(&(timeframe.clone(), concept.clone()), lookback, bar) {
                    Some((at, _)) => bar - at >= min_age && bar - at <= lookback,
                    None => false,
                }
            }
            Condition::Retrace { into, timeframe, lookback_bars } => {
                let lookback = lookback_bars.unwrap_or(20);
                // The retrace LATCHES: the bar the close first entered the
                // zone is the event, and it counts for the lookback's whole
                // window -- "after price came back to the FVG" latched, never
                // "hold it every bar or forget it".
                let key = (timeframe.clone(), format!("retrace:{into}"));
                if let Some((at, _)) = self.leaf(&key, lookback, bar) {
                    return bar - at <= lookback;
                }
                match self.leaf_zone(&(timeframe.clone(), into.clone()), usize::MAX, bar) {
                    Some((_, lo, hi)) => {
                        let close = self.last_close.unwrap_or(f64::NAN);
                        close >= lo && close <= hi
                    }
                    None => false,
                }
            }
            Condition::PriceVs { side, concept, timeframe, lookback_bars, tolerance } => {
                let lookback = lookback_bars.unwrap_or(20);
                let key = (timeframe.clone(), concept.clone());
                match self.leaf(&key, lookback, bar) {
                    Some((_, Some(level))) => {
                        let close = self.last_close.unwrap_or(f64::NAN);
                        let tol = level.abs() * tolerance;
                        match side {
                            PriceSide::Above => close >= level - tol,
                            PriceSide::Below => close <= level + tol,
                            PriceSide::Cross => {
                                let prev_close = self.prev_close.unwrap_or(close);
                                (prev_close - level).signum() != (close - level).signum()
                            }
                        }
                    }
                    _ => false,
                }
            }
            Condition::TimeWindow { start_hour, end_hour } => {
                const HOUR_NS: i64 = 3_600_000_000_000;
                const DAY_NS: i64 = 24 * HOUR_NS;
                match self.bar_time {
                    Some(t) => {
                        let hour = ((t % DAY_NS) / HOUR_NS) as u32;
                        if start_hour <= end_hour {
                            hour >= *start_hour && hour < *end_hour
                        } else {
                            hour >= *start_hour || hour < *end_hour
                        }
                    }
                    None => false,
                }
            }
            Condition::All { conditions } => conditions.iter().all(|c| self.eval(c, bar)),
            Condition::Any { conditions } => conditions.iter().any(|c| self.eval(c, bar)),
            Condition::Not { condition } => !self.eval(condition, bar),
            Condition::Sequence { steps, max_bars, expires_after_bars } => {
                self.eval_sequence_expiring(steps, *max_bars, *expires_after_bars, bar)
            }
        }
    }

    /// A sequence holds when each step satisfied, in order, each within
    /// `max_bars` of the previous step's satisfaction bar. Leaf satisfactions
    /// carry their bars; groups report the newest leaf bar inside them.
    fn eval_sequence(&self, steps: &[SequenceStep], max_bars: usize, bar: usize) -> bool {
        self.eval_sequence_expiring(steps, max_bars, None, bar)
    }

    fn eval_sequence_expiring(&self, steps: &[SequenceStep], max_bars: usize, expires: Option<usize>, bar: usize) -> bool {
        let mut previous: Option<usize> = None;
        let mut first_at: Option<usize> = None;
        for step in steps {
            let Some(at) = self.condition_bar(&step.condition, step.lookback_bars, bar) else {
                return false;
            };
            if let Some(prev) = previous {
                if at < prev || at - prev > max_bars {
                    return false;
                }
            }
            if first_at.is_none() {
                first_at = Some(at);
            }
            previous = Some(at);
        }
        // The expiry: the whole sequence must complete within
        // expires_after_bars of its first step -- "cancel if unfilled".
        if let (Some(first), Some(limit)) = (first_at, expires) {
            if bar - first > limit {
                return false;
            }
        }
        true
    }

    /// The bar a condition's newest required satisfaction sits at (for
    /// sequence ordering), or None when it does not hold.
    fn condition_bar(&self, cond: &Condition, lookback: Option<usize>, bar: usize) -> Option<usize> {
        let default = lookback.unwrap_or(DEFAULT_LOOKBACK_BARS);
        match cond {
            Condition::Concept { concept, timeframe, lookback_bars, .. } => {
                self.leaf(&(timeframe.clone(), concept.clone()), lookback_bars.or(lookback).unwrap_or(default), bar).map(|(at, _)| at)
            }
            Condition::Retrace { .. } => self.eval(cond, bar).then_some(bar),
            Condition::TimeWindow { .. } => self.eval(cond, bar).then_some(bar),
            Condition::PriceVs { concept, timeframe, lookback_bars, .. } => {
                self.leaf(&(timeframe.clone(), concept.clone()), lookback_bars.or(lookback).unwrap_or(default), bar).map(|(at, _)| at)
            }
            Condition::All { conditions } => {
                let mut newest = 0;
                for c in conditions {
                    newest = newest.max(self.condition_bar(c, lookback, bar)?);
                }
                Some(newest)
            }
            Condition::Any { conditions } => conditions
                .iter()
                .filter_map(|c| self.condition_bar(c, lookback, bar))
                .max(),
            // A `not` has no satisfaction bar of its own; for ordering it
            // contributes nothing, so report the current bar when it holds.
            Condition::Not { condition } => (!self.eval(condition, bar)).then_some(bar),
            Condition::Retrace { .. } => self.eval(cond, bar).then_some(bar),
            Condition::Sequence { steps, max_bars, expires_after_bars } => {
                self.eval_sequence_expiring(steps, *max_bars, *expires_after_bars, bar).then_some(bar)
            }
        }
    }

    /// The per-leaf state as the diagnostics report needs it: (key,
    /// satisfied_at, level, zone). The "why no fire" report is built from these.
    pub fn leaf_states(&self) -> Vec<(LeafKey, Option<usize>, Option<f64>, Option<(f64, f64)>)> {
        self.leaves
            .iter()
            .map(|(key, state)| (key.clone(), state.satisfied_at, state.level, state.zone))
            .collect()
    }

    /// The stop level for a structure stop: the newest satisfying event's
    /// level (the thing whose break made the entry).
    fn structure_level(&self) -> Option<f64> {
        self.leaves
            .iter()
            .filter_map(|(_, state)| state.level)
            .reduce(|_, newest| newest)
    }

    /// The concepts that satisfied, for the evidence chain.
    fn evidence_chain(&self) -> Vec<String> {
        self.leaves
            .iter()
            .filter(|(_, state)| state.satisfied_at.is_some())
            .map(|(key, _)| match &key.0 {
                Some(tf) => format!("{tf}:{}", key.1),
                None => key.1.clone(),
            })
            .collect()
    }
}

/// Collect every leaf concept id in the tree (deduped, stable order).
/// A leaf's own key: the timeframe it reads (None = the document's own)
/// and the concept id. Two conditions on one concept at two timeframes are
/// two leaves -- the multi-timeframe rule makes them different states.
pub type LeafKey = (Option<String>, String);

fn collect_leaves(cond: &Condition, out: &mut Vec<LeafKey>) {
    match cond {
        Condition::Concept { concept, timeframe, .. } | Condition::PriceVs { concept, timeframe, .. } => {
            let key = (timeframe.clone(), concept.clone());
            if !out.contains(&key) {
                out.push(key);
            }
        }
        Condition::Retrace { into, timeframe, .. } => {
            // The retrace's leaf keys SEPARATELY from the concept's own: a
            // 4h FVG and the retrace into it are different states (the
            // concept event must not satisfy the retrace).
            let key = (timeframe.clone(), format!("retrace:{into}"));
            if !out.contains(&key) {
                out.push(key);
            }
            // And the concept's own leaf, for the zone it carries.
            let zone_key = (timeframe.clone(), into.clone());
            if !out.contains(&zone_key) {
                out.push(zone_key);
            }
        }
        Condition::TimeWindow { .. } => {}
        Condition::All { conditions } | Condition::Any { conditions } => {
            for c in conditions {
                collect_leaves(c, out);
            }
        }
        Condition::Not { condition } => collect_leaves(condition, out),
        Condition::Sequence { steps, .. } => {
            for step in steps {
                collect_leaves(&step.condition, out);
            }
        }
    }
}

/// The multi-timeframe evaluation, one core for every caller: the document
/// (whose conditions may each carry their own timeframe), the base
/// timeframe's candles (the smallest series), and each extra timeframe's
/// candles (resampled by the caller, keyed by timeframe). Events feed the
/// composer with their timeframe tag; the tree evaluates across them, and
/// the retrace condition reads the base series' closes.
/// The evaluation's full yield: the signals and the composer's own leaf
/// states (the "why no fire" data) -- owned by no caller, shared by all.
pub struct MtfEvaluation {
    /// The fired signals.
    pub signals: Vec<Signal>,
    /// Per-leaf satisfaction state at the window's end.
    pub diagnostics: Vec<(LeafKey, Option<usize>, Option<f64>, Option<(f64, f64)>)>,
}

/// The multi-timeframe evaluation, one core for every caller: the document
/// (whose conditions may each carry their own timeframe), the base
/// timeframe's candles, and each extra timeframe's resampled series, with
/// the composer's leaf states yielded alongside the signals (the "why no
/// fire" data).
pub fn evaluate_mtf(
    doc: &Composition,
    config: &analytics_core::events::EventConfig,
    symbol: &str,
    base_timeframe: analytics_core::types::Timeframe,
    base_candles: &[analytics_core::types::Candle],
    extra_series: &[(analytics_core::types::Timeframe, Vec<analytics_core::types::Candle>)],
) -> MtfEvaluation {
    let mut composer = Composer::new(doc.clone());
    let mut signals = Vec::new();
    // Precompute each extra timeframe's per-bar events, keyed by its own
    // bar time; the base walk consumes them in time order.
    let mut extra_events: Vec<(String, Vec<(i64, Vec<ConceptEvent>)>)> = Vec::new();
    for (tf, candles) in extra_series {
        let mut per_bar = Vec::new();
        for bar in 22..candles.len() {
            let raw = analytics_core::events::detect_events(&candles[..=bar], symbol, config);
            let events: Vec<ConceptEvent> = raw
                .iter()
                .filter(|e| e.bar_time == candles[bar].open_time)
                .filter_map(crate::concepts::as_concept_event)
                .collect();
            per_bar.push((candles[bar].open_time, events));
        }
        extra_events.push((tf.to_string(), per_bar));
    }
    let mut cursors: Vec<usize> = extra_events.iter().map(|_| 0).collect();
    for bar in 22..base_candles.len() {
        let bar_time = base_candles[bar].open_time;
        for (ei, (_, per_bar)) in extra_events.iter().enumerate() {
            let mut due: Vec<ConceptEvent> = Vec::new();
            while cursors[ei] < per_bar.len() && per_bar[cursors[ei]].0 <= bar_time {
                due.extend(per_bar[cursors[ei]].1.iter().cloned());
                cursors[ei] += 1;
            }
            if !due.is_empty() {
                let tf_name = extra_events[ei].0.clone();
                let _ = composer.on_bar(bar, &due, Some(&tf_name));
            }
        }
        composer.set_last_close(base_candles[bar].close);
        let raw = analytics_core::events::detect_events(&base_candles[..=bar], symbol, config);
        let events: Vec<ConceptEvent> = raw
            .iter()
            .filter(|e| e.bar_time == base_candles[bar].open_time)
            .filter_map(crate::concepts::as_concept_event)
            .collect();
        if let Some(signal) = composer.on_bar(bar, &events, None) {
            signals.push(signal);
        }
    }
    let _ = base_timeframe;
    MtfEvaluation {
        signals,
        diagnostics: composer.leaf_states(),
    }
}

/// The timeframes a document involves: its own plus every condition's
/// override, deduped.
pub fn involved_timeframes(doc: &Composition) -> Vec<String> {
    fn walk(cond: &Condition, out: &mut Vec<String>) {
        match cond {
            Condition::Concept { timeframe: Some(t), .. } | Condition::Retrace { timeframe: Some(t), .. } => {
                if !out.contains(t) {
                    out.push(t.clone());
                }
            }
            Condition::All { conditions } | Condition::Any { conditions } => {
                for c in conditions {
                    walk(c, out);
                }
            }
            Condition::Not { condition } => walk(condition, out),
            Condition::Sequence { steps, .. } => {
                for step in steps {
                    walk(&step.condition, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&doc.conditions, &mut out);
    out
}

#[cfg(test)]
mod mtf_tests {
    use super::*;
    use crate::concepts::ConceptStatus;

    fn event(concept: &str, direction: Direction, price: f64, zone: Option<(f64, f64)>) -> ConceptEvent {
        ConceptEvent {
            concept_id: concept.to_string(),
            concept_version: "1.0.0".to_string(),
            symbol: "T".to_string(),
            timeframe: "1m".to_string(),
            direction,
            status: ConceptStatus::Confirmed,
            event_time_ns: 0,
            price_level: Some(price),
            zone_low: zone.map(|z| z.0),
            zone_high: zone.map(|z| z.1),
            evidence: serde_json::json!({}),
            bars: (0, 0),
        }
    }

    #[test]
    fn the_users_scenario_4h_fvg_retrace_1m_bos() {
        // The document: a 4h FVG forms, price retraces into it, a 1m BOS
        // confirms -- the sell. A sequence across two timeframes.
        let doc: Composition = serde_json::from_value(serde_json::json!({
            "name": "mtf-fvg-bos",
            "version": "1.0.0",
            "timeframe": "1m",
            "side": "short",
            "conditions": {
                "type": "sequence",
                "max_bars": 200,
                "steps": [
                    {"condition": {"type": "concept", "concept": "smc.fvg", "direction": "bearish", "timeframe": "4h"}},
                    {"condition": {"type": "retrace", "into": "smc.fvg", "timeframe": "4h"}},
                    {"condition": {"type": "concept", "concept": "smc.bos", "direction": "bearish"}}
                ]
            },
            "risk": {"stop": {"type": "structure"}, "sizing": {"type": "fixed_fractional", "percent": 1.0}}
        }))
        .expect("the document parses");
        assert_eq!(validate(&doc), Ok(()), "the scenario validates");

        let mut composer = Composer::new(doc);
        // The 4h FVG fires (its zone 100..101), fed with its timeframe tag.
        let _ = composer.on_bar(10, &[event("smc.fvg", Direction::Bearish, 100.5, Some((100.0, 101.0)))], Some("4h"));
        assert_eq!(composer.on_bar(11, &[], None), None, "no signal on the FVG alone");
        // The retrace: the close lands inside the zone (100.5).
        composer.set_last_close(100.5);
        assert_eq!(composer.on_bar(12, &[], None), None, "retraced, but the 1m confirmation has not come");
        // The close leaves the zone before the BOS: the retrace's lookback
        // is per-bar state, not latched -- the sequence needs the 1m BOS
        // while the close still sits inside (or recently sat inside). The
        // BOS fires on the 1m series (None = the document's timeframe).
        let fired = composer.on_bar(13, &[event("smc.bos", Direction::Bearish, 100.2, None)], None);
        assert!(fired.is_some(), "the full sequence fires the sell");
        let signal = fired.expect("checked");
        assert!(!signal.long, "a sell, as the user defined it");
    }

    #[test]
    fn the_retrace_needs_the_close_inside_the_zone() {
        let doc: Composition = serde_json::from_value(serde_json::json!({
            "name": "retrace-test",
            "version": "1.0.0",
            "timeframe": "1m",
            "side": "short",
            "conditions": {"type": "retrace", "into": "smc.fvg", "timeframe": "4h"},
            "risk": {"stop": {"type": "structure"}, "sizing": {"type": "fixed_fractional", "percent": 1.0}}
        }))
        .expect("parses");
        let mut composer = Composer::new(doc);
        let _ = composer.on_bar(10, &[event("smc.fvg", Direction::Bearish, 100.5, Some((100.0, 101.0)))], Some("4h"));
        composer.set_last_close(102.0);
        assert!(composer.on_bar(11, &[], None).is_none(), "outside the zone is no retrace");
        composer.set_last_close(100.5);
        assert!(composer.on_bar(12, &[], None).is_some(), "inside the zone IS the retrace");
    }
}

#[cfg(test)]
mod feature_tests {
    use super::*;
    use crate::concepts::ConceptStatus;

    fn event(concept: &str, direction: Direction, price: f64, zone: Option<(f64, f64)>) -> ConceptEvent {
        ConceptEvent {
            concept_id: concept.to_string(),
            concept_version: "1.0.0".to_string(),
            symbol: "T".to_string(),
            timeframe: "1m".to_string(),
            direction,
            status: ConceptStatus::Confirmed,
            event_time_ns: 0,
            price_level: Some(price),
            zone_low: zone.map(|z| z.0),
            zone_high: zone.map(|z| z.1),
            evidence: serde_json::json!({}),
            bars: (0, 0),
        }
    }

    fn doc_with(cond: Condition) -> Composition {
        Composition {
            name: "t".to_string(),
            version: "1.0.0".to_string(),
            timeframe: "1m".to_string(),
            market: None,
            side: CompositionSide::Long,
            conditions: cond,
            risk: RiskBlock {
                stop: StopKind::Structure,
                sizing: SizeKind::Fixed { quantity: 1.0 },
                management: None,
                guards: None,
                exit: None,
                partials: Vec::new(),
                entry: None,
            },
            parameters: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn price_vs_reads_the_leafs_own_level() {
        let doc = doc_with(Condition::PriceVs {
            side: PriceSide::Above,
            concept: "smc.bos".to_string(),
            timeframe: None,
            lookback_bars: None,
            tolerance: 0.0,
        });
        let mut composer = Composer::new(doc);
        let _ = composer.on_bar(5, &[event("smc.bos", Direction::Bullish, 100.0, None)], None);
        composer.set_last_close(99.0);
        assert!(composer.on_bar(6, &[], None).is_none(), "below the level fails");
        composer.set_last_close(101.0);
        assert!(composer.on_bar(7, &[], None).is_some(), "above the level holds");
    }

    #[test]
    fn min_bars_since_delays_the_confirmation() {
        let doc = doc_with(Condition::Concept {
            concept: "smc.bos".to_string(),
            direction: DirectionReq::Bullish,
            lookback_bars: Some(10),
            timeframe: None,
            min_bars_since: Some(5),
        });
        let mut composer = Composer::new(doc);
        let _ = composer.on_bar(5, &[event("smc.bos", Direction::Bullish, 100.0, None)], None);
        assert!(composer.on_bar(8, &[], None).is_none(), "3 bars is too fresh");
        assert!(composer.on_bar(11, &[], None).is_some(), "6 bars is old enough");
    }

    #[test]
    fn the_time_window_gates_the_entry_hour() {
        let doc = doc_with(Condition::All {
            conditions: vec![
                Condition::Concept {
                    concept: "smc.bos".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: Some(10),
                    timeframe: None,
                    min_bars_since: None,
                },
                Condition::TimeWindow { start_hour: 12, end_hour: 15 },
            ],
        });
        let mut composer = Composer::new(doc);
        let _ = composer.on_bar(5, &[event("smc.bos", Direction::Bullish, 100.0, None)], None);
        // 04:00 UTC: outside the killzone.
        composer.set_bar_time(4 * 3_600_000_000_000);
        assert!(composer.on_bar(6, &[], None).is_none(), "outside the window fails");
        // 13:30 UTC: inside.
        composer.set_bar_time(13 * 3_600_000_000_000 + 30 * 60_000_000_000);
        assert!(composer.on_bar(7, &[], None).is_some(), "inside the window holds");
    }

    #[test]
    fn the_sequence_expires_unless_it_completes_in_time() {
        let doc = doc_with(Condition::Sequence {
            steps: vec![
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.liquidity_sweep".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                        timeframe: None,
                        min_bars_since: None,
                    },
                    lookback_bars: Some(50),
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                        timeframe: None,
                        min_bars_since: None,
                    },
                    lookback_bars: Some(50),
                },
            ],
            max_bars: 100,
            expires_after_bars: Some(10),
        });
        let mut composer = Composer::new(doc);
        let _ = composer.on_bar(5, &[event("smc.liquidity_sweep", Direction::Bullish, 100.0, None)], None);
        // The confirmation at bar 18: 13 bars after the sweep -- expired.
        let fired = composer.on_bar(18, &[event("smc.bos", Direction::Bullish, 100.0, None)], None);
        assert!(fired.is_none(), "past the expiry, the sequence is dead");
    }

    #[test]
    fn warnings_name_contradictions_and_duplicates() {
        let doc = doc_with(Condition::All {
            conditions: vec![
                Condition::Concept {
                    concept: "smc.bos".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: None,
                    timeframe: None,
                    min_bars_since: None,
                },
                Condition::Concept {
                    concept: "smc.bos".to_string(),
                    direction: DirectionReq::Bearish,
                    lookback_bars: None,
                    timeframe: None,
                    min_bars_since: None,
                },
            ],
        });
        let w = warnings(&doc);
        assert!(w.iter().any(|m| m.contains("contradiction")), "the contradiction is named: {w:?}");
        let dup = doc_with(Condition::All {
            conditions: vec![
                Condition::Concept {
                    concept: "indicators.rsi".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: None,
                    timeframe: None,
                    min_bars_since: None,
                },
                Condition::Concept {
                    concept: "indicators.stochastic".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: None,
                    timeframe: None,
                    min_bars_since: None,
                },
            ],
        });
        let w = warnings(&dup);
        assert!(w.iter().any(|m| m.contains("duplicate measurement")), "the overlap is named: {w:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concepts::ConceptStatus;

    fn doc(conditions: Condition) -> Composition {
        Composition {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            timeframe: "15m".to_string(),
            market: None,
            side: CompositionSide::Long,
            conditions,
            risk: RiskBlock {
                stop: StopKind::Structure,
                sizing: SizeKind::Fixed { quantity: 1.0 },
                management: None,
                guards: None,
                exit: None,
                partials: Vec::new(),
                entry: None,
            },
            parameters: std::collections::HashMap::new(),
        }
    }

    fn event(concept: &str, direction: Direction, level: f64) -> ConceptEvent {
        ConceptEvent {
            concept_id: concept.to_string(),
            concept_version: "1.0.0".to_string(),
            symbol: "T".to_string(),
            timeframe: "15m".to_string(),
            direction,
            status: ConceptStatus::Confirmed,
            event_time_ns: 0,
            price_level: Some(level),
            zone_low: None,
            zone_high: None,
            evidence: serde_json::json!({}),
            bars: (0, 0),
        }
    }

    #[test]
    fn validation_names_every_bad_piece() {
        let bad = Composition {
            name: " ".to_string(),
            timeframe: "9m".to_string(),
            ..doc(Condition::Concept {
                concept: "smc.nope".to_string(),
                direction: DirectionReq::Bullish,
                lookback_bars: Some(0),
                min_bars_since: None,
                timeframe: None,
            })
        };
        let errors = validate(&bad).expect_err("invalid");
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"name"), "{paths:?}");
        assert!(paths.contains(&"timeframe"), "{paths:?}");
        assert!(paths.contains(&"conditions"), "{paths:?}");
        // The good document passes clean.
        let good = doc(Condition::Sequence {
            steps: vec![
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.liquidity_sweep".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                        min_bars_since: None,
                        timeframe: None,
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                        min_bars_since: None,
                        timeframe: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 10,
            expires_after_bars: None,
        });
        assert!(validate(&good).is_ok());
    }

    #[test]
    fn a_leaf_holds_within_its_lookback_and_expires_after_it() {
        let document = doc(Condition::Concept {
            concept: "smc.bos".to_string(),
            direction: DirectionReq::Bullish,
            lookback_bars: Some(3),
                min_bars_since: None,
                timeframe: None,
        });
        let mut composer = Composer::new(document.clone());
        assert!(composer.on_bar(0, &[event("smc.bos", Direction::Bullish, 100.0)], None).is_some());
        // Edge-triggered: the same bar must not fire twice.
        assert!(composer.on_bar(0, &[], None).is_none());
        assert!(composer.on_bar(3, &[], None).is_none(), "still inside the lookback, already fired");
        // The lookback lapses without a fresh event.
        for bar in 4..=4 {
            assert!(composer.on_bar(bar, &[], None).is_none());
        }
        // A fresh event re-arms it.
        assert!(composer.on_bar(5, &[event("smc.bos", Direction::Bullish, 105.0)], None).is_some());
    }

    #[test]
    fn a_sequence_requires_order_and_the_window() {
        let document = doc(Condition::Sequence {
            steps: vec![
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.liquidity_sweep".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                min_bars_since: None,
                timeframe: None,
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                min_bars_since: None,
                timeframe: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 3,
            expires_after_bars: None,
        });
        // Wrong order: bos before the sweep never fires.
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.bos", Direction::Bullish, 100.0)], None);
        assert!(composer.on_bar(1, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)], None).is_none());
        // Right order inside the window: sweep at 0, bos at 2 (gap 2 <= 3).
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)], None);
        assert!(composer.on_bar(2, &[event("smc.bos", Direction::Bullish, 100.0)], None).is_some());
        // Right order but too wide: gap 4 > 3.
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)], None);
        assert!(composer.on_bar(4, &[event("smc.bos", Direction::Bullish, 100.0)], None).is_none());
    }

    #[test]
    fn groups_compose_and_not_vetoes_only_while_fresh() {
        let document = doc(Condition::All {
            conditions: vec![
                Condition::Concept {
                    concept: "smc.bos".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: Some(5),
                min_bars_since: None,
                timeframe: None,
                },
                Condition::Not {
                    condition: Box::new(Condition::Concept {
                        concept: "volume.spike".to_string(),
                        direction: DirectionReq::Any,
                        lookback_bars: Some(2),
                min_bars_since: None,
                timeframe: None,
                    }),
                },
            ],
        });
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("volume.spike", Direction::Neutral, 100.0)], None);
        // The spike vetoes within its lookback.
        assert!(composer.on_bar(1, &[event("smc.bos", Direction::Bullish, 100.0)], None).is_none());
        // The spike's lookback (2 bars) lapses at bar 4 while the bos's (5)
        // still holds -- so the veto lifts and the group fires here.
        assert!(composer.on_bar(4, &[], None).is_some(), "the lapsed veto lets the bos through");
    }

    #[test]
    fn parameter_overrides_validate_and_reach_the_engine_config() {
        let mut document = doc(Condition::Concept {
            concept: "smc.bos".to_string(),
            direction: DirectionReq::Bullish,
            lookback_bars: None,
                min_bars_since: None,
                timeframe: None,
        });
        // A trader's own swing lookback validates and lands on the engine knob.
        document.parameters.insert(
            "smc.bos".to_string(),
            [("swing_lookback".to_string(), 5.0)].into_iter().collect(),
        );
        assert!(validate(&document).is_ok());
        let config = event_config_for(&document.parameters);
        assert_eq!(config.structure_lookback, 5, "the override reached the engine");
        // A spike override hits both spike thresholds.
        document.parameters.insert(
            "volume.spike".to_string(),
            [("spike_multiple".to_string(), 4.0)].into_iter().collect(),
        );
        let config = event_config_for(&document.parameters);
        assert_eq!(config.volume_multiplier, 4.0);
        assert_eq!(config.delta_multiplier, 4.0);
        // Out of range: refused with the parameter's own limits.
        document.parameters.insert(
            "smc.bos".to_string(),
            [("swing_lookback".to_string(), 99.0)].into_iter().collect(),
        );
        let errors = validate(&document).expect_err("out of range");
        assert!(errors.iter().any(|e| e.path.contains("swing_lookback")), "{errors:?}");
        // An unknown parameter: refused by name.
        document.parameters.insert(
            "smc.bos".to_string(),
            [("wick_mode".to_string(), 1.0)].into_iter().collect(),
        );
        let errors = validate(&document).expect_err("unknown param");
        assert!(errors.iter().any(|e| e.message.contains("not a parameter")), "{errors:?}");
    }

    #[test]
    fn the_signal_carries_the_structure_level_and_evidence() {
        let document = doc(Condition::Sequence {
            steps: vec![
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.liquidity_sweep".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                min_bars_since: None,
                timeframe: None,
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                min_bars_since: None,
                timeframe: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 5,
            expires_after_bars: None,
        });
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 98.5)], None);
        let signal = composer
            .on_bar(1, &[event("smc.bos", Direction::Bullish, 101.0)], None)
            .expect("fires");
        assert_eq!(signal.bar, 1);
        assert!(signal.long);
        assert_eq!(signal.stop_level, Some(101.0), "the newest satisfying level");
        assert!(signal.evidence.contains(&"smc.bos".to_string()));
        assert!(signal.evidence.contains(&"smc.liquidity_sweep".to_string()));
    }
}
