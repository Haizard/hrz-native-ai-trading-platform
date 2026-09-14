//! The explainable-thesis object (`docs/09`).
//!
//! ## The one rule this module enforces
//!
//! > The narrative is generated **from** the structured numeric fields, never
//! > the other way around. The numbers are ground truth; the prose explains
//! > them.
//!
//! That rule exists because of a measured behaviour of the configured model:
//! given `103250.5` in a tool result, it wrote `$103,250.50` back. A pipeline
//! that recovered numbers by parsing the model's prose would therefore be
//! parsing reformatted, rounded, sometimes invented values. So the structured
//! fields are what the model fills in via a tool call, and the prose is written
//! afterwards from those fields.
//!
//! ## What is recomputed rather than trusted
//!
//! [`TradeThesis::finalize`] recalculates every derived field from the
//! primitives:
//!
//! * `risk_reward` from entry/stop/target -- the model's own arithmetic is
//!   discarded entirely;
//! * `confidence_pct` is clamped to 0..100;
//! * levels are checked against the prices actually observed in tool results
//!   (see [`PriceRange`]), which is what turns "the model typed a plausible
//!   number" into "the number is reconcilable with the data".
//!
//! A thesis that fails the structural checks is rejected with
//! [`AgentError::Ungrounded`] rather than quietly repaired: a long whose stop is
//! above its entry is not a thesis with a minor arithmetic slip.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::AgentError;
use crate::llm_client::ToolSpec;

/// Smallest gap between entry and stop that counts as risk, as a fraction of
/// the entry price. `0.0002` is two basis points.
///
/// Purely a sanity floor. It exists because a live run placed the stop exactly
/// on the swept level -- 0.7bp below entry -- and the recomputed R came out at
/// 44, which is the kind of number that makes the whole thesis look
/// fabricated. It is set loose on purpose: this catches stops that are noise,
/// it does not tell the model how wide a stop should be.
pub const MIN_RISK_FRACTION: f64 = 0.0002;

/// Which way the thesis points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bias {
    /// Expecting price to rise.
    Long,
    /// Expecting price to fall.
    Short,
    /// No directional view -- a "stand aside" thesis.
    None,
}

impl Bias {
    /// Parse from a model-supplied string.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "long" | "buy" | "bullish" => Self::Long,
            "short" | "sell" | "bearish" => Self::Short,
            _ => Self::None,
        }
    }

    /// Whether this bias implies a direction.
    #[must_use]
    pub fn is_directional(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Whether a condition held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The condition held.
    Pass,
    /// The condition did not hold.
    Fail,
    /// It could not be evaluated -- no data, or the method does not cover it.
    ///
    /// Distinct from [`CheckStatus::Fail`] on purpose. "Absorption absent" and
    /// "absorption undetectable in this window" are different statements and
    /// collapsing them is how an agent talks itself into a setup that was never
    /// actually confirmed.
    Unknown,
}

impl CheckStatus {
    /// A short mark for rendering: tick, cross, question mark.
    #[must_use]
    pub fn mark(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// One evaluated condition, with where its number came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionCheck {
    /// What was checked.
    pub label: String,
    /// Whether it held.
    pub status: CheckStatus,
    /// Human-readable detail, ideally naming the observed value.
    pub detail: String,
    /// The number that decided it, when there is one.
    pub observed: Option<f64>,
    /// The tool (or skill rule) the number came from.
    pub source: String,
}

/// One tool call and its raw result, kept for audit.
///
/// `docs/09`'s done criteria require every numeric field in a thesis to be
/// traceable to a specific tool call, logged alongside it. This is that log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolTrace {
    /// Tool name.
    pub tool: String,
    /// Arguments the model supplied.
    pub args: Value,
    /// The result returned.
    pub result: Value,
}

/// The explainable thesis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeThesis {
    /// Symbol analysed.
    pub symbol: String,
    /// The timeframe the entry decision was made on.
    pub timeframe: String,
    /// Direction.
    pub direction: Bias,
    /// Confidence, 0..100.
    pub confidence_pct: f64,
    /// Higher-timeframe context checks.
    pub higher_timeframe_checks: Vec<ConditionCheck>,
    /// Order-flow checks on the decision timeframe.
    pub order_flow_checks: Vec<ConditionCheck>,
    /// Planned entry.
    pub entry_price: f64,
    /// Planned stop.
    pub stop_price: f64,
    /// Planned target.
    pub target_price: f64,
    /// Reward-to-risk, recomputed in Rust.
    pub risk_reward: f64,
    /// What would prove the thesis wrong.
    pub invalidation: String,
    /// The skill that produced this, as `name vVersion`.
    pub skill_used: Option<String>,
    /// How many similar setups fired historically.
    pub historical_similar_setups: Option<u32>,
    /// Their win rate, 0..1.
    pub historical_win_rate: Option<f64>,
    /// Prose explanation, generated last from the fields above.
    pub narrative: String,
    /// Every tool call and result behind this thesis.
    pub provenance: Vec<ToolTrace>,
}

impl TradeThesis {
    /// Every check, higher-timeframe first.
    #[must_use]
    pub fn all_checks(&self) -> Vec<&ConditionCheck> {
        self.higher_timeframe_checks
            .iter()
            .chain(self.order_flow_checks.iter())
            .collect()
    }

    /// How many checks passed, failed and were unknown.
    #[must_use]
    pub fn check_tally(&self) -> (usize, usize, usize) {
        let mut passed = 0;
        let mut failed = 0;
        let mut unknown = 0;
        for check in self.all_checks() {
            match check.status {
                CheckStatus::Pass => passed += 1,
                CheckStatus::Fail => failed += 1,
                CheckStatus::Unknown => unknown += 1,
            }
        }
        (passed, failed, unknown)
    }

    /// Whether any check outright failed.
    #[must_use]
    pub fn has_failed_check(&self) -> bool {
        self.check_tally().1 > 0
    }

    /// Recompute derived fields and verify the thesis is internally coherent.
    ///
    /// A [`Bias::None`] thesis is exempt from the level checks. A model that
    /// examined the data and concluded there is no setup is giving the most
    /// valuable answer there is, and it has no entry, stop or target to
    /// report -- requiring one would force it to invent a trade it had
    /// correctly ruled out. Its levels are zeroed so a consumer reading them
    /// without checking the direction sees "no levels", not "levels at zero".
    ///
    /// # Errors
    /// [`AgentError::Ungrounded`] when a number is non-finite, when the
    /// stop/target sit on the wrong side of entry for the direction, or when a
    /// level cannot be reconciled with anything the tools reported.
    pub fn finalize(&mut self, observed: &PriceRange) -> Result<(), AgentError> {
        self.confidence_pct = self.confidence_pct.clamp(0.0, 100.0);
        if !self.confidence_pct.is_finite() {
            self.confidence_pct = 0.0;
        }

        if !self.direction.is_directional() {
            self.entry_price = 0.0;
            self.stop_price = 0.0;
            self.target_price = 0.0;
            self.risk_reward = 0.0;
            return Ok(());
        }

        for (name, value) in [
            ("entry_price", self.entry_price),
            ("stop_price", self.stop_price),
            ("target_price", self.target_price),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(AgentError::Ungrounded(format!(
                    "{name} is {value}, which is not a usable price"
                )));
            }
        }

        // Structural sanity: a long's stop is below entry and its target above.
        // Without this, a model that swapped two numbers produces a thesis that
        // looks complete and is nonsense.
        {
            let long = matches!(self.direction, Bias::Long);
            let risk = if long {
                self.entry_price - self.stop_price
            } else {
                self.stop_price - self.entry_price
            };
            let reward = if long {
                self.target_price - self.entry_price
            } else {
                self.entry_price - self.target_price
            };

            if risk <= 0.0 {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis has entry {} and stop {}: the stop is on the wrong side",
                    self.direction, self.entry_price, self.stop_price
                )));
            }
            if reward <= 0.0 {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis has entry {} and target {}: the target is on the wrong side",
                    self.direction, self.entry_price, self.target_price
                )));
            }

            // A stop a fraction of a basis point away is not risk, it is
            // noise -- and it inflates R into nonsense. A live run put the
            // stop exactly on the swept level, 0.7bp below entry, and
            // reported a 44R setup. Two basis points is deliberately loose;
            // it catches the absurd case without legislating real stops.
            let min_risk = self.entry_price * MIN_RISK_FRACTION;
            if risk < min_risk {
                return Err(AgentError::Ungrounded(format!(
                    "{:?} thesis risks {} between entry {} and stop {}, which is under \
                     the {} minimum ({:.4}): the stop may sit on a level, but it must sit \
                     beyond it with room",
                    self.direction,
                    risk,
                    self.entry_price,
                    self.stop_price,
                    MIN_RISK_FRACTION,
                    min_risk
                )));
            }

            self.risk_reward = reward / risk;
        }

        if !self.risk_reward.is_finite() {
            self.risk_reward = 0.0;
        }

        // Grounding: every level must be reconcilable with something the tools
        // actually reported.
        for (name, value) in [
            ("entry_price", self.entry_price),
            ("stop_price", self.stop_price),
            ("target_price", self.target_price),
        ] {
            observed.check(name, value)?;
        }

        Ok(())
    }

    /// A deterministic narrative, used when the model cannot be asked for one.
    ///
    /// Every sentence is built from the structured fields, so this is a valid
    /// (if flat) narrative rather than a placeholder. It is also the control
    /// against which a model-written narrative can be sanity-checked by a human.
    #[must_use]
    pub fn fallback_narrative(&self) -> String {
        let (passed, failed, unknown) = self.check_tally();
        let mut out = format!(
            "{} on {} ({}): {:?} bias at {:.1}% confidence.\n",
            self.symbol,
            self.timeframe,
            self.direction_mark(),
            self.direction,
            self.confidence_pct
        );

        if self.direction.is_directional() {
            out.push_str(&format!(
                "Entry {:.4}, stop {:.4}, target {:.4} -> {:.2}R.\n",
                self.entry_price, self.stop_price, self.target_price, self.risk_reward
            ));
        } else {
            out.push_str("No trade: the conditions for a setup were not met.\n");
        }

        out.push_str(&format!(
            "Checks: {passed} passed, {failed} failed, {unknown} unknown.\n"
        ));
        for check in self.all_checks() {
            let observed = check
                .observed
                .map(|v| format!(" (observed {v:.4} via {})", check.source))
                .unwrap_or_else(|| format!(" (via {})", check.source));
            out.push_str(&format!(
                "  [{}] {}: {}{}\n",
                check.status.mark(),
                check.label,
                check.detail,
                observed
            ));
        }

        if !self.invalidation.is_empty() {
            out.push_str(&format!("Invalidation: {}\n", self.invalidation));
        }
        if let Some(skill) = &self.skill_used {
            out.push_str(&format!("Skill: {skill}\n"));
        }
        if let (Some(count), Some(rate)) =
            (self.historical_similar_setups, self.historical_win_rate)
        {
            out.push_str(&format!(
                "Historically: {count} similar setups, {:.1}% win rate.\n",
                rate * 100.0
            ));
        }
        out
    }

    fn direction_mark(&self) -> &'static str {
        match self.direction {
            Bias::Long => "long",
            Bias::Short => "short",
            Bias::None => "no-direction",
        }
    }
}

// ---------------------------------------------------------------------------
// Observed price range -- the grounding evidence
// ---------------------------------------------------------------------------

/// The span of prices the tools actually reported.
///
/// Populated by walking every tool result for numeric fields whose *name* says
/// they are a price. Key-name matching rather than "any number in the payload"
/// matters: totals, counts and ratios are numbers too, and treating CVD or a
/// volume total as a price would widen the range until it accepted anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriceRange {
    min: Option<f64>,
    max: Option<f64>,
    samples: usize,
}

/// JSON keys whose values are prices.
const PRICE_KEYS: &[&str] = &[
    "price",
    "poc",
    "vah",
    "val",
    "vwap",
    "open",
    "high",
    "low",
    "close",
    "o",
    "h",
    "l",
    "c",
    "price_level",
    "level",
    "nearest_above",
    "nearest_below",
    "nearest_liquidity_above",
    "nearest_liquidity_below",
    "swing_highs",
    "swing_lows",
    "high_volume_nodes",
    "low_volume_nodes",
];

impl PriceRange {
    /// An empty range, which accepts everything.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one tool result into the range.
    pub fn observe_result(&mut self, result: &Value) {
        self.walk(result, None);
    }

    /// Whether any price has been observed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.min.is_none() || self.max.is_none()
    }

    /// Lowest observed price.
    #[must_use]
    pub fn min(&self) -> Option<f64> {
        self.min
    }

    /// Highest observed price.
    #[must_use]
    pub fn max(&self) -> Option<f64> {
        self.max
    }

    /// Number of price samples folded in.
    #[must_use]
    pub fn samples(&self) -> usize {
        self.samples
    }

    /// Verify a level is reconcilable with what was observed.
    ///
    /// The band is deliberately generous (20% below the lowest observed price,
    /// 25% above the highest): a target may legitimately sit beyond the
    /// window's high, and a stop below its low. What it catches is the failure
    /// mode that matters -- a level from a different symbol, a different
    /// regime, or nowhere near the data at all.
    ///
    /// # Errors
    /// [`AgentError::Ungrounded`] when the level falls outside the band.
    pub fn check(&self, field: &str, value: f64) -> Result<(), AgentError> {
        let (Some(min), Some(max)) = (self.min, self.max) else {
            return Ok(());
        };
        let lower = min * 0.8;
        let upper = max * 1.25;
        if value < lower || value > upper {
            return Err(AgentError::Ungrounded(format!(
                "{field} is {value:.4}, but every price the tools reported lies in \
                 [{min:.4}, {max:.4}] (accepted band [{lower:.4}, {upper:.4}])"
            )));
        }
        Ok(())
    }

    fn walk(&mut self, value: &Value, key: Option<&str>) {
        match value {
            Value::Number(number) => {
                let is_price_key = key.is_some_and(|k| PRICE_KEYS.contains(&k));
                if is_price_key {
                    if let Some(number) = number.as_f64().filter(|v| v.is_finite() && *v > 0.0) {
                        self.min = Some(self.min.map_or(number, |m: f64| m.min(number)));
                        self.max = Some(self.max.map_or(number, |m: f64| m.max(number)));
                        self.samples += 1;
                    }
                }
            }
            Value::Object(map) => {
                for (key, value) in map {
                    self.walk(value, Some(key.as_str()));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.walk(item, key);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The submit_thesis tool
// ---------------------------------------------------------------------------

/// The name of the tool the model must call to deliver a thesis.
pub const SUBMIT_THESIS: &str = "submit_thesis";

/// The `submit_thesis` schema.
///
/// This is how the thesis becomes structured *before* any prose exists: the
/// model fills fields, not sentences.
#[must_use]
pub fn submit_thesis_spec() -> ToolSpec {
    ToolSpec {
        name: SUBMIT_THESIS.into(),
        description: "Deliver your final answer as a structured trade thesis. Call this \
            exactly once, after you have the tool results you need. Copy numbers \
            verbatim from tool output -- never round, reformat or estimate them."
            .into(),
        input_schema: check_schema(),
    }
}

fn check_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "e.g. BTCUSDT"},
            "timeframe": {"type": "string", "description": "The timeframe the entry decision was made on"},
            "direction": {"type": "string", "enum": ["long", "short", "none"]},
            "confidence_pct": {"type": "number", "minimum": 0, "maximum": 100},
            "entry_price": {"type": "number"},
            "stop_price": {"type": "number"},
            "target_price": {"type": "number"},
            "invalidation": {"type": "string", "description": "What would prove this wrong"},
            "higher_timeframe_checks": check_array_schema(),
            "order_flow_checks": check_array_schema(),
        },
        "required": [
            "symbol", "timeframe", "direction", "confidence_pct",
            "entry_price", "stop_price", "target_price", "invalidation",
            "higher_timeframe_checks", "order_flow_checks"
        ],
    })
}

/// The schema for one checked condition.
///
/// `status` is a three-way enum, not a boolean, and that is load-bearing.
/// The first cut of this schema used `passed: true|false`, and a live model
/// answered `"passed": false` on a condition whose detail read "tick data
/// unavailable; absorption check UNKNOWN" -- it had correctly determined the
/// condition was unevaluable, then had nowhere to say so except `false`, and
/// so reported a confident failure instead of an honest blank.
///
/// A boolean cannot express [`CheckStatus::Unknown`]. Making the model's
/// vocabulary match the internal one is what keeps "not checked" from being
/// recorded as "checked and failed".
fn check_array_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "label": {"type": "string", "description": "Short name of the condition"},
                "status": {
                    "type": "string",
                    "enum": ["pass", "fail", "unknown"],
                    "description": "pass = the condition held. fail = it was evaluated \
                        and did not hold. unknown = it could not be evaluated at all \
                        (no data, tool reported available:false). Use unknown rather \
                        than fail whenever the data was missing.",
                },
                "detail": {"type": "string"},
                "observed": {
                    "description": "The number that decided it, if any",
                    "type": ["number", "null"],
                },
                "source": {"type": "string", "description": "Tool or rule the number came from"},
            },
            "required": ["label", "status", "detail"],
        },
    })
}

/// Parse the `submit_thesis` arguments into a thesis.
///
/// Tolerant on purpose: a missing `source` defaults to "unattributed" rather
/// than failing the call, because a thesis with an unattributed check is still
/// inspectable, whereas a rejected call produces nothing at all. The hard
/// checks (finite prices, coherent geometry) happen in
/// [`TradeThesis::finalize`], where failing is the right answer.
///
/// # Errors
/// [`AgentError::InvalidToolArgs`] when a required field is missing or of the
/// wrong type.
pub fn parse_thesis(args: &Value) -> Result<TradeThesis, AgentError> {
    const TOOL: &str = SUBMIT_THESIS;

    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let timeframe = args
        .get("timeframe")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let direction = Bias::parse(args.get("direction").and_then(Value::as_str).unwrap_or(""));

    let number = |key: &str| -> Result<f64, AgentError> {
        args.get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: format!("`{key}` is required and must be a number"),
            })
    };

    Ok(TradeThesis {
        symbol,
        timeframe,
        direction,
        confidence_pct: number("confidence_pct")?,
        higher_timeframe_checks: parse_checks(args.get("higher_timeframe_checks")),
        order_flow_checks: parse_checks(args.get("order_flow_checks")),
        entry_price: number("entry_price")?,
        stop_price: number("stop_price")?,
        target_price: number("target_price")?,
        // Recomputed by finalize(); zero here means "not yet derived", and
        // finalize always overwrites it before the thesis is returned.
        risk_reward: 0.0,
        invalidation: args
            .get("invalidation")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        skill_used: None,
        historical_similar_setups: None,
        historical_win_rate: None,
        narrative: String::new(),
        provenance: Vec::new(),
    })
}

/// Read a check's status, preferring `status` and falling back to `passed`.
///
/// `passed` is not in the schema any more, but models do emit it, and a
/// boolean-shaped answer is still an answer: `true` and `false` are
/// unambiguous. Only the third case is lost, which is the one the enum exists
/// to recover -- so the fallback is accepted, not treated as an error.
fn parse_status(item: &Value) -> CheckStatus {
    if let Some(status) = item.get("status").and_then(Value::as_str) {
        return match status.trim().to_ascii_lowercase().as_str() {
            "pass" | "passed" | "true" => CheckStatus::Pass,
            "fail" | "failed" | "false" => CheckStatus::Fail,
            // Anything unrecognised -- including "unknown" -- is Unknown. A
            // status we cannot read is not a status we can trust as a failure.
            _ => CheckStatus::Unknown,
        };
    }

    match item.get("passed").and_then(Value::as_bool) {
        Some(true) => CheckStatus::Pass,
        Some(false) => CheckStatus::Fail,
        // Absent entirely is Unknown rather than false: a hedging model is not
        // the same as a model reporting a failure.
        None => CheckStatus::Unknown,
    }
}

fn parse_checks(value: Option<&Value>) -> Vec<ConditionCheck> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let label = item.get("label").and_then(Value::as_str)?;
            let status = parse_status(item);
            Some(ConditionCheck {
                label: label.to_string(),
                status,
                detail: item
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                observed: item.get("observed").and_then(Value::as_f64),
                source: item
                    .get("source")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("unattributed")
                    .to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thesis(direction: Bias, entry: f64, stop: f64, target: f64) -> TradeThesis {
        TradeThesis {
            symbol: "BTCUSDT".into(),
            timeframe: "5m".into(),
            direction,
            confidence_pct: 62.0,
            higher_timeframe_checks: vec![ConditionCheck {
                label: "4h trend".into(),
                status: CheckStatus::Pass,
                detail: "bullish".into(),
                observed: Some(1.0),
                source: "analyze_timeframe".into(),
            }],
            order_flow_checks: vec![ConditionCheck {
                label: "absorption".into(),
                status: CheckStatus::Unknown,
                detail: "no tick data in window".into(),
                observed: None,
                source: "detect_absorption".into(),
            }],
            entry_price: entry,
            stop_price: stop,
            target_price: target,
            risk_reward: 0.0,
            invalidation: "5m close below the swept low".into(),
            skill_used: Some("Liquidity Sweep + Absorption v2.1".into()),
            historical_similar_setups: Some(219),
            historical_win_rate: Some(0.269),
            narrative: String::new(),
            provenance: Vec::new(),
        }
    }

    /// Prices a model could plausibly return against [`observed`].
    ///
    /// The happy-path cases deliberately use realistic BTC-scale numbers rather
    /// than round placeholders: with `100.0 / 90.0 / 130.0` every one of them
    /// was rejected by the grounding check before reaching the arithmetic it
    /// meant to test, which is exactly the failure the check exists to catch.
    fn observed() -> PriceRange {
        let mut range = PriceRange::new();
        range.observe_result(&json!({
            "price": 103250.5, "poc": 103000.0, "vah": 104000.0, "val": 102000.0,
            "high": 104500.0, "low": 101500.0,
        }));
        range
    }

    #[test]
    fn risk_reward_is_recomputed_not_trusted() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.risk_reward = 99.0; // the model's own arithmetic
        t.finalize(&observed()).unwrap();
        // (106000-103000) / (103000-102000) == 3.0
        assert!((t.risk_reward - 3.0).abs() < 1e-9, "got {}", t.risk_reward);
    }

    #[test]
    fn a_short_thesis_inverts_the_ratio() {
        let mut t = thesis(Bias::Short, 103000.0, 104000.0, 101000.0);
        t.finalize(&observed()).unwrap();
        // (103000-101000) / (104000-103000) == 2.0
        assert!((t.risk_reward - 2.0).abs() < 1e-9, "got {}", t.risk_reward);
    }

    #[test]
    fn a_long_with_its_stop_above_entry_is_rejected() {
        let mut t = thesis(Bias::Long, 103000.0, 104000.0, 106000.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
        assert!(err.to_string().contains("wrong side"));
    }

    #[test]
    fn a_target_on_the_wrong_side_is_rejected_too() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 101000.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
    }

    #[test]
    fn a_stop_that_is_noise_not_risk_is_rejected() {
        // The live bug: the stop landed exactly on the swept level, 5.73 below
        // a 77221.10 entry, and the recomputed R came out at 44. Structurally
        // valid, grounding-clean, and useless -- so it is rejected outright.
        let mut t = thesis(Bias::Long, 103000.0, 102995.0, 106000.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
        assert!(err.to_string().contains("minimum"), "got {err}");
        // 5.0 of risk on a 103000 entry would otherwise be a 600R "setup".
        assert!(
            t.risk_reward == 0.0,
            "R must not be published: {}",
            t.risk_reward
        );
    }

    #[test]
    fn the_risk_floor_is_loose_enough_to_allow_a_normal_stop() {
        // 100 points on 103000 is ~10bp -- tighter than most real stops, and
        // still accepted. The floor catches noise, it does not set policy.
        let mut t = thesis(Bias::Long, 103000.0, 102900.0, 103300.0);
        t.finalize(&observed()).unwrap();
        assert!((t.risk_reward - 3.0).abs() < 1e-9, "got {}", t.risk_reward);
    }

    #[test]
    fn confidence_is_clamped_into_range() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.confidence_pct = 250.0;
        t.finalize(&observed()).unwrap();
        assert!((t.confidence_pct - 100.0).abs() < 1e-9);

        t.confidence_pct = -40.0;
        t.finalize(&observed()).unwrap();
        assert!(t.confidence_pct.abs() < 1e-9);
    }

    #[test]
    fn a_stand_aside_thesis_needs_no_levels() {
        // Found on live data: the model checked the sweep, found the most
        // recent one was buy-side rather than sell-side, and honestly returned
        // direction "none" with zeroed levels. That answer was rejected for
        // having no usable entry price -- which would have forced the model to
        // invent a setup it had correctly ruled out.
        let mut t = thesis(Bias::None, 0.0, 0.0, 0.0);
        t.confidence_pct = 0.0;
        t.finalize(&observed()).unwrap();

        assert_eq!(t.entry_price, 0.0);
        assert_eq!(t.risk_reward, 0.0);
        assert!(t.fallback_narrative().contains("No trade"));
    }

    #[test]
    fn a_directional_thesis_still_needs_real_levels() {
        // The exemption must not leak: a long with no stop is still nonsense.
        let mut t = thesis(Bias::Long, 0.0, 0.0, 0.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
        assert!(err.to_string().contains("entry_price"));
    }

    #[test]
    fn a_level_nowhere_near_the_data_is_rejected() {
        // 250000 against data around 103000: the model typed a number from
        // somewhere else entirely.
        let mut t = thesis(Bias::Long, 250000.0, 249000.0, 253000.0);
        let err = t.finalize(&observed()).unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
        assert!(err.to_string().contains("250000"));
    }

    #[test]
    fn a_target_beyond_the_window_high_is_allowed() {
        // Legitimate: targets sit beyond recent extremes all the time.
        let mut t = thesis(Bias::Long, 104000.0, 102000.0, 125000.0);
        t.finalize(&observed()).unwrap();
        assert!(t.risk_reward > 0.0);
    }

    #[test]
    fn an_empty_range_accepts_everything() {
        // With no tool results there is no evidence to contradict, and the
        // orchestrator should not have produced a thesis anyway.
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.finalize(&PriceRange::new()).unwrap();
    }

    #[test]
    fn the_price_range_only_absorbs_fields_named_like_prices() {
        let mut range = PriceRange::new();
        range.observe_result(&json!({
            "price": 100.0,
            "cvd": 500000.0,
            "total_trades": 219,
            "ratio": 3.7,
        }));
        assert_eq!(range.min(), Some(100.0));
        assert_eq!(range.max(), Some(100.0));
        assert_eq!(range.samples(), 1);
    }

    #[test]
    fn nested_arrays_of_levels_are_folded_in() {
        let mut range = PriceRange::new();
        range.observe_result(&json!({
            "swing_highs": [10.0, 40.0],
            "swing_lows": [5.0],
            "liquidity": [{"price": 60.0}],
        }));
        assert_eq!(range.min(), Some(5.0));
        assert_eq!(range.max(), Some(60.0));
    }

    #[test]
    fn parse_thesis_reads_a_model_shaped_payload() {
        let args = json!({
            "symbol": "BTCUSDT",
            "timeframe": "5m",
            "direction": "long",
            "confidence_pct": 64.0,
            "entry_price": 103250.5,
            "stop_price": 102900.0,
            "target_price": 104300.0,
            "invalidation": "5m close below the swept low",
            "higher_timeframe_checks": [
                {"label": "4h trend", "passed": true, "detail": "bullish",
                 "observed": 1.0, "source": "analyze_timeframe"}
            ],
            "order_flow_checks": [
                {"label": "absorption", "passed": false, "detail": "none detected"}
            ],
        });
        let t = parse_thesis(&args).unwrap();
        assert_eq!(t.direction, Bias::Long);
        assert_eq!(t.higher_timeframe_checks.len(), 1);
        assert_eq!(t.higher_timeframe_checks[0].status, CheckStatus::Pass);
        // A missing `source` must not drop the check.
        assert_eq!(t.order_flow_checks[0].source, "unattributed");
        assert_eq!(t.order_flow_checks[0].status, CheckStatus::Fail);
        assert_eq!(t.check_tally(), (1, 1, 0));
        assert!(t.has_failed_check());
    }

    #[test]
    fn a_check_can_report_unknown_without_being_counted_as_failed() {
        // The bug this fixes, caught on a live model: it marked absorption
        // `passed: false` while writing "tick data unavailable" in the detail.
        // A boolean gave it nowhere to say "I could not check this", so an
        // honest blank was recorded as a confident failure.
        let args = json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 40.0, "entry_price": 77221.1, "stop_price": 77125.03,
            "target_price": 77475.0, "invalidation": "x",
            "higher_timeframe_checks": [],
            "order_flow_checks": [
                {"label": "absorption", "status": "unknown",
                 "detail": "tick data unavailable", "source": "detect_absorption"}
            ],
        });
        let t = parse_thesis(&args).unwrap();
        assert_eq!(t.order_flow_checks[0].status, CheckStatus::Unknown);
        assert!(!t.has_failed_check(), "unknown must not be a failure");
        assert_eq!(t.check_tally(), (0, 0, 1));
    }

    #[test]
    fn the_check_schema_offers_three_states_not_a_boolean() {
        let schema = submit_thesis_spec().input_schema;
        let status = schema["properties"]["order_flow_checks"]["items"]["properties"]["status"]
            .as_object()
            .expect("checks must have a `status` property");
        let values = status["enum"].as_array().unwrap();
        for expected in ["pass", "fail", "unknown"] {
            assert!(values.iter().any(|v| v == expected), "missing `{expected}`");
        }
        // A boolean `passed` is exactly what caused the bug.
        let props = schema["properties"]["order_flow_checks"]["items"]["properties"]
            .as_object()
            .unwrap();
        assert!(
            !props.contains_key("passed"),
            "`passed` must not be offered"
        );
    }

    #[test]
    fn a_boolean_passed_is_still_understood_when_a_model_emits_it() {
        let args = json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 40.0, "entry_price": 1.0, "stop_price": 0.9,
            "target_price": 1.3, "invalidation": "x",
            "higher_timeframe_checks": [{"label": "trend", "passed": true, "detail": "up"}],
            "order_flow_checks": [{"label": "absorption", "passed": false, "detail": "none"}],
        });
        let t = parse_thesis(&args).unwrap();
        assert_eq!(t.higher_timeframe_checks[0].status, CheckStatus::Pass);
        assert_eq!(t.order_flow_checks[0].status, CheckStatus::Fail);
    }

    #[test]
    fn a_missing_number_is_a_recoverable_argument_error() {
        let args = json!({"symbol": "BTCUSDT", "direction": "long"});
        let err = parse_thesis(&args).unwrap_err();
        assert!(
            matches!(err, AgentError::InvalidToolArgs { .. }),
            "got {err}"
        );
    }

    #[test]
    fn an_absent_passed_flag_is_unknown_not_failed() {
        let args = json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 10.0, "entry_price": 1.0, "stop_price": 0.9,
            "target_price": 1.3, "invalidation": "x",
            "higher_timeframe_checks": [{"label": "trend", "detail": "unclear"}],
            "order_flow_checks": [],
        });
        let t = parse_thesis(&args).unwrap();
        assert_eq!(t.higher_timeframe_checks[0].status, CheckStatus::Unknown);
        assert!(!t.has_failed_check());
    }

    #[test]
    fn the_fallback_narrative_is_built_only_from_the_fields() {
        let mut t = thesis(Bias::Long, 103000.0, 102000.0, 106000.0);
        t.finalize(&observed()).unwrap();
        let narrative = t.fallback_narrative();
        assert!(narrative.contains("BTCUSDT on 5m"));
        assert!(narrative.contains("103000.0000"));
        assert!(narrative.contains("3.00R"));
        assert!(narrative.contains("[PASS] 4h trend"));
        assert!(narrative.contains("[UNKNOWN] absorption"));
        assert!(narrative.contains("Liquidity Sweep + Absorption v2.1"));
        assert!(narrative.contains("219 similar setups"));
    }

    #[test]
    fn the_submit_thesis_schema_requires_the_fields_the_thesis_needs() {
        let spec = submit_thesis_spec();
        let required = spec.input_schema["required"].as_array().unwrap();
        for field in [
            "symbol",
            "timeframe",
            "direction",
            "confidence_pct",
            "entry_price",
            "stop_price",
            "target_price",
            "invalidation",
        ] {
            assert!(
                required.iter().any(|v| v == field),
                "schema must require {field}"
            );
        }
    }

    #[test]
    fn bias_parsing_accepts_the_words_a_model_actually_uses() {
        assert_eq!(Bias::parse("long"), Bias::Long);
        assert_eq!(Bias::parse("Bullish"), Bias::Long);
        assert_eq!(Bias::parse("short"), Bias::Short);
        assert_eq!(Bias::parse("bearish"), Bias::Short);
        assert_eq!(Bias::parse(""), Bias::None);
        assert!(!Bias::None.is_directional());
    }
}
