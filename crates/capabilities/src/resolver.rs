//! The join: catalog + profiles + a scope → an honest answer.
//!
//! ## What the resolver is and is not
//!
//! It is a **pure function over declarations**: descriptors say what a
//! capability needs, profiles say what a provider supplies, and
//! [`Registry::resolve`] answers with an [`Availability`], a [`Basis`] when
//! one exists, and an explanation written for the two consumers that read it
//! — the agent's prompt and the UI.
//!
//! It is **not** a market-data reader. Whether the live tape currently holds
//! trades for `BTCUSDT` is a window fact the gateway has and this crate does
//! not; Phase 1 layers those facts on at the call site (the tool layer) as
//! `Degraded` adjustments. What this resolver guarantees is the *static*
//! truth: a provider that can never supply trades resolves footprint
//! `Unavailable` no matter what happens to be in RAM, and one that can
//! resolves it `Available` with the caveat that history may be live-only.
//!
//! ## Why a concrete registry and not a trait
//!
//! There is exactly one resolution algorithm and one catalog. A trait would
//! exist to be mocked, and a mocked resolver is how a tool test ends up
//! asserting against availability answers the real registry would never
//! give. Constructing a `Registry` from two `Vec`s is already cheap enough
//! for tests; the day a second resolution strategy exists is the day the
//! trait is earned.

use std::collections::BTreeMap;

use analytics_core::Timeframe;
use serde::Serialize;

use crate::descriptor::{self, CapabilityDescriptor};
use crate::profile::ProviderDataProfile;
use crate::{Availability, Basis, DataKind, Provider, SymbolClass};

/// What a resolution is about: a provider, a symbol class, and the symbol
/// and timeframes being asked about.
///
/// The symbol and timeframes do not affect Phase 0 resolution — the static
/// profiles are per (provider, class) — but they are part of the question
/// from day one: per-symbol overrides ("this symbol is an index, not a
/// perpetual") and per-timeframe refusals (a venue without weekly bars) land
/// on these fields without a signature change, and the explanation strings
/// name the symbol so an agent can quote the answer back to a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataScope {
    /// The market being read.
    pub provider: Provider,
    /// What kind of instrument the symbol is.
    pub symbol_class: SymbolClass,
    /// The instrument, e.g. `BTCUSDT`. Carried for explanations and future
    /// per-symbol overrides.
    pub symbol: String,
    /// The resolutions in play. Carried for future per-timeframe answers.
    pub timeframes: Vec<Timeframe>,
}

impl DataScope {
    /// A scope for one symbol on one provider.
    #[must_use]
    pub fn new(provider: Provider, symbol_class: SymbolClass, symbol: impl Into<String>) -> Self {
        Self {
            provider,
            symbol_class,
            symbol: symbol.into(),
            timeframes: Vec::new(),
        }
    }

    /// Attach the timeframes in play.
    #[must_use]
    pub fn with_timeframes(mut self, timeframes: impl IntoIterator<Item = Timeframe>) -> Self {
        self.timeframes = timeframes.into_iter().collect();
        self
    }
}

/// The answer to one capability question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The capability asked about.
    pub capability: &'static str,
    /// The verdict.
    pub availability: Availability,
    /// What an answer rests on, when one exists: `true` for the measurement,
    /// `derived` for a declared substitute.
    pub basis: Option<Basis>,
    /// The kinds whose absence blocks the best rule, when unavailable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<DataKind>,
    /// Caveats a consumer must see: provider-wide notes, weaker-channel
    /// notes, live-only-window notes, and the descriptor's own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
    /// One sentence, written for a user or an agent prompt.
    pub explanation: String,
}

/// Catalog + profiles, and the resolution join over them.
///
/// Assembled once at boot (the gateway collects profiles from `market-data`)
/// and immutable afterwards: there is no runtime mutation to race, and the
/// whole registry is cheap to clone into an `Arc`.
#[derive(Debug, Clone)]
pub struct Registry {
    catalog: &'static [CapabilityDescriptor],
    profiles: BTreeMap<Provider, ProviderDataProfile>,
}

impl Registry {
    /// Assemble from the catalog and the declared provider profiles.
    #[must_use]
    pub fn new(
        catalog: &'static [CapabilityDescriptor],
        profiles: Vec<ProviderDataProfile>,
    ) -> Self {
        Self {
            catalog,
            profiles: profiles
                .into_iter()
                .map(|profile| (profile.provider, profile))
                .collect(),
        }
    }

    /// The capability catalog.
    #[must_use]
    pub fn catalog(&self) -> &'static [CapabilityDescriptor] {
        self.catalog
    }

    /// The declared providers.
    pub fn profiles(&self) -> impl Iterator<Item = &ProviderDataProfile> {
        self.profiles.values()
    }

    /// One provider's profile.
    #[must_use]
    pub fn profile(&self, provider: Provider) -> Option<&ProviderDataProfile> {
        self.profiles.get(&provider)
    }

    /// The capability a tool exposes, when it is a data-backed one.
    ///
    /// The join key is the descriptor's `exposed_tool`, so a rename on either
    /// side — the tool or the descriptor — simply stops matching, and the
    /// catalog's `every_tool_with_data_requirements_has_a_row` test is what
    /// catches it. A scan, not a map: the catalog is two dozen rows and this
    /// runs once per tool call.
    #[must_use]
    pub fn for_tool(&self, tool: &str) -> Option<&'static CapabilityDescriptor> {
        self.catalog
            .iter()
            .find(|descriptor| descriptor.exposed_tool == Some(tool))
    }

    /// Resolve one capability against one scope.
    ///
    /// Total by design: unknown ids, undeclared providers and missing kinds
    /// all resolve to `Unavailable` with the explanation doing the work,
    /// because the consumers (agent, UI, MCP) need an answer they can show,
    /// not an error they must translate.
    #[must_use]
    pub fn resolve(&self, id: &str, scope: &DataScope) -> Resolution {
        let Some(descriptor) = descriptor::find_in(self.catalog, id) else {
            return Resolution {
                capability: "unknown",
                availability: Availability::Unavailable,
                basis: None,
                missing: Vec::new(),
                caveats: Vec::new(),
                explanation: format!(
                    "no capability named `{id}` is registered; the catalog holds {} entries",
                    self.catalog.len()
                ),
            };
        };

        if descriptor.implemented_by.is_none() {
            return Resolution {
                capability: descriptor.id,
                availability: Availability::Unavailable,
                basis: None,
                missing: Vec::new(),
                caveats: descriptor.notes.iter().map(|note| (*note).to_string()).collect(),
                explanation: format!(
                    "{} is not implemented on this platform{}",
                    descriptor.id,
                    descriptor
                        .notes
                        .first()
                        .map_or(String::new(), |note| format!(": {note}"))
                ),
            };
        }

        let Some(profile) = self.profiles.get(&scope.provider) else {
            return Resolution {
                capability: descriptor.id,
                availability: Availability::Unavailable,
                basis: None,
                missing: Vec::new(),
                caveats: Vec::new(),
                explanation: format!(
                    "provider {} is not declared in this deployment's registry",
                    scope.provider
                ),
            };
        };

        let Some(class) = profile.class(scope.symbol_class) else {
            return Resolution {
                capability: descriptor.id,
                availability: Availability::Unavailable,
                basis: None,
                missing: Vec::new(),
                caveats: profile.caveats.iter().map(|c| (*c).to_string()).collect(),
                explanation: format!(
                    "{} has no {} instruments ({} on {} is not a thing this provider serves)",
                    scope.provider,
                    scope.symbol_class,
                    scope.symbol,
                    scope.provider
                ),
            };
        };

        // First satisfiable rule wins — the rules are ordered best-first, so
        // the matched rule *is* the best answer this provider can give.
        for rule in descriptor.rules {
            let missing: Vec<DataKind> = rule
                .needs
                .iter()
                .filter(|need| {
                    class
                        .kind(need.kind)
                        .is_none_or(|support| {
                            !support.is_supplied() || support.best_split() < need.min_split
                        })
                })
                .map(|need| need.kind)
                .collect();
            if !missing.is_empty() {
                continue;
            }

            let mut caveats: Vec<String> = Vec::new();
            caveats.extend(profile.caveats.iter().map(|c| (*c).to_string()));
            for need in rule.needs {
                if let Some(support) = class.kind(need.kind) {
                    caveats.extend(support.caveats().iter().map(|c| (*c).to_string()));
                    if !support.has_history() {
                        caveats.push(format!(
                            "{} is live-window only on {}: no historical fetch, so depth depends on how long the symbol has been watched",
                            need.kind, scope.provider
                        ));
                    }
                }
            }
            caveats.extend(descriptor.notes.iter().map(|note| (*note).to_string()));

            let availability = match rule.basis {
                Basis::True => Availability::Available,
                Basis::Derived => Availability::Derived,
            };
            let explanation = match rule.basis {
                Basis::True => format!(
                    "{} is available on {} ({}) from {}",
                    descriptor.id, scope.provider, scope.symbol_class, scope.symbol
                ),
                Basis::Derived => format!(
                    "{} on {} ({}) is derived, not measured: the true inputs are absent and the answer rests on {}",
                    descriptor.id,
                    scope.provider,
                    scope.symbol_class,
                    rule.needs
                        .iter()
                        .map(|need| need.kind.as_str())
                        .collect::<Vec<_>>()
                        .join(" + ")
                ),
            };
            return Resolution {
                capability: descriptor.id,
                availability,
                basis: Some(rule.basis),
                missing: Vec::new(),
                caveats,
                explanation,
            };
        }

        // No rule satisfied: name what the best rule lacked.
        let missing: Vec<DataKind> = descriptor
            .rules
            .first()
            .map(|rule| {
                rule.needs
                    .iter()
                    .filter(|need| class.kind(need.kind).is_none_or(|s| !s.is_supplied()))
                    .map(|need| need.kind)
                    .collect()
            })
            .unwrap_or_default();
        let mut caveats: Vec<String> = profile.caveats.iter().map(|c| (*c).to_string()).collect();
        caveats.extend(descriptor.notes.iter().map(|note| (*note).to_string()));
        Resolution {
            capability: descriptor.id,
            availability: Availability::Unavailable,
            basis: None,
            missing: missing.clone(),
            caveats,
            explanation: format!(
                "{} is unavailable on {} ({}): {} cannot supply {}",
                descriptor.id,
                scope.provider,
                scope.symbol_class,
                scope.provider,
                if missing.is_empty() {
                    "the required split fidelity".to_string()
                } else {
                    missing
                        .iter()
                        .map(|kind| kind.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        }
    }

    /// Resolve every catalog row against one scope — the shape of the
    /// `/capabilities` analysis view and, later, the agent's prompt summary.
    #[must_use]
    pub fn summary(&self, scope: &DataScope) -> Vec<Resolution> {
        self.catalog
            .iter()
            .map(|descriptor| self.resolve(descriptor.id, scope))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::STANDARD;
    use crate::profile::{Channel, ClassProfile, KindSupport};
    use crate::SplitQuality;

    /// A Binance-shaped fixture: real splits on both channels, trade history
    /// via aggTrades, live book. The real profile lives in market-data and is
    /// pinned by its own tests; these fixtures exist so the resolver's
    /// behaviour is tested against intent, not against whatever the venues
    /// currently declare.
    fn binance_like() -> ProviderDataProfile {
        let spot = ClassProfile::new()
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
            )
            .with(
                DataKind::OrderBookSnapshots,
                KindSupport::live_only(Channel::plain()),
            );
        ProviderDataProfile::new(Provider::Binance).with_class(SymbolClass::Spot, spot)
    }

    /// A Deriv-shaped fixture: candles and ticks, nothing else. The pilot the
    /// whole model exists for (the design document's §22.2).
    fn deriv_like() -> ProviderDataProfile {
        let synthetic = ClassProfile::new()
            .with(DataKind::Candles, KindSupport::both(
                Channel::candles(SplitQuality::Absent),
                Channel::candles(SplitQuality::Absent),
            ))
            .with(DataKind::Ticks, KindSupport::live_only(Channel::plain()));
        // Deliberately reusing a Provider variant for the fixture: the
        // resolver keys on the enum, and the fixture is about the *profile
        // shape*, not the name.
        ProviderDataProfile::new(Provider::Bybit)
            .with_class(SymbolClass::SyntheticIndex, synthetic)
    }

    fn registry() -> Registry {
        Registry::new(STANDARD, vec![binance_like(), deriv_like()])
    }

    #[test]
    fn footprint_is_available_where_trades_exist() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry().resolve("footprint", &scope);
        assert_eq!(resolution.availability, Availability::Available);
        assert_eq!(resolution.basis, Some(Basis::True));
    }

    #[test]
    fn footprint_is_unavailable_where_trades_cannot_exist() {
        let scope = DataScope::new(Provider::Bybit, SymbolClass::SyntheticIndex, "R_100");
        let resolution = registry().resolve("footprint", &scope);
        assert_eq!(resolution.availability, Availability::Unavailable);
        assert_eq!(resolution.missing, vec![DataKind::Trades]);
        assert!(
            resolution.explanation.contains("trades"),
            "the explanation names the missing kind: {}",
            resolution.explanation
        );
        // And there is no derived answer: footprint has no candle rule.
        assert_eq!(resolution.basis, None);
    }

    #[test]
    fn volume_profile_degrades_to_derived_on_candles_only() {
        let scope = DataScope::new(Provider::Bybit, SymbolClass::SyntheticIndex, "R_100");
        let resolution = registry().resolve("volume_profile", &scope);
        assert_eq!(resolution.availability, Availability::Derived);
        assert_eq!(resolution.basis, Some(Basis::Derived));
        assert!(
            resolution.explanation.contains("derived"),
            "{}",
            resolution.explanation
        );
        assert!(
            resolution
                .caveats
                .iter()
                .any(|c| c.contains("uniformly")),
            "the uniform-spread caveat must travel with the derived answer: {:?}",
            resolution.caveats
        );
    }

    #[test]
    fn candle_only_scopes_keep_structure_analytics() {
        // The Deriv story's other half: everything price-derived still works.
        let scope = DataScope::new(Provider::Bybit, SymbolClass::SyntheticIndex, "R_100");
        for id in ["market_structure", "liquidity", "derived_events", "forecast_cone"] {
            let resolution = registry().resolve(id, &scope);
            assert_eq!(
                resolution.availability,
                Availability::Available,
                "{id} must survive on candles: {resolution:?}"
            );
        }
    }

    #[test]
    fn an_attributed_split_makes_delta_derived_not_true() {
        // A Bybit-REST-shaped profile: candles exist but their split is
        // direction-attributed. Delta has a rule for exactly this and it must
        // answer `derived` — the audit's provenance-leak finding, encoded.
        let profile = ProviderDataProfile::new(Provider::Bybit).with_class(
            SymbolClass::Spot,
            ClassProfile::new().with(
                DataKind::Candles,
                KindSupport::rest_only(
                    Channel::candles(SplitQuality::Attributed)
                        .with_note("REST klines are direction-attributed, not a measurement"),
                ),
            ),
        );
        let registry = Registry::new(STANDARD, vec![profile]);
        let scope = DataScope::new(Provider::Bybit, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry.resolve("delta", &scope);
        assert_eq!(resolution.availability, Availability::Derived);
        assert_eq!(resolution.basis, Some(Basis::Derived));
    }

    #[test]
    fn unimplemented_capabilities_say_so_in_words() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry().resolve("greeks_exposure", &scope);
        assert_eq!(resolution.availability, Availability::Unavailable);
        assert!(
            resolution.explanation.contains("not implemented"),
            "{}",
            resolution.explanation
        );
        assert!(
            resolution.missing.is_empty(),
            "an unimplemented capability is not a *data* gap: {resolution:?}"
        );
    }

    #[test]
    fn live_only_kinds_carry_the_window_caveat() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry().resolve("iceberg", &scope);
        assert_eq!(resolution.availability, Availability::Available);
        assert!(
            resolution
                .caveats
                .iter()
                .any(|c| c.contains("live-window only")),
            "icebergs over a live-only book history must say so: {:?}",
            resolution.caveats
        );
    }

    #[test]
    fn an_undeclared_class_is_refused_by_name() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Linear, "BTCUSDT");
        let resolution = registry().resolve("candles", &scope);
        assert_eq!(resolution.availability, Availability::Unavailable);
        assert!(resolution.explanation.contains("linear"), "{}", resolution.explanation);
    }

    #[test]
    fn unknown_capabilities_resolve_unavailable_rather_than_error() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry().resolve("gema", &scope);
        assert_eq!(resolution.availability, Availability::Unavailable);
        assert!(resolution.explanation.contains("gema"));
    }

    #[test]
    fn the_summary_covers_the_catalog() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let summary = registry().summary(&scope);
        assert_eq!(summary.len(), STANDARD.len());
    }

    #[test]
    fn resolutions_serialize_for_the_report() {
        let scope = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry().resolve("footprint", &scope);
        let json = serde_json::to_value(&resolution).expect("serializes");
        assert_eq!(json["capability"], "footprint");
        assert_eq!(json["availability"], "available");
        assert_eq!(json["basis"], "true");
        // Empty lists are skipped so the report stays readable.
        assert!(json.get("missing").is_none());
    }
}
