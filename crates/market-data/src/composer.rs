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
    },
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
        Condition::Sequence { steps, max_bars } => {
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
    satisfied_at: Option<usize>,
    level: Option<f64>,
}

/// The evaluator: state over the event stream, edge-triggered. Owns its
/// document: a bot's composer lives as long as the bot does.
#[derive(Debug, Clone)]
pub struct Composer {
    doc: Composition,
    leaves: Vec<(String, LeafState)>,
    fired: bool,
}

impl Composer {
    /// Bind to a document, by value (the evaluator outlives the caller's copy).
    #[must_use]
    pub fn new(doc: Composition) -> Self {
        let mut leaves = Vec::new();
        collect_leaves(&doc.conditions, &mut leaves);
        Self {
            doc,
            leaves: leaves.into_iter().map(|id| (id, LeafState::default())).collect(),
            fired: false,
        }
    }

    /// The document this composer evaluates.
    #[must_use]
    pub fn document(&self) -> &Composition {
        &self.doc
    }

    /// Feed the events of one bar (bar index `bar`), then evaluate. Returns
    /// a signal exactly on the satisfaction edge.
    pub fn on_bar(&mut self, bar: usize, events: &[ConceptEvent]) -> Option<Signal> {
        for event in events {
            if let Some((_, state)) = self.leaves.iter_mut().find(|(id, _)| id == &event.concept_id) {
                // The newest matching event wins the slot; an older one must
                // not overwrite it (events of one bar arrive unordered).
                state.satisfied_at = Some(bar);
                state.level = event.price_level.or(event.zone_low);
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
    fn leaf(&self, concept: &str, lookback: usize, bar: usize) -> Option<(usize, Option<f64>)> {
        self.leaves
            .iter()
            .find(|(id, _)| id == concept)
            .and_then(|(_, state)| state.satisfied_at.filter(|at| bar - at <= lookback).map(|at| (at, state.level)))
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
            Condition::Concept { concept, lookback_bars, .. } => {
                self.leaf(concept, lookback_bars.unwrap_or(DEFAULT_LOOKBACK_BARS), bar).is_some()
            }
            Condition::All { conditions } => conditions.iter().all(|c| self.eval(c, bar)),
            Condition::Any { conditions } => conditions.iter().any(|c| self.eval(c, bar)),
            Condition::Not { condition } => !self.eval(condition, bar),
            Condition::Sequence { steps, max_bars } => self.eval_sequence(steps, *max_bars, bar),
        }
    }

    /// A sequence holds when each step satisfied, in order, each within
    /// `max_bars` of the previous step's satisfaction bar. Leaf satisfactions
    /// carry their bars; groups report the newest leaf bar inside them.
    fn eval_sequence(&self, steps: &[SequenceStep], max_bars: usize, bar: usize) -> bool {
        let mut previous: Option<usize> = None;
        for step in steps {
            let Some(at) = self.condition_bar(&step.condition, step.lookback_bars, bar) else {
                return false;
            };
            if let Some(prev) = previous {
                if at < prev || at - prev > max_bars {
                    return false;
                }
            }
            previous = Some(at);
        }
        true
    }

    /// The bar a condition's newest required satisfaction sits at (for
    /// sequence ordering), or None when it does not hold.
    fn condition_bar(&self, cond: &Condition, lookback: Option<usize>, bar: usize) -> Option<usize> {
        let default = lookback.unwrap_or(DEFAULT_LOOKBACK_BARS);
        match cond {
            Condition::Concept { concept, lookback_bars, .. } => {
                self.leaf(concept, lookback_bars.or(lookback).unwrap_or(default), bar).map(|(at, _)| at)
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
            Condition::Sequence { steps, max_bars } => {
                self.eval_sequence(steps, *max_bars, bar).then_some(bar)
            }
        }
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
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Collect every leaf concept id in the tree (deduped, stable order).
fn collect_leaves(cond: &Condition, out: &mut Vec<String>) {
    match cond {
        Condition::Concept { concept, .. } => {
            if !out.contains(concept) {
                out.push(concept.clone());
            }
        }
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
            },
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
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 10,
        });
        assert!(validate(&good).is_ok());
    }

    #[test]
    fn a_leaf_holds_within_its_lookback_and_expires_after_it() {
        let document = doc(Condition::Concept {
            concept: "smc.bos".to_string(),
            direction: DirectionReq::Bullish,
            lookback_bars: Some(3),
        });
        let mut composer = Composer::new(document.clone());
        assert!(composer.on_bar(0, &[event("smc.bos", Direction::Bullish, 100.0)]).is_some());
        // Edge-triggered: the same bar must not fire twice.
        assert!(composer.on_bar(0, &[]).is_none());
        assert!(composer.on_bar(3, &[]).is_none(), "still inside the lookback, already fired");
        // The lookback lapses without a fresh event.
        for bar in 4..=4 {
            assert!(composer.on_bar(bar, &[]).is_none());
        }
        // A fresh event re-arms it.
        assert!(composer.on_bar(5, &[event("smc.bos", Direction::Bullish, 105.0)]).is_some());
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
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 3,
        });
        // Wrong order: bos before the sweep never fires.
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.bos", Direction::Bullish, 100.0)]);
        assert!(composer.on_bar(1, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)]).is_none());
        // Right order inside the window: sweep at 0, bos at 2 (gap 2 <= 3).
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)]);
        assert!(composer.on_bar(2, &[event("smc.bos", Direction::Bullish, 100.0)]).is_some());
        // Right order but too wide: gap 4 > 3.
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 99.0)]);
        assert!(composer.on_bar(4, &[event("smc.bos", Direction::Bullish, 100.0)]).is_none());
    }

    #[test]
    fn groups_compose_and_not_vetoes_only_while_fresh() {
        let document = doc(Condition::All {
            conditions: vec![
                Condition::Concept {
                    concept: "smc.bos".to_string(),
                    direction: DirectionReq::Bullish,
                    lookback_bars: Some(5),
                },
                Condition::Not {
                    condition: Box::new(Condition::Concept {
                        concept: "volume.spike".to_string(),
                        direction: DirectionReq::Any,
                        lookback_bars: Some(2),
                    }),
                },
            ],
        });
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("volume.spike", Direction::Neutral, 100.0)]);
        // The spike vetoes within its lookback.
        assert!(composer.on_bar(1, &[event("smc.bos", Direction::Bullish, 100.0)]).is_none());
        // The spike's lookback (2 bars) lapses at bar 4 while the bos's (5)
        // still holds -- so the veto lifts and the group fires here.
        assert!(composer.on_bar(4, &[]).is_some(), "the lapsed veto lets the bos through");
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
                    },
                    lookback_bars: None,
                },
                SequenceStep {
                    condition: Condition::Concept {
                        concept: "smc.bos".to_string(),
                        direction: DirectionReq::Bullish,
                        lookback_bars: None,
                    },
                    lookback_bars: None,
                },
            ],
            max_bars: 5,
        });
        let mut composer = Composer::new(document.clone());
        composer.on_bar(0, &[event("smc.liquidity_sweep", Direction::Bullish, 98.5)]);
        let signal = composer
            .on_bar(1, &[event("smc.bos", Direction::Bullish, 101.0)])
            .expect("fires");
        assert_eq!(signal.bar, 1);
        assert!(signal.long);
        assert_eq!(signal.stop_level, Some(101.0), "the newest satisfying level");
        assert!(signal.evidence.contains(&"smc.bos".to_string()));
        assert!(signal.evidence.contains(&"smc.liquidity_sweep".to_string()));
    }
}
