//! The strategy document schema (`docs/06-STRATEGY-DSL.md`).
//!
//! One declarative representation of trading logic, producible by the AI agent,
//! the visual builder, or a developer, and executable unchanged as an indicator
//! overlay, a backtest, a paper bot or a live bot.
//!
//! ## `deny_unknown_fields` everywhere
//!
//! Every struct here denies unknown fields. That is deliberate: the AI agent
//! emits these documents, and a hallucinated field must fail loudly at parse
//! time rather than be silently ignored. A dropped field is a silently
//! different strategy, which is the worst possible failure mode -- the
//! backtest would still produce a confident-looking number.
//!
//! ## Deviations from the illustrative snippet in `docs/06`
//!
//! The snippet in the spec is a sketch; three things had to be pinned down to
//! make it executable. All three are recorded in `docs/06-STRATEGY-DSL.md`:
//!
//! 1. **`entry.direction`** did not exist in the snippet. It is now an optional
//!    field that, when omitted, is derived from the stop rule's side -- every
//!    stop rule is either "below X" (long) or "above X" (short). Supplying it
//!    explicitly is allowed, but a contradiction is a validation error. It is
//!    never guessed.
//! 2. **`risk.stop`** accepted only a bare string in the snippet. It now also
//!    accepts a mapping for rules that take parameters
//!    (`{kind: atr, multiple: 1.5, period: 14}`); the bare-string form still
//!    round-trips exactly.
//! 3. **`kind: indicator`** documents are allowed to omit `entry`/`risk`/
//!    `invalidation`, because an indicator has no trade logic. The validator
//!    enforces that the presence of those blocks matches `kind`.

use std::collections::BTreeMap;
use std::fmt;

use analytics_core::Timeframe;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a document is for. Same schema, different consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// Plotted on a chart; has no entry, risk or invalidation blocks.
    Indicator,
    /// Backtestable and tradable.
    Strategy,
    /// A strategy intended to run unattended.
    Bot,
}

impl DocumentKind {
    /// Every variant, for the builder's vocabulary and exhaustive tests.
    pub const ALL: &'static [Self] = &[Self::Indicator, Self::Strategy, Self::Bot];

    /// Canonical name, as written in a document.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Indicator => "indicator",
            Self::Strategy => "strategy",
            Self::Bot => "bot",
        }
    }
}

impl fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Buy / long. Stops sit below the entry.
    Long,
    /// Sell / short. Stops sit above the entry.
    Short,
}

impl Direction {
    /// Both directions, for the builder's vocabulary.
    pub const ALL: &'static [Self] = &[Self::Long, Self::Short];

    /// Canonical name, as written in a document.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who produced the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedBy {
    /// Generated from natural language by the AI agent.
    AiAgent,
    /// Composed in the visual no-code builder.
    VisualBuilder,
    /// Written directly by a developer.
    DeveloperSdk,
}

/// Provenance and human-facing notes. Never affects execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    /// Which of the three creation modes produced this document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<CreatedBy>,
    /// Versioned Skill this document was derived from, if any
    /// (see `docs/11-SKILLS-SYSTEM.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_ref: Option<String>,
    /// Free-text explanation, surfaced in the UI and to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One condition, evaluated against one declared timeframe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditional {
    /// Name of a timeframe declared in [`StrategyDocument::timeframes`].
    pub timeframe: String,
    /// Condition source, parsed by [`crate::expr`].
    ///
    /// Kept as a string in the schema on purpose: parsing happens in the
    /// validator, so a malformed expression produces a field-level
    /// [`ValidationIssue`](crate::error::ValidationIssue) naming this exact
    /// condition, rather than an opaque serde error.
    pub condition: String,
    /// Optional human-readable label, carried into the trade log so a losing
    /// trade can be explained after the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Where a stop loss sits.
///
/// Every variant is either below the entry (long) or above it (short); see
/// [`StopSpec::direction`]. That is what lets `entry.direction` be optional
/// without ever being ambiguous.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopSpec {
    /// Just beyond the most recently swept low -- the sweep that set up the
    /// trade is the level that invalidates it.
    BelowSweepLow,
    /// Just beyond the most recently swept high.
    AboveSweepHigh,
    /// Below the most recent confirmed swing low.
    BelowSwingLow,
    /// Above the most recent confirmed swing high.
    AboveSwingHigh,
    /// Below the lowest low of the last `bars` candles.
    BelowRecentLow {
        /// Lookback in candles.
        bars: usize,
    },
    /// Above the highest high of the last `bars` candles.
    AboveRecentHigh {
        /// Lookback in candles.
        bars: usize,
    },
    /// A multiple of ATR away from the entry, on the losing side.
    Atr {
        /// Distance in ATRs, e.g. `1.5`.
        multiple: f64,
        /// ATR period.
        period: usize,
    },
    /// An absolute price.
    Fixed {
        /// The stop price.
        price: f64,
    },
}

impl StopSpec {
    /// Which direction of trade this stop implies.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        match self {
            Self::BelowSweepLow | Self::BelowSwingLow | Self::BelowRecentLow { .. } => {
                Direction::Long
            }
            Self::AboveSweepHigh | Self::AboveSwingHigh | Self::AboveRecentHigh { .. } => {
                Direction::Short
            }
            // ATR and fixed stops carry no side; the explicit `direction`
            // field is required for these.
            Self::Atr { .. } | Self::Fixed { .. } => Direction::Long,
        }
    }

    /// Whether the side of this stop is implied by its rule, as opposed to
    /// needing an explicit `direction`.
    #[must_use]
    pub const fn implies_direction(&self) -> bool {
        !matches!(self, Self::Atr { .. } | Self::Fixed { .. })
    }

    /// Canonical name, as it appears in YAML/JSON.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::BelowSweepLow => "below_sweep_low",
            Self::AboveSweepHigh => "above_sweep_high",
            Self::BelowSwingLow => "below_swing_low",
            Self::AboveSwingHigh => "above_swing_high",
            Self::BelowRecentLow { .. } => "below_recent_low",
            Self::AboveRecentHigh { .. } => "above_recent_high",
            Self::Atr { .. } => "atr",
            Self::Fixed { .. } => "fixed",
        }
    }
}

/// The wire form. Serializing as an untagged enum lets the unit variants come
/// out as bare strings, exactly as the spec's YAML shows them.
#[derive(Serialize)]
#[serde(untagged)]
enum StopRepr<'a> {
    Named(&'a str),
    Detailed {
        kind: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        bars: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        multiple: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        period: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        price: Option<f64>,
    },
}

impl Serialize for StopSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let detailed = |kind: &'static str,
                        bars: Option<usize>,
                        multiple: Option<f64>,
                        period: Option<usize>,
                        price: Option<f64>| {
            StopRepr::Detailed {
                kind,
                bars,
                multiple,
                period,
                price,
            }
        };

        let repr = match *self {
            Self::BelowSweepLow
            | Self::AboveSweepHigh
            | Self::BelowSwingLow
            | Self::AboveSwingHigh => StopRepr::Named(self.kind_name()),
            Self::BelowRecentLow { bars } => {
                detailed("below_recent_low", Some(bars), None, None, None)
            }
            Self::AboveRecentHigh { bars } => {
                detailed("above_recent_high", Some(bars), None, None, None)
            }
            Self::Atr { multiple, period } => {
                detailed("atr", None, Some(multiple), Some(period), None)
            }
            Self::Fixed { price } => detailed("fixed", None, None, None, Some(price)),
        };

        repr.serialize(serializer)
    }
}

/// The mapping form, deserialized with `deny_unknown_fields` so a misspelled
/// parameter is reported rather than dropped.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopMap {
    kind: String,
    #[serde(default)]
    bars: Option<usize>,
    #[serde(default)]
    multiple: Option<f64>,
    #[serde(default)]
    period: Option<usize>,
    #[serde(default)]
    price: Option<f64>,
}

impl StopMap {
    fn build(self) -> Result<StopSpec, String> {
        match self.kind.as_str() {
            "below_sweep_low" => Ok(StopSpec::BelowSweepLow),
            "above_sweep_high" => Ok(StopSpec::AboveSweepHigh),
            "below_swing_low" => Ok(StopSpec::BelowSwingLow),
            "above_swing_high" => Ok(StopSpec::AboveSwingHigh),
            "below_recent_low" => self
                .bars
                .map(|bars| StopSpec::BelowRecentLow { bars })
                .ok_or_else(|| "`below_recent_low` requires `bars`".to_string()),
            "above_recent_high" => self
                .bars
                .map(|bars| StopSpec::AboveRecentHigh { bars })
                .ok_or_else(|| "`above_recent_high` requires `bars`".to_string()),
            "atr" => match (self.multiple, self.period) {
                (Some(multiple), Some(period)) => Ok(StopSpec::Atr { multiple, period }),
                _ => Err("`atr` requires `multiple` and `period`".to_string()),
            },
            "fixed" => self
                .price
                .map(|price| StopSpec::Fixed { price })
                .ok_or_else(|| "`fixed` requires `price`".to_string()),
            other => Err(format!(
                "unknown stop kind `{other}`; expected one of below_sweep_low, \
                 above_sweep_high, below_swing_low, above_swing_high, below_recent_low, \
                 above_recent_high, atr, fixed"
            )),
        }
    }
}

/// Accepts either a bare string (`below_sweep_low`) or a mapping with `kind`.
struct StopVisitor;

impl<'de> Visitor<'de> for StopVisitor {
    type Value = StopSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a stop rule name, or a mapping with a `kind` field")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        StopMap {
            kind: v.to_string(),
            bars: None,
            multiple: None,
            period: None,
            price: None,
        }
        .build()
        .map_err(E::custom)
    }

    fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
        let parsed = StopMap::deserialize(de::value::MapAccessDeserializer::new(map))?;
        parsed.build().map_err(de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for StopSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StopVisitor)
    }
}

/// What one stop rule is called and what it needs.
///
/// `expr::ALL_FIELDS` exists so a validation message can name every field;
/// this exists so a *client* can offer every stop rule. The visual builder
/// needs the same vocabulary the parser accepts, and a hand-maintained copy of
/// it in JavaScript is a copy that drifts -- the builder would offer a rule the
/// validator then rejects, or silently omit a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StopKind {
    /// Canonical name, as written in a document.
    pub kind: &'static str,
    /// Parameters this rule takes, in order. Empty for the bare-string forms.
    pub params: &'static [&'static str],
    /// The trade direction this rule implies, when it implies one.
    ///
    /// `None` for `atr` and `fixed`: those carry no side, which is exactly why
    /// `entry.direction` is required when the stop is one of them.
    pub implies_direction: Option<Direction>,
}

/// Every stop rule, for the builder's vocabulary and exhaustive tests.
pub const ALL_STOP_KINDS: &[StopKind] = &[
    StopKind {
        kind: "below_sweep_low",
        params: &[],
        implies_direction: Some(Direction::Long),
    },
    StopKind {
        kind: "above_sweep_high",
        params: &[],
        implies_direction: Some(Direction::Short),
    },
    StopKind {
        kind: "below_swing_low",
        params: &[],
        implies_direction: Some(Direction::Long),
    },
    StopKind {
        kind: "above_swing_high",
        params: &[],
        implies_direction: Some(Direction::Short),
    },
    StopKind {
        kind: "below_recent_low",
        params: &["bars"],
        implies_direction: Some(Direction::Long),
    },
    StopKind {
        kind: "above_recent_high",
        params: &["bars"],
        implies_direction: Some(Direction::Short),
    },
    StopKind {
        kind: "atr",
        params: &["multiple", "period"],
        implies_direction: None,
    },
    StopKind {
        kind: "fixed",
        params: &["price"],
        implies_direction: None,
    },
];

/// How the take profit is calculated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeProfitKind {
    /// `value` times the distance from entry to stop.
    RiskMultiple,
    /// `value` ATRs from the entry.
    AtrMultiple,
    /// An absolute price.
    FixedPrice,
}

impl TakeProfitKind {
    /// Every variant, for the builder's vocabulary and exhaustive tests.
    pub const ALL: &'static [Self] = &[Self::RiskMultiple, Self::AtrMultiple, Self::FixedPrice];

    /// Canonical name, as written in a document.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RiskMultiple => "risk_multiple",
            Self::AtrMultiple => "atr_multiple",
            Self::FixedPrice => "fixed_price",
        }
    }
}

/// Take-profit definition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeProfit {
    /// Which calculation to use.
    #[serde(rename = "type")]
    pub kind: TakeProfitKind,
    /// The magnitude, interpreted per [`TakeProfit::kind`].
    pub value: f64,
}

/// Position sizing and exits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskBlock {
    /// Percentage of account equity risked per trade.
    ///
    /// Capped by the validator at a hard ceiling regardless of what was
    /// requested -- an AI-generated document must not be able to risk the
    /// account.
    pub max_risk_pct: f64,
    /// Where the stop sits.
    pub stop: StopSpec,
    /// Optional profit target. Without one, trades exit only on stop,
    /// invalidation or an explicit exit condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_profit: Option<TakeProfit>,
}

/// The entry rule set.
///
/// `all_of` must all hold; `any_of` needs at least one. When both are present
/// they are ANDed together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryBlock {
    /// Explicit direction. Optional -- when omitted it is derived from the
    /// stop rule's side, and a contradiction is a validation error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Every condition must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_of: Vec<Conditional>,
    /// At least one condition must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<Conditional>,
}

/// Conditions that close an open position early.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitBlock {
    /// Every condition must hold to exit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_of: Vec<Conditional>,
    /// At least one condition must hold to exit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<Conditional>,
}

impl ExitBlock {
    /// Whether this block contains any condition at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all_of.is_empty() && self.any_of.is_empty()
    }
}

/// A complete strategy document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDocument {
    /// Human-readable name.
    pub name: String,
    /// Document version, e.g. `"2.1"`.
    pub version: String,
    /// What this document is for.
    pub kind: DocumentKind,
    /// Symbol, e.g. `BTCUSDT`.
    pub market: String,
    /// Named timeframes. Conditions reference these names, never raw strings.
    ///
    /// A `BTreeMap` rather than a `HashMap` so serialization and iteration are
    /// deterministic -- a document that round-trips must come back byte-stable.
    pub timeframes: BTreeMap<String, Timeframe>,
    /// Entry rules. Required unless `kind` is `indicator`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryBlock>,
    /// Sizing and exits. Required unless `kind` is `indicator`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskBlock>,
    /// Conditions that invalidate the setup. Must be non-empty for a tradable
    /// document -- a strategy with no invalidation can only ever exit on its
    /// stop or target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidation: Vec<Conditional>,
    /// Optional additional exit conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitBlock>,
    /// Provenance. Never affects execution.
    #[serde(default)]
    pub metadata: Metadata,
}

impl StrategyDocument {
    /// Every condition in the document, tagged with its JSON path.
    ///
    /// Used by the validator to check each expression, and by the runtime to
    /// compile them once up front.
    #[must_use]
    pub fn all_conditions(&self) -> Vec<(String, &Conditional)> {
        let mut out: Vec<(String, &Conditional)> = Vec::new();

        if let Some(entry) = &self.entry {
            for (i, c) in entry.all_of.iter().enumerate() {
                out.push((indexed_path("entry.all_of", i), c));
            }
            for (i, c) in entry.any_of.iter().enumerate() {
                out.push((indexed_path("entry.any_of", i), c));
            }
        }
        for (i, c) in self.invalidation.iter().enumerate() {
            out.push((indexed_path("invalidation", i), c));
        }
        if let Some(exit) = &self.exit {
            for (i, c) in exit.all_of.iter().enumerate() {
                out.push((indexed_path("exit.all_of", i), c));
            }
            for (i, c) in exit.any_of.iter().enumerate() {
                out.push((indexed_path("exit.any_of", i), c));
            }
        }

        out
    }

    /// The timeframe the strategy makes decisions on: the **finest** declared
    /// one.
    ///
    /// This is the execution clock. Coarser timeframes are context -- they
    /// update when their candles close, and the entry is evaluated only when
    /// this timeframe closes a candle.
    ///
    /// Returns `None` only when `timeframes` is empty, which the validator
    /// rejects.
    #[must_use]
    pub fn decision_timeframe(&self) -> Option<(&str, Timeframe)> {
        self.timeframes
            .iter()
            .map(|(name, tf)| (name.as_str(), *tf))
            .min_by_key(|(_, tf)| tf.nanos())
    }

    /// Resolve a timeframe name to its duration.
    #[must_use]
    pub fn timeframe(&self, name: &str) -> Option<Timeframe> {
        self.timeframes.get(name).copied()
    }

    /// The trade direction: the explicit one if given, otherwise the one the
    /// stop rule implies.
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        if let Some(explicit) = self.entry.as_ref().and_then(|e| e.direction) {
            return Some(explicit);
        }
        self.risk.as_ref().map(|r| r.stop.direction())
    }
}

/// Render `prefix[index]` for error paths and condition tags.
#[must_use]
pub fn indexed_path(prefix: &str, index: usize) -> String {
    format!("{prefix}[{index}]")
}

// The schema's serde behaviour is exercised through its YAML form -- that is the
// encoding a human writes and the one whose quirks (bare strings, mappings,
// missing keys) are worth pinning. The guest build drops `serde_yaml`, so these
// are gated with the feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;

    #[test]
    fn stop_spec_round_trips_through_yaml() {
        let cases = [
            StopSpec::BelowSweepLow,
            StopSpec::AboveSweepHigh,
            StopSpec::BelowSwingLow,
            StopSpec::AboveSwingHigh,
            StopSpec::BelowRecentLow { bars: 20 },
            StopSpec::AboveRecentHigh { bars: 20 },
            StopSpec::Atr {
                multiple: 1.5,
                period: 14,
            },
            StopSpec::Fixed { price: 42_000.0 },
        ];

        for spec in cases {
            let yaml = serde_yaml::to_string(&spec).unwrap();
            let back: StopSpec = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(back, spec, "round trip failed for {yaml}");
        }
    }

    #[test]
    fn unit_stop_variants_serialize_as_bare_strings() {
        let yaml = serde_yaml::to_string(&StopSpec::BelowSweepLow).unwrap();
        assert_eq!(yaml.trim(), "below_sweep_low");
    }

    #[test]
    fn parameterised_stop_variants_serialize_as_mappings() {
        let yaml = serde_yaml::to_string(&StopSpec::Atr {
            multiple: 1.5,
            period: 14,
        })
        .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed["kind"].as_str(), Some("atr"));
        assert_eq!(parsed["multiple"].as_f64(), Some(1.5));
        assert_eq!(parsed["period"].as_u64(), Some(14));
        // Parameters that do not apply to this rule are omitted, not null.
        assert!(parsed.get("bars").is_none());
        assert!(parsed.get("price").is_none());
    }

    #[test]
    fn stop_rule_side_is_derived_from_its_direction_word() {
        assert_eq!(StopSpec::BelowSweepLow.direction(), Direction::Long);
        assert_eq!(StopSpec::BelowSwingLow.direction(), Direction::Long);
        assert_eq!(
            StopSpec::BelowRecentLow { bars: 5 }.direction(),
            Direction::Long
        );
        assert_eq!(StopSpec::AboveSweepHigh.direction(), Direction::Short);
        assert_eq!(StopSpec::AboveSwingHigh.direction(), Direction::Short);
        assert_eq!(
            StopSpec::AboveRecentHigh { bars: 5 }.direction(),
            Direction::Short
        );
        assert!(StopSpec::BelowSweepLow.implies_direction());
        assert!(!StopSpec::Atr {
            multiple: 1.0,
            period: 14
        }
        .implies_direction());
    }

    #[test]
    fn a_missing_required_parameter_is_rejected() {
        let err = serde_yaml::from_str::<StopSpec>("{kind: atr, multiple: 1.5}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("multiple") && err.contains("period"), "{err}");
    }

    #[test]
    fn an_unknown_stop_kind_lists_the_valid_ones() {
        let err = serde_yaml::from_str::<StopSpec>("below_the_moon")
            .unwrap_err()
            .to_string();
        assert!(err.contains("below_sweep_low"), "{err}");
    }

    #[test]
    fn an_unknown_stop_parameter_is_rejected() {
        let err =
            serde_yaml::from_str::<StopSpec>("{kind: atr, multiple: 1.5, period: 14, foo: 1}")
                .unwrap_err()
                .to_string();
        assert!(err.contains("foo"), "{err}");
    }

    /// The builder's stop vocabulary is a *description* of `StopSpec`, so the
    /// risk is that the two drift apart: a rule the parser accepts but the
    /// builder never offers, or one it offers that then fails to parse. This
    /// pins every variant against what actually serializes, which is the only
    /// form a client ever sees.
    #[test]
    fn every_stop_rule_is_described_exactly_once() {
        let specs = [
            StopSpec::BelowSweepLow,
            StopSpec::AboveSweepHigh,
            StopSpec::BelowSwingLow,
            StopSpec::AboveSwingHigh,
            StopSpec::BelowRecentLow { bars: 20 },
            StopSpec::AboveRecentHigh { bars: 20 },
            StopSpec::Atr {
                multiple: 1.5,
                period: 14,
            },
            StopSpec::Fixed { price: 42_000.0 },
        ];

        assert_eq!(
            specs.len(),
            ALL_STOP_KINDS.len(),
            "a stop rule was added without describing it, or described twice"
        );

        for spec in specs {
            let name = spec.kind_name();
            let described: Vec<_> = ALL_STOP_KINDS.iter().filter(|k| k.kind == name).collect();
            assert_eq!(
                described.len(),
                1,
                "`{name}` is described {0} times",
                described.len()
            );
            let kind = described[0];

            // The side: `atr` and `fixed` carry none, which is why they need an
            // explicit `entry.direction`.
            assert_eq!(
                kind.implies_direction.is_some(),
                spec.implies_direction(),
                "`{name}` disagrees with StopSpec::implies_direction"
            );
            if spec.implies_direction() {
                assert_eq!(kind.implies_direction, Some(spec.direction()));
            }

            // The parameters: read them off the serialized form rather than
            // restating them, so a new parameter cannot be added silently.
            let value = serde_json::to_value(spec).unwrap();
            let mut actual: Vec<&str> = match &value {
                serde_json::Value::Object(map) => map
                    .keys()
                    .filter(|k| k.as_str() != "kind")
                    .map(String::as_str)
                    .collect(),
                // The four bare-string rules carry no parameters at all.
                serde_json::Value::String(_) => Vec::new(),
                other => panic!("`{name}` serialized as {other:?}"),
            };
            actual.sort_unstable();
            let mut expected = kind.params.to_vec();
            expected.sort_unstable();
            assert_eq!(
                actual, expected,
                "`{name}` takes different parameters than described"
            );
        }
    }

    /// `ALL` *is* the list, so it cannot miss a variant -- what it can do is
    /// contain a duplicate, which would make the builder offer the same option
    /// twice. This also pins the wire spelling, which is what a client reads.
    #[test]
    fn the_listed_kinds_are_distinct_and_keep_their_wire_spelling() {
        let rendered: Vec<String> = DocumentKind::ALL
            .iter()
            .map(|k| serde_json::to_string(k).unwrap())
            .collect();
        assert_eq!(rendered, ["\"indicator\"", "\"strategy\"", "\"bot\""]);

        let rendered: Vec<String> = TakeProfitKind::ALL
            .iter()
            .map(|k| serde_json::to_string(k).unwrap())
            .collect();
        assert_eq!(
            rendered,
            ["\"risk_multiple\"", "\"atr_multiple\"", "\"fixed_price\""]
        );
    }

    #[test]
    fn decision_timeframe_is_the_finest_declared_one() {
        let yaml = r#"
name: t
version: "1"
kind: indicator
market: BTCUSDT
timeframes:
  trend: 4h
  setup: 1h
  entry: 5m
"#;
        let doc: StrategyDocument = serde_yaml::from_str(yaml).unwrap();
        let (name, tf) = doc.decision_timeframe().unwrap();
        assert_eq!(name, "entry");
        assert_eq!(tf, Timeframe::M5);
    }

    #[test]
    fn unknown_top_level_fields_are_rejected() {
        let yaml = r#"
name: t
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
oops: true
"#;
        let err = serde_yaml::from_str::<StrategyDocument>(yaml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("oops"), "{err}");
    }

    #[test]
    fn direction_falls_back_to_the_stop_side() {
        let yaml = r#"
name: t
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  entry: 5m
entry:
  all_of: []
risk:
  max_risk_pct: 1.0
  stop: above_swing_high
invalidation: []
"#;
        let doc: StrategyDocument = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(doc.direction(), Some(Direction::Short));
    }
}
