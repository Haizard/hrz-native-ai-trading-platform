//! The provider half of the capability model: what a venue can honestly
//! supply.
//!
//! ## Why profiles are types here but *values* in `market-data`
//!
//! The shape of a profile — classes, kinds, channels, split fidelity — is
//! vocabulary and belongs with the vocabulary. The **content** of a profile
//! ("Bybit klines have no taker split") is a venue fact, and venue facts live
//! where the venue code lives: `market-data` constructs these values (see
//! `exchanges/profile.rs` there). Keeping the content next to the codec and
//! venue implementations is what stops the profile drifting from the parser
//! it describes — the same reason `Venue` owns its own column map.
//!
//! ## Channel fidelity, and why it is per channel
//!
//! One venue can serve the same kind at two fidelities: Bybit candles built
//! from the live trade stream carry a *real* split, while its REST klines
//! have no taker column and get a *direction-attributed* split at the venue
//! boundary. A single `split` field per kind would force one of those to
//! lie. So fidelity hangs on the channel ([`Channel`]) and the resolver
//! reports the best channel while the caveats carry the weaker one.

use std::collections::BTreeMap;

use crate::{DataKind, Provider, SplitQuality, SymbolClass};

/// One channel's fidelity for a kind: live stream or REST history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    /// The strongest split this channel can carry. `Absent` for kinds where a
    /// split is not a concept (trades always carry their aggressor side — the
    /// fidelity question does not arise — and a book has no split).
    pub split: SplitQuality,
    /// A caveat that travels with any resolution that used this channel, e.g.
    /// "REST klines are direction-attributed, not a measurement".
    pub note: Option<&'static str>,
}

impl Channel {
    /// A channel with no split fidelity and no caveat.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            split: SplitQuality::Absent,
            note: None,
        }
    }

    /// A channel whose candles carry the given split fidelity.
    #[must_use]
    pub const fn candles(split: SplitQuality) -> Self {
        Self { split, note: None }
    }

    /// Attach a caveat.
    #[must_use]
    pub const fn with_note(mut self, note: &'static str) -> Self {
        self.note = Some(note);
        self
    }
}

/// What a provider supplies for one kind, per channel.
///
/// `None` for a channel means that channel does not serve the kind at all —
/// e.g. Bybit has no trade-history endpoint this platform speaks, so its
/// `rest` is `None` for `Trades` even though its live stream carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KindSupport {
    /// The live stream, when it carries this kind.
    pub live: Option<Channel>,
    /// The REST history, when it serves this kind.
    pub rest: Option<Channel>,
}

impl KindSupport {
    /// Not supplied on any channel.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            live: None,
            rest: None,
        }
    }

    /// Supplied live only.
    #[must_use]
    pub const fn live_only(live: Channel) -> Self {
        Self {
            live: Some(live),
            rest: None,
        }
    }

    /// Supplied on both channels.
    #[must_use]
    pub const fn both(live: Channel, rest: Channel) -> Self {
        Self {
            live: Some(live),
            rest: Some(rest),
        }
    }

    /// Supplied by REST only — a provider with no live stream.
    #[must_use]
    pub const fn rest_only(rest: Channel) -> Self {
        Self {
            live: None,
            rest: Some(rest),
        }
    }

    /// The strongest split any channel carries, for rule matching.
    #[must_use]
    pub fn best_split(&self) -> SplitQuality {
        [self.live, self.rest]
            .into_iter()
            .flatten()
            .map(|channel| channel.split)
            .max()
            .unwrap_or(SplitQuality::Absent)
    }

    /// Whether any channel supplies the kind.
    #[must_use]
    pub fn is_supplied(&self) -> bool {
        self.live.is_some() || self.rest.is_some()
    }

    /// Whether history can be fetched for this kind. A live-only kind means
    /// the analysis window is bounded by how long the symbol has been
    /// watched — a fact the resolver surfaces as a caveat.
    #[must_use]
    pub fn has_history(&self) -> bool {
        self.rest.is_some()
    }

    /// Caveats from the channels that are *weaker* than the best one — the
    /// facts a consumer must know before trusting a specific read. The best
    /// channel's own fidelity is expressed by the resolution's basis.
    pub fn caveats(&self) -> Vec<&'static str> {
        let best = self.best_split();
        let mut out = Vec::new();
        for channel in [self.live, self.rest].into_iter().flatten() {
            if channel.split < best {
                if let Some(note) = channel.note {
                    out.push(note);
                }
            }
        }
        out
    }
}

/// One symbol class's data inventory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClassProfile {
    kinds: BTreeMap<DataKind, KindSupport>,
}

impl ClassProfile {
    /// An empty inventory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a kind. Builder-style; the last declaration wins, so a
    /// profile reads as one statement per kind.
    #[must_use]
    pub fn with(mut self, kind: DataKind, support: KindSupport) -> Self {
        self.kinds.insert(kind, support);
        self
    }

    /// What this class supplies for a kind. `None` = not supplied at all.
    #[must_use]
    pub fn kind(&self, kind: DataKind) -> Option<&KindSupport> {
        self.kinds.get(&kind)
    }

    /// Every declared kind, for reports.
    pub fn kinds(&self) -> impl Iterator<Item = (DataKind, &KindSupport)> {
        self.kinds.iter().map(|(kind, support)| (*kind, support))
    }
}

/// One provider's data truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDataProfile {
    /// Which provider this describes.
    pub provider: Provider,
    /// The data inventory per symbol class.
    pub classes: BTreeMap<SymbolClass, ClassProfile>,
    /// Provider-wide caveats, surfaced on every resolution against it.
    pub caveats: &'static [&'static str],
}

impl ProviderDataProfile {
    /// A provider with no classes yet.
    #[must_use]
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            classes: BTreeMap::new(),
            caveats: &[],
        }
    }

    /// Declare a symbol class. Builder-style, like [`ClassProfile::with`].
    #[must_use]
    pub fn with_class(mut self, class: SymbolClass, profile: ClassProfile) -> Self {
        self.classes.insert(class, profile);
        self
    }

    /// Provider-wide caveats.
    #[must_use]
    pub fn with_caveats(mut self, caveats: &'static [&'static str]) -> Self {
        self.caveats = caveats;
        self
    }

    /// The inventory for a class. `None` = this provider has no such
    /// instruments (a spot-only venue asked about perpetuals).
    #[must_use]
    pub fn class(&self, class: SymbolClass) -> Option<&ClassProfile> {
        self.classes.get(&class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_only_kind_reports_no_history() {
        let support = KindSupport::live_only(Channel::plain());
        assert!(support.is_supplied());
        assert!(!support.has_history());
    }

    #[test]
    fn the_weaker_channel_contributes_the_caveat() {
        // The shape this exists for: live candles real, REST candles
        // attributed. The resolution's basis reflects the best channel; the
        // caveat must carry the REST half or a consumer reading history
        // believes a measurement it does not have.
        let support = KindSupport::both(
            Channel::candles(SplitQuality::Real),
            Channel::candles(SplitQuality::Attributed)
                .with_note("REST klines are direction-attributed, not a measurement"),
        );
        assert_eq!(support.best_split(), SplitQuality::Real);
        assert_eq!(support.caveats(), vec!["REST klines are direction-attributed, not a measurement"]);

        // When live is absent the REST channel *is* the best: its caveat then
        // travels through the resolution's basis (derived) instead of the
        // caveats list, so `caveats()` is empty.
        let rest_only = KindSupport::rest_only(
            Channel::candles(SplitQuality::Attributed).with_note("attributed"),
        );
        assert!(rest_only.caveats().is_empty());
    }
}
