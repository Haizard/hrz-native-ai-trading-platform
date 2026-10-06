//! The join between the registry's static answers and what a run actually
//! read (`docs/39`).
//!
//! ## The two halves of an honest label
//!
//! A tool result's provenance has two independent halves:
//!
//! * **Declared** — what the provider *can* supply, from the capability
//!   registry (docs/38). Static, per (provider, symbol class): Bybit REST
//!   klines are direction-attributed whether or not anyone is looking.
//! * **Observed** — what the window *did* hold, from
//!   [`analytics_core::StateProvenance`]: how many bars, whether any trades
//!   arrived. A venue that *can* stream trades still yields an empty window
//!   when nobody watched the symbol.
//!
//! Neither half alone is honest: the declared half would call a delta "real"
//! over a window with no tape, and the observed half cannot tell a measured
//! split from an attributed one (attribution happens at the venue boundary;
//! a `Candle` carries plain numbers). [`CapabilityView`] is where they meet,
//! and the render rules are deliberately small:
//!
//! * footprint-level sections (footprint, absorption, imbalance) need trades.
//!   Statically impossible ⇒ `unavailable` with the registry's explanation;
//!   possible but the window held none ⇒ `unavailable` with "no trades in
//!   this window". Both say *unavailable*; the why differs and both whys are
//!   kept, because the fixes differ (switch venues vs widen the window).
//! * split-reading sections (delta, cvd, volume score): trades present ⇒
//!   `true` (the tape is live-built on both current venues). No trades ⇒ the
//!   window is history, so the label is the provider's **REST candle**
//!   fidelity: real ⇒ `true`, attributed ⇒ `derived` with the venue's own
//!   caveat, absent ⇒ `unavailable`.
//!
//! What a view does not do: answer *per-channel* or *per-timeframe*
//! questions (the registry's static scope stops at provider + class), and
//! degrade by freshness (the window's `Degraded` adjustments are the host's
//! call site, not this render).

use std::sync::Arc;

use serde_json::{json, Value};

use analytics_core::MarketState;
use capabilities::{Availability, DataKind, DataScope, Provider, Registry, SplitQuality, SymbolClass};

use crate::skills::{FallbackAccept, Skill};

/// A request-scoped handle on the capability registry: one provider, one
/// symbol class, resolved per symbol as tools run.
///
/// Attached by the host (the gateway knows the deployment's venue); `None`
/// on a request means no claims are made — the tools then report data facts
/// only, which is the same absent-means-absent posture as the drawing and
/// memory sources.
#[derive(Clone)]
pub struct CapabilityView {
    registry: Arc<Registry>,
    provider: Provider,
    symbol_class: SymbolClass,
}

impl std::fmt::Debug for CapabilityView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The registry is a shared lookup table; what matters in a log line is
        // whose eyes the answer was computed for.
        f.debug_struct("CapabilityView")
            .field("provider", &self.provider)
            .field("symbol_class", &self.symbol_class)
            .finish()
    }
}

impl CapabilityView {
    /// A view over this registry for one (provider, symbol class) scope.
    #[must_use]
    pub fn new(registry: Arc<Registry>, provider: Provider, symbol_class: SymbolClass) -> Self {
        Self {
            registry,
            provider,
            symbol_class,
        }
    }

    /// The scope's provider.
    #[must_use]
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// The scope's symbol class.
    #[must_use]
    pub fn symbol_class(&self) -> SymbolClass {
        self.symbol_class
    }

    /// The registry's answer for one capability on one symbol.
    #[must_use]
    pub fn resolve(&self, capability: &str, symbol: &str) -> capabilities::Resolution {
        self.registry.resolve(
            capability,
            &DataScope::new(self.provider, self.symbol_class, symbol),
        )
    }

    /// The provenance block for one tool's result, when the tool exposes a
    /// catalogued capability.
    ///
    /// The block *is* the registry's [`capabilities::Resolution`], serialized:
    /// availability, basis, missing kinds, caveats and the one-sentence
    /// explanation — the same words `/capabilities` serves, so the model and
    /// the UI never read two different stories. `None` for plumbing tools
    /// (memory, drawings, control): they have no data fidelity to declare,
    /// and a block on them would teach the model to ignore the field.
    #[must_use]
    pub fn tool_provenance(&self, tool: &str, symbol: &str) -> Option<Value> {
        let descriptor = self.registry.for_tool(tool)?;
        let resolution = self.resolve(descriptor.id, symbol);
        Some(serde_json::to_value(&resolution).expect("a Resolution serializes"))
    }

    /// Whether a skill may run on this scope, per its data contract
    /// (schema v2, docs/40).
    ///
    /// The verdict's rule per required capability:
    ///
    /// * `available` ⇒ satisfied.
    /// * `derived` ⇒ satisfied only when the skill's own `fallback` list
    ///   accepts derived answers for it — the contract is the document's to
    ///   relax, never the resolver's to assume.
    /// * `degraded` ⇒ satisfied, but listed as a gap: partial data is a risk
    ///   the doctrine manages, not a reason to silence the skill.
    /// * `unavailable` ⇒ refused, with the registry's explanation carried so
    ///   the refusal names the gap (the missing kinds), not just the verdict.
    ///
    /// `preferred` capabilities never refuse; their misses come back as gaps
    /// the caller may surface.
    #[must_use]
    pub fn check_skill(&self, skill: &Skill, symbol: &str) -> SkillVerdict {
        let requirements = &skill.capability_requirements;
        let mut refusals = Vec::new();
        let mut gaps = Vec::new();

        for need in &requirements.required {
            let capability = need.capability.as_str();
            let resolution = self.resolve(capability, symbol);
            match resolution.availability {
                // Satisfied. The resolution's caveats are NOT copied into the
                // gaps: a caveat describes the shape of present data and
                // already rides every tool result's provenance block (docs/39),
                // while the gaps list is about *missing or partial* data.
                // Repeating the caveats here would show the model the same
                // sentence twice per turn.
                Availability::Available => {}
                Availability::Derived => {
                    let accepts_derived = requirements.fallback.iter().any(|rule| {
                        rule.needs == capability && rule.accept == FallbackAccept::Derived
                    });
                    if !accepts_derived {
                        refusals.push(format!(
                            "{capability}: only a derived answer exists here, and this skill does not accept derived answers for it"
                        ));
                    }
                }
                Availability::Degraded => {
                    gaps.push(format!("{capability}: {}", resolution.explanation));
                }
                // `Partial` means the requirement holds for only part of the
                // scope; for a *required* capability that is a refusal — the
                // skill's ladder would read the missing part as present.
                Availability::Partial | Availability::Unavailable => {
                    refusals.push(format!("{capability}: {}", resolution.explanation));
                }
            }
        }

        for need in &requirements.preferred {
            let resolution = self.resolve(need.capability.as_str(), symbol);
            if resolution.availability != Availability::Available {
                gaps.push(format!(
                    "{}: {}",
                    need.capability, resolution.explanation
                ));
            }
        }

        if refusals.is_empty() {
            SkillVerdict::Eligible { gaps }
        } else {
            SkillVerdict::Refused { reasons: refusals }
        }
    }

    /// The REST channel's candle split fidelity for this scope.
    ///
    /// The label a split read gets when the window held no trades: with no
    /// live tape the candles are history, and history comes from REST. `None`
    /// when the provider serves no candles at all.
    fn rest_candle_split(&self) -> Option<SplitQuality> {
        self.registry
            .profile(self.provider)?
            .class(self.symbol_class)?
            .kind(DataKind::Candles)?
            .rest
            .map(|channel| channel.split)
    }

    /// The caveat the provider attaches to its REST candle channel, if any.
    fn rest_candle_note(&self) -> Option<&'static str> {
        self.registry
            .profile(self.provider)?
            .class(self.symbol_class)?
            .kind(DataKind::Candles)?
            .rest
            .and_then(|channel| channel.note)
    }

    /// The provenance block for a rendered [`MarketState`]: the observed
    /// facts from [`observed_provenance`], plus the declared split-fidelity
    /// labels only this view can supply.
    ///
    /// Terse on purpose — this lands in the model's context on every
    /// `analyze_timeframe` call, so the sections are one-word labels and the
    /// prose lives in `why`, capped at the few strings that change what the
    /// model may claim.
    #[must_use]
    pub fn state_provenance(&self, state: &MarketState) -> Value {
        let mut block = observed_provenance(state);
        let observed = state.provenance;

        // Split-reading sections: trades present ⇒ the tape is live-built and
        // the split is measured. Otherwise the candles are REST history and
        // the provider's declared REST fidelity is the label.
        let mut notes: Vec<String> = Vec::new();
        let split_label = if observed.trades > 0 {
            Some("true")
        } else {
            match self.rest_candle_split() {
                Some(SplitQuality::Real) => Some("true"),
                Some(SplitQuality::Attributed) => {
                    if let Some(note) = self.rest_candle_note() {
                        notes.push(note.to_string());
                    }
                    Some("derived")
                }
                Some(SplitQuality::Absent) => {
                    notes.push(format!(
                        "{} candles carry no buy/sell split at all: delta reads are not meaningful",
                        self.provider
                    ));
                    Some("unavailable")
                }
                None => None,
            }
        };
        if let Some(label) = split_label {
            if let Some(sections) = block["sections"].as_object_mut() {
                for section in ["delta", "cvd", "volume_score"] {
                    sections.insert(section.to_string(), json!(label));
                }
            }
        }
        if !notes.is_empty() {
            let why = block["why"].as_array_mut().expect("observed block has a why list");
            why.extend(notes.into_iter().map(Value::from));
        }
        block
    }
}

/// The outcome of checking a skill's data contract against a scope
/// (docs/40).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillVerdict {
    /// The skill may run.
    Eligible {
        /// Preferred or degraded capabilities, each as `"name: reason"` —
        /// surfaced in the prompt, never refused on.
        gaps: Vec<String>,
    },
    /// The skill must not run.
    Refused {
        /// Each failed requirement with the registry's explanation, so a
        /// refusal is a sentence the agent can say aloud, not a score that
        /// silently dropped the skill.
        reasons: Vec<String>,
    },
}

/// The observed half of a state's provenance: what the window held, with no
/// venue claims.
///
/// This is the render for a request with no [`CapabilityView`] attached —
/// tests and tools-only builds — and the base [`CapabilityView::state_provenance`]
/// layers its labels onto. Either way the footprint-level sections say
/// `unavailable` when the window held no trades: that absence is a fact about
/// the data itself, and no registry is needed to state it.
#[must_use]
pub fn observed_provenance(state: &MarketState) -> Value {
    let observed = state.provenance;
    let mut why: Vec<Value> = Vec::new();
    let trades_label = if observed.footprint_level {
        "available"
    } else {
        why.push(json!(
            "no trades in this window: footprint-level signals cannot be computed; \
             an empty list is not evidence none occurred"
        ));
        "unavailable"
    };
    let sections = json!({
        "footprint": trades_label,
        "absorption": trades_label,
        "imbalance": trades_label,
    });
    json!({
        "window_bars": observed.window_bars,
        "trades": observed.trades,
        "profile": observed.profile,
        "sections": sections,
        "why": why,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capabilities::descriptor::STANDARD;
    use capabilities::profile::{Channel, ClassProfile, KindSupport, ProviderDataProfile};

    /// The two venues as declared in market-data, restated here as fixtures so
    /// this crate's tests do not depend on market-data (the dependency edge
    /// runs the other way, and a cycle is the one thing it cannot have).
    fn binance() -> ProviderDataProfile {
        ProviderDataProfile::new(Provider::Binance).with_class(
            SymbolClass::Spot,
            ClassProfile::new()
                .with(
                    DataKind::Candles,
                    KindSupport::both(
                        Channel::candles(SplitQuality::Real),
                        Channel::candles(SplitQuality::Real),
                    ),
                )
                .with(
                    DataKind::Trades,
                    KindSupport::both(Channel::plain(), Channel::plain()),
                ),
        )
    }

    fn bybit() -> ProviderDataProfile {
        ProviderDataProfile::new(Provider::Bybit).with_class(
            SymbolClass::Spot,
            ClassProfile::new()
                .with(
                    DataKind::Candles,
                    KindSupport::both(
                        Channel::candles(SplitQuality::Real),
                        Channel::candles(SplitQuality::Attributed)
                            .with_note("attributed, not a measurement"),
                    ),
                )
                .with(DataKind::Trades, KindSupport::live_only(Channel::plain())),
        )
    }

    fn view(profile: ProviderDataProfile) -> CapabilityView {
        let provider = profile.provider;
        CapabilityView::new(
            Arc::new(Registry::new(STANDARD, vec![profile])),
            provider,
            SymbolClass::Spot,
        )
    }

    /// A real state from the real builder: the provenance under test is the
    /// one `build_market_state` computed, not a hand-written stand-in.
    fn state(trades: usize) -> MarketState {
        use analytics_core::{Candle, Timeframe, Trade};
        let candle = |open_time: i64| Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 10.0,
            high: 11.0,
            low: 9.0,
            close: 10.5,
            volume: 2.0,
            buy_volume: 1.2,
            sell_volume: 0.8,
        };
        let candles: Vec<Candle> = (0..4).map(|i| candle(i * 60_000_000_000)).collect();
        let tape: Vec<Trade> = (0..trades)
            .map(|i| Trade {
                symbol: "BTCUSDT".into(),
                trade_id: i as u64,
                price: 10.0,
                quantity: 1.0,
                is_buyer_maker: i % 2 == 0,
                timestamp: 60_000_000_000,
            })
            .collect();
        analytics_core::build_market_state(&candles, &tape, &Default::default())
            .expect("four candles make a state")
    }

    #[test]
    fn a_tool_with_a_capability_row_gets_the_registrys_answer() {
        let view = view(binance());
        let block = view
            .tool_provenance("get_footprint", "BTCUSDT")
            .expect("footprint is catalogued");
        assert_eq!(block["capability"], "footprint");
        assert_eq!(block["availability"], "available");
        assert_eq!(block["basis"], "true");
    }

    #[test]
    fn a_plumbing_tool_gets_no_block() {
        let view = view(binance());
        assert!(view.tool_provenance("remember", "BTCUSDT").is_none());
    }

    #[test]
    fn an_unknown_capability_reports_unavailable_in_words() {
        let view = view(binance());
        let block = view
            .tool_provenance("get_vpin", "BTCUSDT")
            .expect("vpin is catalogued");
        assert_eq!(block["capability"], "vpin");
        assert!(block["explanation"].is_string());
    }

    #[test]
    fn no_trades_marks_footprint_sections_unavailable_with_the_why() {
        let view = view(binance());
        let block = view.state_provenance(&state(0));
        assert_eq!(block["trades"], 0);
        assert_eq!(block["sections"]["absorption"], "unavailable");
        assert_eq!(block["sections"]["footprint"], "unavailable");
        assert_eq!(block["sections"]["imbalance"], "unavailable");
        let why = block["why"].as_array().expect("a why list").clone();
        assert!(
            why.iter().any(|w| w.as_str().is_some_and(|s| s.contains("no trades"))),
            "{why:?}"
        );
    }

    #[test]
    fn trades_present_marks_the_sections_available_and_splits_true() {
        let view = view(binance());
        let block = view.state_provenance(&state(42));
        assert_eq!(block["sections"]["absorption"], "available");
        assert_eq!(block["sections"]["delta"], "true");
        assert!(
            block["why"].as_array().expect("a why list").is_empty(),
            "nothing to explain: {block}"
        );
    }

    #[test]
    fn a_bybit_history_window_labels_delta_derived_not_true() {
        // The audit's provenance-leak finding, pinned as a render: no tape in
        // the window ⇒ the candles are REST history ⇒ Bybit's REST klines are
        // direction-attributed ⇒ the split reads are *derived*.
        let view = view(bybit());
        let block = view.state_provenance(&state(0));
        assert_eq!(block["sections"]["delta"], "derived");
        assert_eq!(block["sections"]["cvd"], "derived");
        assert_eq!(block["sections"]["volume_score"], "derived");
        let why = block["why"].as_array().expect("a why list").clone();
        assert!(
            why.iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("not a measurement"))),
            "the venue's own caveat must travel: {why:?}"
        );
    }

    #[test]
    fn a_bybit_live_window_labels_splits_true() {
        let view = view(bybit());
        let block = view.state_provenance(&state(7));
        assert_eq!(block["sections"]["delta"], "true");
    }

    #[test]
    fn the_block_stays_small_enough_for_a_prompt() {
        let view = view(bybit());
        let block = view.state_provenance(&state(0));
        let rendered = serde_json::to_string(&block).expect("serializes");
        assert!(
            rendered.len() < 900,
            "the block is prompt budget, not a report: {} bytes",
            rendered.len()
        );
    }

    // -- skill contracts (docs/40) -------------------------------------------

    fn skill(required: &[&str], fallback: &[(&str, crate::skills::FallbackAccept)]) -> Skill {
        Skill {
            name: "Test Skill".into(),
            capability_requirements: crate::skills::CapabilityRequirements {
                required: required
                    .iter()
                    .map(|capability| crate::skills::CapabilityNeed {
                        capability: (*capability).to_string(),
                    })
                    .collect(),
                fallback: fallback
                    .iter()
                    .map(|(needs, accept)| crate::skills::FallbackRule {
                        needs: (*needs).to_string(),
                        accept: *accept,
                    })
                    .collect(),
                ..crate::skills::CapabilityRequirements::default()
            },
            ..Skill::default()
        }
    }

    #[test]
    fn a_skill_with_no_contract_runs_anywhere() {
        let view = view(bybit());
        assert_eq!(
            view.check_skill(&skill(&[], &[]), "BTCUSDT"),
            SkillVerdict::Eligible { gaps: Vec::new() }
        );
    }

    /// A Deriv-*like* scope: a provider the registry knows, serving a symbol
    /// class with no data at all. The audit's P2 scenario — footprint on such
    /// a symbol must be refused statically, by name.
    fn deriv_like() -> ProviderDataProfile {
        ProviderDataProfile::new(Provider::Bybit)
            .with_class(SymbolClass::SyntheticIndex, ClassProfile::new())
    }

    fn deriv_view() -> CapabilityView {
        CapabilityView::new(
            Arc::new(Registry::new(STANDARD, vec![deriv_like()])),
            Provider::Bybit,
            SymbolClass::SyntheticIndex,
        )
    }

    #[test]
    fn a_missing_requirement_refuses_with_the_gap_named() {
        let view = deriv_view();
        let verdict = view.check_skill(&skill(&["footprint"], &[]), "V75");
        let SkillVerdict::Refused { reasons } = verdict else {
            panic!("footprint is not honest without candles or trades: {verdict:?}")
        };
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("footprint") && reason.contains("trades")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_live_tape_venue_satisfies_footprint_statically() {
        // The boundary the refusal tests lean on: a venue WITH a live tape
        // resolves footprint `available` even though its history is thin —
        // the thinness is a caveat on the answer, not a refusal of the skill.
        let view = view(bybit());
        assert_eq!(
            view.check_skill(&skill(&["footprint"], &[]), "BTCUSDT"),
            SkillVerdict::Eligible { gaps: Vec::new() }
        );
    }

    #[test]
    fn a_requirement_satisfied_by_live_data_is_eligible() {
        let view = view(binance());
        assert_eq!(
            view.check_skill(&skill(&["footprint"], &[]), "BTCUSDT"),
            SkillVerdict::Eligible { gaps: Vec::new() }
        );
    }

    #[test]
    fn derived_answers_refuse_unless_the_document_accepts_them() {
        // A venue whose candles are *only* direction-attributed: `delta`
        // resolves `derived` there.
        let attributed_only = ProviderDataProfile::new(Provider::Bybit).with_class(
            SymbolClass::Spot,
            ClassProfile::new().with(
                DataKind::Candles,
                KindSupport::both(
                    Channel::candles(SplitQuality::Attributed)
                        .with_note("attributed, not a measurement"),
                    Channel::candles(SplitQuality::Attributed),
                ),
            ),
        );
        let view = view(attributed_only);

        // No fallback: refused, in words.
        let verdict = view.check_skill(&skill(&["delta"], &[]), "BTCUSDT");
        assert!(matches!(verdict, SkillVerdict::Refused { .. }), "{verdict:?}");

        // The document opts in: eligible — and now its own rules must treat
        // the number as the estimate it is.
        let verdict = view.check_skill(
            &skill(&["delta"], &[("delta", crate::skills::FallbackAccept::Derived)]),
            "BTCUSDT",
        );
        assert_eq!(verdict, SkillVerdict::Eligible { gaps: Vec::new() });
    }

    #[test]
    fn a_preferred_miss_is_a_gap_never_a_refusal() {
        let view = view(binance());
        let mut needing = skill(&["footprint"], &[]);
        needing.capability_requirements.preferred = vec![crate::skills::CapabilityNeed {
            capability: "orderbook_snapshots".into(),
        }];
        let SkillVerdict::Eligible { gaps } = view.check_skill(&needing, "BTCUSDT") else {
            panic!("a preferred miss must not refuse")
        };
        assert!(
            gaps.iter()
                .any(|gap| gap.contains("orderbook_snapshots")),
            "{gaps:?}"
        );
    }
}
