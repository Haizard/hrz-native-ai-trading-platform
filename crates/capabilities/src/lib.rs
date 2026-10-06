//! The capability layer: what an analysis needs, what a provider can honestly
//! supply, and the one place those two facts meet (`docs/38`).
//!
//! ## Why this crate exists
//!
//! Before it, the platform answered "can we do footprint analysis here?" in
//! three disconnected places: the venue layer knew a kline has no taker split
//! (`RawKline::taker_buy_base: Option`), the tool layer guessed from data
//! presence (`trades.is_empty()`), and the deployment report
//! (`api-gateway/src/capabilities.rs`) knew which *services* were configured.
//! Nobody could answer the question **before** a tool ran, and nobody could
//! say it per provider. A second provider with different data (Deriv: ticks,
//! no order book) would have had to rediscover every answer one failed tool
//! call at a time.
//!
//! This crate holds the three things that answer it:
//!
//! * the **vocabulary** (`DataKind`, `Availability`, `Basis`, `SplitQuality`)
//!   — the closed set of data kinds the platform can read and the honesty
//!   labels an answer may carry;
//! * the **catalog** ([`descriptor`]) — one [`CapabilityDescriptor`] per
//!   analytical capability, declaring which data it requires, which it
//!   prefers, and whether a lower-fidelity substitute yields a *derived*
//!   answer rather than a true one;
//! * the **registry** ([`resolver::Registry`]) — catalog + provider profiles
//!   (declared in `market-data`, the only place a venue's truth lives) +
//!   the pure [`Registry::resolve`] join.
//!
//! ## The rules the whole platform inherits from this crate
//!
//! 1. **Availability is a function of (capability, provider, symbol class),
//!    never of the capability alone.** The resolver takes a [`DataScope`];
//!    there is no provider-less answer.
//! 2. **True and derived are different answers.** A candle-spread volume
//!    profile resolves `Derived`, never `Available`. A capability that would
//!    fabricate its input (footprint from plain candles) has no derived rule
//!    at all — it resolves `Unavailable` with an explanation.
//! 3. **Unavailable is a first-class answer with an explanation**, written for
//!    the two readers that consume it: the AI agent's prompt and the UI.
//! 4. **This crate computes nothing about markets.** It is a leaf like
//!    `analytics-core`: no I/O, no async, wasm32-clean. The resolver reads
//!    profiles and descriptors, never market data. Dynamic facts (how deep
//!    the live window currently is, whether a feed is stale) join in a later
//!    phase at the call site, which has them.
//!
//! ## What this phase deliberately does not do
//!
//! Tool results are not stamped yet, skills do not declare requirements yet,
//! and `/capabilities` only *gains* the analysis view — the per-tool
//! `trades.is_empty()` guards stay as the enforcement floor until Phase 1
//! moves them onto the resolver. See `docs/38` for the phase split.

pub mod descriptor;
pub mod profile;
pub mod resolver;

pub use descriptor::{CapabilityDescriptor, Category};
pub use profile::{Channel, ClassProfile, KindSupport, ProviderDataProfile};
pub use resolver::{DataScope, Registry, Resolution};

use serde::{Deserialize, Serialize};

/// A market the platform can read from.
///
/// Closed on purpose: a provider is not a string a client sends, it is a
/// declared profile. New providers are new variants plus a profile in
/// `market-data`, never a config value that resolves to whatever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Binance spot: trades (live + aggTrades REST), order book, klines with
    /// a real taker-buy split.
    Binance,
    /// Bybit v5: trades and order book live, klines without a taker split.
    Bybit,
}

impl Provider {
    /// The wire name, e.g. `"binance"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Bybit => "bybit",
        }
    }

    /// Every declared provider.
    pub const ALL: [Self; 2] = [Self::Binance, Self::Bybit];
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of instrument a symbol is, as far as data shape goes.
///
/// The class decides the profile section a scope resolves against: a venue
/// can serve candles for everything but an order book for nothing (a
/// synthetic index), or klines with a split for spot only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolClass {
    /// Spot instruments.
    Spot,
    /// Linear perpetuals.
    Linear,
    /// Inverse perpetuals.
    Inverse,
    /// Provider-computed synthetic indices (e.g. Deriv's) — candles and
    /// ticks, no book, no splits, no funding.
    SyntheticIndex,
}

impl SymbolClass {
    /// The wire name, e.g. `"synthetic_index"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Linear => "linear",
            Self::Inverse => "inverse",
            Self::SyntheticIndex => "synthetic_index",
        }
    }
}

impl std::fmt::Display for SymbolClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A kind of market data, at the granularity capabilities reason about.
///
/// Closed on purpose — rule 2 of the crate: a kind is added by a variant
/// here plus profile entries, so "the provider sends something new" is a
/// compile-time event, not a silent loss. `Ticks` and `Trades` are different
/// kinds because a tick carries no aggressor side and no size: analytics that
/// need those must not silently accept ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    /// OHLCV bars. Buy/sell split fidelity is a property of the *channel*
    /// (`SplitQuality`), not a separate kind.
    Candles,
    /// Executed trades with aggressor side and size (Binance aggTrade counts:
    /// aggregated but side-true).
    Trades,
    /// Price prints without side or size (a Deriv-style stream).
    Ticks,
    /// Synced level-2 books over time, not just the newest.
    OrderBookSnapshots,
    /// Open interest series.
    OpenInterest,
    /// Funding rate series.
    Funding,
    /// Forced-liquidation prints.
    Liquidations,
    /// Option chains with strikes, greeks inputs and open interest.
    OptionsChain,
}

impl DataKind {
    /// The wire name, e.g. `"order_book_snapshots"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candles => "candles",
            Self::Trades => "trades",
            Self::Ticks => "ticks",
            Self::OrderBookSnapshots => "order_book_snapshots",
            Self::OpenInterest => "open_interest",
            Self::Funding => "funding",
            Self::Liquidations => "liquidations",
            Self::OptionsChain => "options_chain",
        }
    }
}

impl std::fmt::Display for DataKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a channel's candles know their buy/sell split.
///
/// Ordered weakest to strongest so `KindNeed::min_split` can say "at least
/// attributed". The distinctions are load-bearing: a delta read from a
/// `Real` split is a measurement; from an `Attributed` split it is a
/// direction guess that agrees with the candle by construction — the venue
/// layer has always known this (`venue.rs`'s `taker_buy_base: Option`), and
/// this enum is what carries the fact above the data layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitQuality {
    /// No split requirement, or no split at all.
    Absent,
    /// Direction-attributed (close >= open ⇒ all volume "buy"). Agrees with
    /// the candle by construction; not a measurement.
    Attributed,
    /// A venue-measured taker-buy split, or trade-built candles (whose split
    /// comes from real aggressor sides).
    Real,
}

impl SplitQuality {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Attributed => "attributed",
            Self::Real => "real",
        }
    }
}

/// Whether a resolved answer is the true measurement or a declared
/// lower-fidelity substitute.
///
/// A derived answer is not a failed one — a candle-spread volume profile's
/// value area is genuinely useful — but it must never be *read* as the true
/// one, which is why this is a field on every resolution rather than a note
/// in prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The capability's declared measurement.
    True,
    /// A documented approximation from a lower-fidelity kind, e.g. a volume
    /// profile spread uniformly across a candle's range.
    Derived,
}

impl Basis {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::Derived => "derived",
        }
    }
}

/// The answer to "can this capability run here, and how honestly".
///
/// Five values rather than a boolean because each calls for different action:
/// `Derived` says "use it, label it"; `Degraded` says "present but suspect";
/// `Partial` says "some of the scope only"; `Unavailable` says "do not
/// pretend". Phase 0's static resolution emits only `Available`, `Derived`
/// and `Unavailable` — `Partial` and `Degraded` need per-timeframe and
/// window-depth facts that join in Phase 1 — but the wire vocabulary is
/// frozen now so consumers never see a new variant appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The capability's true-basis rule is satisfiable on this scope.
    Available,
    /// Only a declared lower-fidelity rule is satisfiable. The answer exists
    /// and is labeled.
    Derived,
    /// Some timeframes or symbols in the scope only. (Phase 1; not emitted by
    /// static resolution.)
    Partial,
    /// Present but suspect — stale feed, thin window. (Phase 1; needs the
    /// gateway's freshness facts.)
    Degraded,
    /// No rule is satisfiable. The explanation names the missing kinds.
    Unavailable,
}

impl Availability {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Derived => "derived",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One data kind a [`descriptor::SourceRule`] needs, with its minimum split
/// fidelity.
///
/// The split floor is per-need rather than per-capability because one
/// capability can read the same kind at two fidelities: `market_structure`
/// takes candles with any split, `delta` has a true rule that requires a real
/// split and a derived rule that accepts an attributed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindNeed {
    /// The kind required.
    pub kind: DataKind,
    /// The weakest split this need accepts. `Absent` means "no split
    /// requirement" — price and volume only.
    pub min_split: SplitQuality,
}

impl KindNeed {
    /// A need with no split requirement.
    #[must_use]
    pub const fn of(kind: DataKind) -> Self {
        Self {
            kind,
            min_split: SplitQuality::Absent,
        }
    }

    /// A need with a minimum split fidelity.
    #[must_use]
    pub const fn with_split(kind: DataKind, min_split: SplitQuality) -> Self {
        Self { kind, min_split }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_quality_is_ordered_weakest_to_strongest() {
        // The ordering is the comparison `min_split` relies on. Declared
        // order *is* the order; this test pins that nobody inserts a variant
        // in the middle and silently re-grades every derived answer.
        assert!(SplitQuality::Absent < SplitQuality::Attributed);
        assert!(SplitQuality::Attributed < SplitQuality::Real);
    }

    #[test]
    fn wire_names_are_snake_case_and_frozen() {
        // Consumers branch on these strings and there is no schema anywhere
        // that checks them, so a rename is a silent break -- the same failure
        // shape as the gateway's readiness names. Pin the whole vocabulary.
        assert_eq!(
            serde_json::to_value(Availability::Derived).unwrap(),
            "derived"
        );
        assert_eq!(
            serde_json::to_value(DataKind::OrderBookSnapshots).unwrap(),
            "order_book_snapshots"
        );
        assert_eq!(
            serde_json::to_value(SymbolClass::SyntheticIndex).unwrap(),
            "synthetic_index"
        );
        assert_eq!(serde_json::to_value(Provider::Bybit).unwrap(), "bybit");
        assert_eq!(serde_json::to_value(Basis::True).unwrap(), "true");
        assert_eq!(
            serde_json::to_value(SplitQuality::Attributed).unwrap(),
            "attributed"
        );
    }
}
