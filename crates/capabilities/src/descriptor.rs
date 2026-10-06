//! The capability catalog: one descriptor per analytical capability.
//!
//! ## What a descriptor is
//!
//! The *declaration* half of the capability model: what the capability
//! measures, which data it requires, and which lower-fidelity substitute —
//! if any — yields a [`Basis::Derived`] answer instead of refusing. The
//! provider half (who can supply those kinds) lives in [`crate::profile`]
//! and is declared in `market-data`; the join lives in [`crate::resolver`].
//!
//! ## How to read the rule lists
//!
//! Each capability carries an ordered list of [`SourceRule`]s, best first.
//! Resolution takes the **first rule whose needs the provider profile
//! satisfies**: a capability with a trade-based true rule and a candle-based
//! derived rule resolves `Available` where trades exist and `Derived` where
//! only candles do. A capability with one trade-based rule and nothing else
//! (footprint) resolves `Unavailable` where trades are absent — there is no
//! honest substitute, so there is no rule.
//!
//! ## Editing rules
//!
//! * Add rows; do not rename `id`s. Skills (Phase 2) and MCP clients
//!   (Phase 6) cite ids; a rename is a silent break, exactly the failure the
//!   wire-name tests exist to catch.
//! * `implemented_by: None` means "the platform cannot do this at all, on any
//!   provider" — the row exists so the answer is *explained* rather than
//!   absent. Keep those rows honest: when an engine lands, the descriptor
//!   gains its `implemented_by` and rules in the same change.

use crate::{Basis, DataKind, KindNeed, SplitQuality};

/// One way a capability can be satisfied: every need met ⇒ the capability
/// resolves with this rule's basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRule {
    /// What an answer built on these inputs is.
    pub basis: Basis,
    /// The kinds required, each with its minimum split fidelity.
    pub needs: &'static [KindNeed],
}

/// A coarse family, for grouping in reports and (later) skill requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Raw market data itself.
    MarketData,
    /// Order-flow analytics (delta, footprint, toxicity, ...).
    Orderflow,
    /// Price-structure analytics (swings, BOS/CHoCH, liquidity, zones).
    Structure,
    /// Statistical studies (indicators, divergences, forecast cones).
    Statistical,
    /// Session/volatility context.
    Sessional,
    /// Positioning data (open interest, funding, liquidations, options).
    Positioning,
}

/// The declaration of one analytical capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    /// Stable id, e.g. `"volume_profile"`. Frozen once shipped.
    pub id: &'static str,
    /// One sentence: what it measures.
    pub summary: &'static str,
    /// Family grouping.
    pub category: Category,
    /// The engine that implements it, as a source path for a reader — e.g.
    /// `"analytics_core::volume_profile"`. `None` means **not implemented on
    /// any provider**: the row exists so resolution can say *that*, in words,
    /// instead of the capability being invisible.
    pub implemented_by: Option<&'static str>,
    /// The agent tool that exposes it, when one does.
    pub exposed_tool: Option<&'static str>,
    /// HTTP routes that expose it.
    pub routes: &'static [&'static str],
    /// Ordered satisfaction rules, best first. Empty when unimplemented.
    pub rules: &'static [SourceRule],
    /// Known caveats, surfaced verbatim in resolutions and reports.
    pub notes: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// Reusable need lists. Const items because the descriptor rows are statics.
// ---------------------------------------------------------------------------

const CANDLES: &[KindNeed] = &[KindNeed::of(DataKind::Candles)];
const CANDLES_REAL_SPLIT: &[KindNeed] =
    &[KindNeed::with_split(DataKind::Candles, SplitQuality::Real)];
const CANDLES_ATTRIBUTED_SPLIT: &[KindNeed] =
    &[KindNeed::with_split(DataKind::Candles, SplitQuality::Attributed)];
const TRADES: &[KindNeed] = &[KindNeed::of(DataKind::Trades)];

/// True from trades, true from candles with a real split, derived from
/// candles whose split is direction-attributed. The shared shape of delta,
/// CVD and volume score.
const SPLIT_READ_RULES: &[SourceRule] = &[
    SourceRule {
        basis: Basis::True,
        needs: TRADES,
    },
    SourceRule {
        basis: Basis::True,
        needs: CANDLES_REAL_SPLIT,
    },
    SourceRule {
        basis: Basis::Derived,
        needs: CANDLES_ATTRIBUTED_SPLIT,
    },
];

const TRUE_FROM_TRADES: &[SourceRule] = &[SourceRule {
    basis: Basis::True,
    needs: TRADES,
}];

const TRUE_FROM_CANDLES: &[SourceRule] = &[SourceRule {
    basis: Basis::True,
    needs: CANDLES,
}];

/// The shipped catalog. Order is presentation order in reports.
///
/// One row per row of the audit's capability matrix (the design document's
/// §13.2). A capability that exists in code but not here is invisible to the
/// resolver — the registry's tests pin that every agent tool with data
/// requirements has a row.
pub static STANDARD: &[CapabilityDescriptor] = &[
    CapabilityDescriptor {
        id: "candles",
        summary: "OHLCV candles, the substrate everything else reads",
        category: Category::MarketData,
        implemented_by: Some("market_data::WindowService"),
        exposed_tool: Some("get_candles"),
        routes: &["/candles"],
        rules: TRUE_FROM_CANDLES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "classic_indicators",
        summary: "EMA, SMA, RSI, ATR over closes/candles",
        category: Category::Statistical,
        implemented_by: Some("analytics_core::indicators"),
        exposed_tool: None,
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &[
            "reached through pine-lite `ta.*` and DSL fields, not a per-indicator tool or route",
        ],
    },
    CapabilityDescriptor {
        id: "delta",
        summary: "Per-bar buy-minus-sell volume",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::delta"),
        exposed_tool: Some("get_delta"),
        routes: &[],
        rules: SPLIT_READ_RULES,
        notes: &[
            "an attributed split agrees with the candle by construction; it is not a measurement",
        ],
    },
    CapabilityDescriptor {
        id: "cvd",
        summary: "Cumulative delta and price/CVD divergence",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::cvd"),
        exposed_tool: Some("get_cvd"),
        routes: &[],
        rules: SPLIT_READ_RULES,
        notes: &["inherits the split fidelity of delta"],
    },
    CapabilityDescriptor {
        id: "volume_score",
        summary: "Aggression x relative-volume score",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::volume_score"),
        exposed_tool: None,
        routes: &[],
        rules: SPLIT_READ_RULES,
        notes: &["surfaced inside MarketState and DSL fields"],
    },
    CapabilityDescriptor {
        id: "vwap",
        summary: "Session and anchored VWAP",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::vwap"),
        exposed_tool: Some("get_vwap"),
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &[
            "a venue whose candle volume is a tick count (Deriv) would make this a different metric; that venue must declare it before vwap resolves there",
        ],
    },
    CapabilityDescriptor {
        id: "volume_profile",
        summary: "Volume-at-price: POC, value area, high/low-volume nodes",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::volume_profile"),
        exposed_tool: Some("get_volume_profile"),
        routes: &[],
        rules: &[
            SourceRule {
                basis: Basis::True,
                needs: TRADES,
            },
            SourceRule {
                basis: Basis::Derived,
                needs: CANDLES,
            },
        ],
        notes: &[
            "the candle rule spreads each bar's volume uniformly across its range: fine for the value-area shape, not evidence for per-level order flow",
        ],
    },
    CapabilityDescriptor {
        id: "footprint",
        summary: "Per-price bid x ask cells inside each bar",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::footprint"),
        exposed_tool: Some("get_footprint"),
        routes: &["/footprint", "/footprint/coverage"],
        rules: TRUE_FROM_TRADES,
        notes: &[
            "no derived rule on purpose: a candle-spread footprint is a rendering mode that must never emit imbalances (footprint.rs refuses), not an analysis",
        ],
    },
    CapabilityDescriptor {
        id: "imbalance",
        summary: "Stacked bid/ask imbalances across footprint levels",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::imbalance"),
        exposed_tool: Some("detect_imbalance"),
        routes: &[],
        rules: TRUE_FROM_TRADES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "absorption",
        summary: "High-volume levels that fail to move price",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::absorption"),
        exposed_tool: Some("detect_absorption"),
        routes: &[],
        rules: TRUE_FROM_TRADES,
        notes: &[
            "structurally inert on candle-derived footprints (a uniform spread never reaches the 2x level-volume condition) — which is exactly why the candle path has no rule here",
        ],
    },
    CapabilityDescriptor {
        id: "bar_delta_stats",
        summary: "Intra-bar delta extremes and intrabar VWAP",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::bar_delta"),
        exposed_tool: Some("get_bar_delta_stats"),
        routes: &["/bar-delta-stats"],
        rules: TRUE_FROM_TRADES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "delta_by_size",
        summary: "Delta and CVD bucketed by small/medium/large trade notional",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::size_classes"),
        exposed_tool: Some("get_delta_by_size"),
        routes: &["/delta-by-size"],
        rules: TRUE_FROM_TRADES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "size_divergence",
        summary: "Large- vs small-trade CVD correlation",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::size_classes"),
        exposed_tool: Some("detect_size_divergence"),
        routes: &[],
        rules: TRUE_FROM_TRADES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "vpin",
        summary: "Volume-synchronized probability of informed trading",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::vpin"),
        exposed_tool: Some("get_vpin"),
        routes: &["/vpin"],
        rules: TRUE_FROM_TRADES,
        notes: &["an estimator by design; the basis is true, the value is statistical"],
    },
    CapabilityDescriptor {
        id: "iceberg",
        summary: "Iceberg detection from ordered L2 snapshots plus trades",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::iceberg"),
        exposed_tool: None,
        routes: &["/icebergs"],
        rules: &[SourceRule {
            basis: Basis::True,
            needs: &[
                KindNeed::of(DataKind::OrderBookSnapshots),
                KindNeed::of(DataKind::Trades),
            ],
        }],
        notes: &[
            "book history is a rolling live window (~1000 snapshots), so evidence depth depends on how long the symbol has been watched",
        ],
    },
    CapabilityDescriptor {
        id: "profile_memory",
        summary: "Per-level session delta history over volume profiles",
        category: Category::Orderflow,
        implemented_by: Some("analytics_core::profile_memory"),
        exposed_tool: None,
        routes: &["/profile-memory"],
        rules: &[SourceRule {
            basis: Basis::True,
            needs: &[KindNeed::of(DataKind::Trades), KindNeed::of(DataKind::Candles)],
        }],
        notes: &[],
    },
    CapabilityDescriptor {
        id: "liquidity",
        summary: "Equal highs/lows, sweeps, reclaim verdicts",
        category: Category::Structure,
        implemented_by: Some("analytics_core::liquidity"),
        exposed_tool: Some("detect_liquidity"),
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "market_structure",
        summary: "Swings, trend, breaks of structure and changes of character",
        category: Category::Structure,
        implemented_by: Some("analytics_core::market_structure"),
        exposed_tool: Some("detect_market_structure"),
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "zones",
        summary: "Supply/demand zones, fair value gaps, order blocks, concept patterns",
        category: Category::Structure,
        implemented_by: Some("analytics_core::regions + analytics_core::concepts"),
        exposed_tool: None,
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &["surfaced through the chart scene and derived events"],
    },
    CapabilityDescriptor {
        id: "derived_events",
        summary: "Closed-bar event stream: sweeps, BOS/CHoCH, FVGs, volume spikes, delta shifts",
        category: Category::Structure,
        implemented_by: Some("analytics_core::events"),
        exposed_tool: None,
        routes: &["/events"],
        rules: TRUE_FROM_CANDLES,
        notes: &["the monitor substrate: deterministic, candle-only, free of model calls"],
    },
    CapabilityDescriptor {
        id: "sessions",
        summary: "Session windows (Asia/London/New York) and killzones",
        category: Category::Sessional,
        implemented_by: Some("analytics_core::sessions"),
        exposed_tool: None,
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &[],
    },
    CapabilityDescriptor {
        id: "forecast_cone",
        summary: "Seeded stationary-bootstrap forecast cone",
        category: Category::Statistical,
        implemented_by: Some("analytics_core::forecast"),
        exposed_tool: None,
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &["refuses windows too short for the bootstrap rather than widening them"],
    },
    CapabilityDescriptor {
        id: "rsi_divergence",
        summary: "RSI/price divergence",
        category: Category::Statistical,
        implemented_by: Some("analytics_core::rsi_divergence"),
        exposed_tool: None,
        routes: &[],
        rules: TRUE_FROM_CANDLES,
        notes: &["surfaced inside MarketState"],
    },
    // ------------------------------------------------------------------
    // Declared gaps: rows whose honest answer is "not implemented", so the
    // report can say that in words instead of the capability being absent.
    // ------------------------------------------------------------------
    CapabilityDescriptor {
        id: "greeks_exposure",
        summary: "Options positioning: gamma exposure, dealer hedging levels",
        category: Category::Positioning,
        implemented_by: None,
        exposed_tool: None,
        routes: &[],
        rules: &[],
        notes: &[
            "requires an options chain with greeks inputs and open interest; no provider on this platform supplies one",
        ],
    },
    CapabilityDescriptor {
        id: "funding_oi",
        summary: "Perpetual funding rates and open interest",
        category: Category::Positioning,
        implemented_by: None,
        exposed_tool: None,
        routes: &[],
        rules: &[],
        notes: &["no funding or open-interest streams are ingested"],
    },
    CapabilityDescriptor {
        id: "liquidations",
        summary: "Forced-liquidation prints and clusters",
        category: Category::Positioning,
        implemented_by: None,
        exposed_tool: None,
        routes: &[],
        rules: &[],
        notes: &["no liquidation stream is ingested"],
    },
];

/// Look up a capability by id in the shipped catalog.
#[must_use]
pub fn find(id: &str) -> Option<&'static CapabilityDescriptor> {
    find_in(STANDARD, id)
}

/// Look up a capability by id in an arbitrary catalog — the resolver's entry
/// point, so tests can resolve against fixture catalogs.
#[must_use]
pub fn find_in<'a>(catalog: &'a [CapabilityDescriptor], id: &str) -> Option<&'a CapabilityDescriptor> {
    catalog.iter().find(|descriptor| descriptor.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_self_consistent() {
        for descriptor in STANDARD {
            assert!(
                !descriptor.id.is_empty() && descriptor.id.chars().all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit()),
                "ids are snake_case wire names: {}",
                descriptor.id
            );
            assert!(!descriptor.summary.is_empty(), "{}: no summary", descriptor.id);
            match descriptor.implemented_by {
                Some(_) => assert!(
                    !descriptor.rules.is_empty(),
                    "{}: implemented but has no rules — it could never resolve available",
                    descriptor.id
                ),
                None => assert!(
                    descriptor.rules.is_empty(),
                    "{}: unimplemented rows carry no rules; the explanation does the work",
                    descriptor.id
                ),
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = STANDARD.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate capability ids");
    }

    #[test]
    fn every_tool_with_data_requirements_has_a_row() {
        // The agent's registry (crates/ai-agent/src/tools.rs) dispatches these
        // data-backed tools; a capability one of them exposes that is missing
        // here is one the resolver cannot answer for. Pure plumbing tools
        // (memory, drawings, control) are not data capabilities and have no row.
        for (tool, capability) in [
            ("get_candles", "candles"),
            ("get_volume_profile", "volume_profile"),
            ("get_footprint", "footprint"),
            ("get_delta", "delta"),
            ("get_cvd", "cvd"),
            ("get_vwap", "vwap"),
            ("get_vpin", "vpin"),
            ("get_bar_delta_stats", "bar_delta_stats"),
            ("get_delta_by_size", "delta_by_size"),
            ("detect_size_divergence", "size_divergence"),
            ("detect_liquidity", "liquidity"),
            ("detect_absorption", "absorption"),
            ("detect_imbalance", "imbalance"),
            ("detect_market_structure", "market_structure"),
        ] {
            let descriptor = find(capability).unwrap_or_else(|| panic!("no row for {capability}"));
            assert_eq!(
                descriptor.exposed_tool,
                Some(tool),
                "{capability} names the wrong tool"
            );
        }
    }
}
