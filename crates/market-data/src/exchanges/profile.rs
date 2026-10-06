//! What each venue can honestly supply, declared as data (`docs/38`).
//!
//! ## Why this file exists, and why it lives here
//!
//! The capability registry (`capabilities` crate) joins two declarations:
//! what an analysis *needs* (the catalog, in the leaf crate) and what a
//! provider *supplies* (here). The values below are the **only** place those
//! supply facts are stated, and they sit next to the codecs and venues that
//! make them true — `BinanceVenue::columns` names the taker-buy column that
//! makes `SplitQuality::Real` honest, `bybit_codec` is what makes the live
//! channel exist. A profile that lived anywhere else would drift from the
//! parser it describes.
//!
//! ## What a profile is not
//!
//! It is not a claim about the *current* deployment: a profile says the
//! venue **can** serve trades live, not that a feed is running right now.
//! Whether one is running is the freshness half of `/capabilities`
//! (`api-gateway/src/capabilities.rs`), and Phase 1's `Degraded` adjustments
//! join the two at the call site. This file is the static truth the dynamic
//! half degrades from.
//!
//! It is also not a `Venue` trait method. A profile spans **both** seams —
//! the live codec and the REST venue — and `Venue` is the REST half only.
//! Free functions keep the two-seam structure visible rather than hiding the
//! live half behind a trait named for the historical one.

use capabilities::profile::{Channel, ClassProfile, KindSupport, ProviderDataProfile};
use capabilities::{DataKind, Provider, SplitQuality, SymbolClass};

/// Binance spot.
///
/// Facts and their sources:
///
/// * klines carry `takerBuyBaseAssetVolume` (venue.rs `Columns.taker_buy_base:
///   Some(9)`), so REST candles have a **real** split;
/// * live candles are trade-built (`candle_builder.rs`), so they have a real
///   split too;
/// * trade history is pageable via `/api/v3/aggTrades` (backfill.rs) —
///   **aggregate** trades: side-true, but several executions can share a row;
/// * the book is a synced live stream (orderbook.rs); there is no historical
///   book fetch.
#[must_use]
pub fn binance() -> ProviderDataProfile {
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

    ProviderDataProfile::new(Provider::Binance)
        .with_class(SymbolClass::Spot, spot)
        .with_caveats(&[
            "spot instruments only: no perpetuals, funding, open interest or options are ingested",
            "trade history is aggregate trades (aggTrade): side-true, but several executions can share one row",
        ])
}

/// Bybit v5, all three product categories.
///
/// Facts and their sources:
///
/// * v5 klines are seven columns and the seventh is *turnover* — there is no
///   taker-buy column (venue.rs `BybitVenue::columns`), so REST candles get a
///   **direction-attributed** split at the venue boundary, which the venue
///   layer itself documents as "not a measurement";
/// * live candles are trade-built, so the live channel's split is real;
/// * trades and the book are live-only: no trade-history endpoint is spoken
///   (backfill.rs notes aggTrades is a Binance-only capability until a venue
///   supplies one).
///
/// The categories share one inventory today — nothing about the split or the
/// channels differs between spot and perpetual klines — but they are declared
/// separately because funding and open interest will land on the perpetual
/// classes when they arrive, and a shared class would have to be split then.
#[must_use]
pub fn bybit() -> ProviderDataProfile {
    let inventory = || {
        ClassProfile::new()
            .with(
                DataKind::Candles,
                KindSupport::both(
                    Channel::candles(SplitQuality::Real),
                    Channel::candles(SplitQuality::Attributed).with_note(
                        "Bybit klines carry no taker split: buy/sell volume over REST history is \
                         direction-attributed (close >= open), which agrees with the candle by \
                         construction and is not a measurement",
                    ),
                ),
            )
            .with(DataKind::Trades, KindSupport::live_only(Channel::plain()))
            .with(
                DataKind::OrderBookSnapshots,
                KindSupport::live_only(Channel::plain()),
            )
    };

    ProviderDataProfile::new(Provider::Bybit)
        .with_class(SymbolClass::Spot, inventory())
        .with_class(SymbolClass::Linear, inventory())
        .with_class(SymbolClass::Inverse, inventory())
        .with_caveats(&[
            "REST history is klines only: neither trade nor book history can be fetched, so \
             trade-based analytics are live-window analyses on this venue",
        ])
}

/// Every declared provider, in report order.
///
/// The gateway assembles its capability registry from this plus
/// `capabilities::descriptor::STANDARD`; a new provider is a new function
/// above plus one entry here.
#[must_use]
pub fn standard() -> Vec<ProviderDataProfile> {
    vec![binance(), bybit()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use capabilities::descriptor;
    use capabilities::{Availability, Basis, DataScope, Registry};

    #[test]
    fn binance_candles_have_a_real_split_on_both_channels() {
        let profile = binance();
        let candles = profile
            .class(SymbolClass::Spot)
            .and_then(|class| class.kind(DataKind::Candles))
            .expect("binance spot candles");
        assert_eq!(candles.best_split(), SplitQuality::Real);
        assert!(
            candles.rest.is_some_and(|c| c.split == SplitQuality::Real),
            "the REST split is what column 9 buys; losing it must fail loudly"
        );
    }

    #[test]
    fn bybit_rest_candles_are_attributed_and_say_so() {
        let profile = bybit();
        let candles = profile
            .class(SymbolClass::Spot)
            .and_then(|class| class.kind(DataKind::Candles))
            .expect("bybit spot candles");
        let rest = candles.rest.expect("a REST channel");
        assert_eq!(rest.split, SplitQuality::Attributed);
        assert!(
            rest.note.is_some_and(|note| note.contains("not a measurement")),
            "the attribution caveat is the honesty mechanism; it must exist"
        );
        // And the live channel is better: trade-built candles.
        assert_eq!(candles.best_split(), SplitQuality::Real);
    }

    #[test]
    fn bybit_has_no_trade_history() {
        let profile = bybit();
        let trades = profile
            .class(SymbolClass::Spot)
            .and_then(|class| class.kind(DataKind::Trades))
            .expect("bybit spot trades");
        assert!(trades.is_supplied());
        assert!(!trades.has_history());
    }

    #[test]
    fn the_real_profiles_resolve_the_way_the_audit_found() {
        // Cross-check against the catalog: the answers the design document's
        // §13.2 matrix asserts, computed rather than restated.
        let registry = Registry::new(descriptor::STANDARD, standard());

        let binance_spot = DataScope::new(Provider::Binance, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry.resolve("footprint", &binance_spot);
        assert_eq!(resolution.availability, Availability::Available);
        assert_eq!(resolution.basis, Some(Basis::True));

        // Bybit trade analytics exist but are live-window analyses: the
        // caveat is the difference between "available" and "trustworthy for
        // last month".
        let bybit_spot = DataScope::new(Provider::Bybit, SymbolClass::Spot, "BTCUSDT");
        let resolution = registry.resolve("footprint", &bybit_spot);
        assert_eq!(resolution.availability, Availability::Available);
        assert!(
            resolution
                .caveats
                .iter()
                .any(|c| c.contains("live-window only")),
            "{:?}",
            resolution.caveats
        );

        // Positioning is not implemented anywhere.
        let resolution = registry.resolve("greeks_exposure", &binance_spot);
        assert_eq!(resolution.availability, Availability::Unavailable);

        // A spot-only venue asked about perpetuals refuses by name.
        let resolution = registry.resolve(
            "candles",
            &DataScope::new(Provider::Binance, SymbolClass::Linear, "BTCUSDT"),
        );
        assert_eq!(resolution.availability, Availability::Unavailable);
    }
}
