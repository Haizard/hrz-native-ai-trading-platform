//! What instruments the venue actually offers.
//!
//! ## Why this is not a constant
//!
//! `MARKET_SYMBOLS` is a deployment's *watchlist* -- what to keep warm -- and it
//! was being used as the limit of what the platform would admit exists. That is
//! a different question with a different answer, and the wrong answer was
//! visible: a chart that could perfectly well draw `ETHUSDT` from the venue
//! would refuse the symbol because nobody had listed it, and the error a user
//! got named a variable they have never heard of.
//!
//! So the platform asks the venue. `GET /api/v3/exchangeInfo` returns every
//! tradable instrument with its status and its base/quote assets, which is the
//! authoritative answer to "can this be charted" -- and it is one request, made
//! once, not per symbol.
//!
//! ## Why it is cached and why it may go stale
//!
//! `exchangeInfo` is a large payload (`~2 MB` for the full spot listing) and the
//! venue rate-limits it at weight 20. Fetching it per request would be the
//! single most expensive thing the platform does. It changes when a listing
//! changes -- a handful of times a week -- so it is held in RAM and refreshed on
//! a clock.
//!
//! The failure mode of a cache is staleness, so it is *reported* rather than
//! hidden: [`SymbolIndex::fetched_at`] is on every validation answer, and a
//! validation that fell back to the static watchlist says so. A symbol that is
//! missing from a stale index is a symbol the platform will not chart, and the
//! difference between "the venue does not offer it" and "our copy is old" is
//! exactly the kind of thing that should never be guessed at.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::error::MarketDataError;

/// How long a fetched index is trusted before it is refreshed.
///
/// Six hours. Long enough that the cost is negligible -- four requests a day
/// against a weight-20 endpoint -- and short enough that a listing added this
/// morning is chartable by the afternoon without a redeploy. There is no
/// correctness requirement that makes it shorter: a symbol added minutes ago is
/// not something a user is waiting on, while a platform that hammered this
/// endpoint would get itself rate-limited for the data that *is* latency
/// critical.
pub const INDEX_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// One tradable instrument, as the venue describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    /// The symbol as the venue spells it, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Base asset, e.g. `BTC`.
    pub base: String,
    /// Quote asset, e.g. `USDT`.
    pub quote: String,
    /// Whether the venue is currently accepting orders for it.
    ///
    /// Kept because "the venue lists it" and "the venue will trade it" are
    /// different, and a chart of a halted instrument is a chart of a market
    /// that has stopped. `BREAK`/`HALT` instruments are indexed but marked.
    pub trading: bool,
}

/// The venue's `exchangeInfo` payload, reduced to what the platform needs.
///
/// Deliberately **not** the whole payload: deserializing every filter and
/// permission of every instrument would be ~2 MB of strongly-typed data the
/// platform never reads. Only three fields per symbol are taken, and the rest is
/// dropped by serde rather than held.
#[derive(Debug, Deserialize)]
struct ExchangeInfo {
    symbols: Vec<RawSymbol>,
}

#[derive(Debug, Deserialize)]
struct RawSymbol {
    symbol: String,
    /// `baseAsset`. Renamed explicitly: serde's default would look for
    /// `base_asset`, find nothing, and take the `default` -- so every
    /// instrument would index with an **empty** base and quote and a search for
    /// `btc` would match nothing. Silent, because an empty string is a valid
    /// `String` and nothing in the type says otherwise.
    #[serde(rename = "baseAsset", default)]
    base_asset: String,
    /// `quoteAsset`, for the same reason.
    #[serde(rename = "quoteAsset", default)]
    quote_asset: String,
    /// `TRADING`, `BREAK`, `HALT`, ... Defaulted rather than required: the field
    /// has been present for years but a payload without it should degrade to
    /// "listed", not fail to parse and leave the platform with no index at all.
    #[serde(default = "default_status")]
    status: String,
}

fn default_status() -> String {
    "TRADING".to_string()
}

/// Every instrument the venue offers, held in RAM.
///
/// Cheap to clone: an `Arc` and a lock.
#[derive(Debug, Clone)]
pub struct SymbolIndex {
    inner: Arc<RwLock<Cached>>,
}

#[derive(Debug, Default)]
struct Cached {
    /// The instruments, keyed by the uppercase symbol.
    instruments: HashMap<String, Instrument>,
    /// When the payload was fetched, unix nanos. `None` when nothing has been
    /// fetched yet.
    fetched_at: Option<i64>,
}

impl SymbolIndex {
    /// An empty index.
    ///
    /// Empty rather than seeded from `MARKET_SYMBOLS`, because a seeded index
    /// would report a *validation* it never performed. [`Self::validate`] is
    /// explicit about knowing nothing when it knows nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Cached::default())),
        }
    }

    /// How many instruments are indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .map_or(0, |cached| cached.instruments.len())
    }

    /// Whether the index holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// When the index was last fetched, unix nanos.
    #[must_use]
    pub fn fetched_at(&self) -> Option<i64> {
        self.inner.read().ok().and_then(|cached| cached.fetched_at)
    }

    /// Whether the index has no data, or its data is older than [`INDEX_TTL`].
    ///
    /// The question a validation answer has to be able to address. "We do not
    /// offer that symbol" is only actionable if the caller can tell whether it
    /// came from a current listing or from nothing at all.
    #[must_use]
    pub fn is_stale(&self, now: i64) -> bool {
        let Ok(cached) = self.inner.read() else {
            return true;
        };
        match cached.fetched_at {
            None => true,
            Some(at) => now.saturating_sub(at) > i64::try_from(INDEX_TTL.as_nanos()).unwrap_or(i64::MAX),
        }
    }

    /// Every indexed instrument, sorted by symbol.
    #[must_use]
    pub fn all(&self) -> Vec<Instrument> {
        let Ok(cached) = self.inner.read() else {
            return Vec::new();
        };
        let mut out: Vec<Instrument> = cached.instruments.values().cloned().collect();
        out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        out
    }

    /// Substring search over symbol, base and quote.
    ///
    /// Case-insensitive and matches **anywhere**, not just as a prefix: a user
    /// looking for a chain's pairs types `sol`, and the instrument is `SOLUSDT`
    /// -- a prefix search finds it, but a user typing `usdt` to see dollar pairs
    /// would not, and that is a reasonable thing to want. Ranked so exact and
    /// prefix matches come first: a search that returned `WBTCUSDT` before
    /// `BTCUSDT` would be technically correct and useless.
    ///
    /// `limit` bounds the answer. An empty query returns the first `limit`
    /// instruments, which is what a dropdown shows before anything is typed.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<Instrument> {
        let query = query.trim().to_ascii_uppercase();
        let Ok(cached) = self.inner.read() else {
            return Vec::new();
        };

        if query.is_empty() {
            let mut out: Vec<Instrument> = cached.instruments.values().cloned().collect();
            out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            out.truncate(limit);
            return out;
        }

        let mut hits: Vec<(u8, &Instrument)> = cached
            .instruments
            .values()
            .filter_map(|instrument| {
                rank(instrument, &query).map(|rank| (rank, instrument))
            })
            .collect();

        // By rank, then alphabetically, so the answer is stable between calls.
        // An unstable order would make a dropdown's entries move as a user
        // types, which reads as the list being broken.
        hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.symbol.cmp(&b.1.symbol)));
        hits.into_iter()
            .take(limit)
            .map(|(_, instrument)| instrument.clone())
            .collect()
    }

    /// Whether the venue offers `symbol`, and how confident the answer is.
    #[must_use]
    pub fn lookup(&self, symbol: &str) -> SymbolCheck {
        let symbol = symbol.trim().to_ascii_uppercase();
        let Ok(cached) = self.inner.read() else {
            return SymbolCheck::UnknownIndex;
        };
        if cached.fetched_at.is_none() {
            return SymbolCheck::UnknownIndex;
        }

        match cached.instruments.get(&symbol) {
            Some(instrument) if instrument.trading => SymbolCheck::Tradable,
            // Listed but halted. Distinct from unknown, and distinct from
            // tradable: charting it works, trading it does not, and a platform
            // that conflated the two would either refuse a chartable symbol or
            // let a bot trade a halted market.
            Some(_) => SymbolCheck::NotTrading,
            None => SymbolCheck::Unknown,
        }
    }

    /// Replace the index with a freshly fetched payload.
    ///
    /// Returns how many instruments were indexed.
    ///
    /// A payload with **zero** symbols is refused rather than installed. The
    /// venue has never returned an empty listing, so an empty one means a
    /// proxy or a captive portal answered instead -- and installing it would
    /// turn a transient network problem into "no instrument the platform offers
    /// exists", which is a far worse failure than serving a stale index.
    pub fn install(&self, body: &str, now: i64) -> Result<usize, MarketDataError> {
        let parsed: ExchangeInfo = serde_json::from_str(body)
            .map_err(|e| MarketDataError::Normalization(format!("exchangeInfo decode: {e}")))?;

        if parsed.symbols.is_empty() {
            return Err(MarketDataError::Normalization(
                "exchangeInfo returned no symbols; refusing to install an empty index".into(),
            ));
        }

        let mut instruments = HashMap::with_capacity(parsed.symbols.len());
        for raw in parsed.symbols {
            let symbol = raw.symbol.to_ascii_uppercase();
            instruments.insert(
                symbol.clone(),
                Instrument {
                    symbol,
                    base: raw.base_asset.to_ascii_uppercase(),
                    quote: raw.quote_asset.to_ascii_uppercase(),
                    trading: raw.status.eq_ignore_ascii_case("TRADING"),
                },
            );
        }
        let count = instruments.len();

        let Ok(mut cached) = self.inner.write() else {
            return Err(MarketDataError::Normalization(
                "the symbol index lock was poisoned".into(),
            ));
        };
        cached.instruments = instruments;
        cached.fetched_at = Some(now);
        Ok(count)
    }

    /// Fetch the venue's listing and install it.
    ///
    /// # Errors
    /// Transport failures, malformed payloads, and an empty listing -- see
    /// [`Self::install`]. A caller that gets an error still has whatever the
    /// index held before, which is the point of caching it.
    pub async fn refresh(&self, client: &crate::backfill::BackfillClient) -> Result<usize, MarketDataError> {
        let body = client.exchange_info().await?;
        let count = self.install(&body, now_ns())?;
        info!(count, "symbol index refreshed");
        Ok(count)
    }

    /// Refresh only if the index is stale, ignoring a failure.
    ///
    /// What the gateway calls before answering. A refresh that fails leaves the
    /// previous index in place and logs, because a user asking for a chart
    /// should not be told the platform cannot look up symbols when the platform
    /// can look up symbols perfectly well from the copy it already has.
    pub async fn refresh_if_stale(&self, client: &crate::backfill::BackfillClient) {
        if !self.is_stale(now_ns()) {
            return;
        }
        match self.refresh(client).await {
            Ok(count) => debug!(count, "symbol index brought up to date"),
            Err(e) => warn!(
                error = %e,
                "could not refresh the symbol index; serving the copy already held"
            ),
        }
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// The three answers a symbol check can give.
///
/// Three rather than two because the platform's responses differ: `Unknown` is
/// a bad request, `NotTrading` is a chart that works and a bot that must not
/// start, and `UnknownIndex` is the platform's own problem and must not be
/// reported as the user's mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolCheck {
    /// The venue lists it and is accepting orders.
    Tradable,
    /// The venue lists it but is not currently trading it.
    NotTrading,
    /// The venue does not list it.
    Unknown,
    /// The index has never been fetched, so nothing can be said.
    UnknownIndex,
}

impl SymbolCheck {
    /// Whether the symbol may be charted.
    ///
    /// Both halts and unknowns are chartable: the venue still serves their
    /// history, and refusing would mean a symbol that stopped trading yesterday
    /// could not be *looked at* today, which is precisely when someone wants to
    /// look at it. Only a symbol the venue does not list at all is refused.
    #[must_use]
    pub const fn chartable(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Whether a bot may trade it.
    #[must_use]
    pub const fn tradable(self) -> bool {
        matches!(self, Self::Tradable)
    }
}

/// How well an instrument matches a query, lower being better.
///
/// `None` when it does not match at all.
fn rank(instrument: &Instrument, query: &str) -> Option<u8> {
    let symbol = instrument.symbol.as_str();
    if symbol == query {
        return Some(0);
    }
    if symbol.starts_with(query) {
        return Some(1);
    }
    // A base-asset match outranks a quote match, because a user typing `btc`
    // wants BTC pairs rather than every pair quoted in a token that happens to
    // contain those letters.
    if instrument.base == query {
        return Some(2);
    }
    if instrument.base.starts_with(query) {
        return Some(3);
    }
    if instrument.quote.starts_with(query) {
        return Some(4);
    }
    if symbol.contains(query) {
        return Some(5);
    }
    None
}

/// Wall-clock now, in unix nanoseconds.
#[must_use]
pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload shaped like the venue's, reduced to the fields that are read.
    fn payload(pairs: &[(&str, &str, &str, &str)]) -> String {
        let symbols: Vec<serde_json::Value> = pairs
            .iter()
            .map(|(symbol, base, quote, status)| {
                serde_json::json!({
                    "symbol": symbol,
                    "baseAsset": base,
                    "quoteAsset": quote,
                    "status": status,
                    // Present in the real payload and deliberately not read: if
                    // this made parsing fail, the index would be empty in
                    // production and every symbol would look unknown.
                    "filters": [{"filterType": "PRICE_FILTER", "tickSize": "0.01000000"}],
                    "permissions": ["SPOT"],
                })
            })
            .collect();
        serde_json::json!({ "symbols": symbols }).to_string()
    }

    fn indexed() -> SymbolIndex {
        let index = SymbolIndex::new();
        index
            .install(
                &payload(&[
                    ("BTCUSDT", "BTC", "USDT", "TRADING"),
                    ("WBTCUSDT", "WBTC", "USDT", "TRADING"),
                    ("ETHUSDT", "ETH", "USDT", "TRADING"),
                    ("SOLBTC", "SOL", "BTC", "TRADING"),
                    ("HALTUSDT", "HALT", "USDT", "BREAK"),
                ]),
                1_000,
            )
            .expect("installs");
        index
    }

    #[test]
    fn an_index_nobody_has_fetched_admits_it_knows_nothing() {
        // The distinction the whole module is arranged around. An empty index
        // saying "unknown symbol" would turn a cold start into a platform that
        // denies every instrument exists -- and the user would be told their
        // symbol was wrong.
        let index = SymbolIndex::new();
        assert_eq!(index.lookup("BTCUSDT"), SymbolCheck::UnknownIndex);
        assert!(index.is_stale(0));
        assert_eq!(index.fetched_at(), None);

        // And it is still *chartable*, because saying no on no evidence is the
        // failure this exists to prevent.
        assert!(index.lookup("BTCUSDT").chartable());
        assert!(!index.lookup("BTCUSDT").tradable());
    }

    #[test]
    fn a_listed_instrument_is_tradable_and_a_halted_one_is_not() {
        let index = indexed();
        assert_eq!(index.lookup("BTCUSDT"), SymbolCheck::Tradable);
        assert_eq!(index.lookup("HALTUSDT"), SymbolCheck::NotTrading);

        // A halted instrument is still chartable: its history is what someone
        // wants to look at, precisely because it stopped.
        assert!(index.lookup("HALTUSDT").chartable());
        assert!(
            !index.lookup("HALTUSDT").tradable(),
            "a bot must not start on a market the venue has stopped"
        );
    }

    #[test]
    fn symbols_are_matched_case_insensitively() {
        // The chart sends whatever the user typed. A lookup that was
        // case-sensitive would report a lowercase symbol as unknown, which is
        // the same class of bug as the original missing-watchlist entry.
        let index = indexed();
        assert_eq!(index.lookup("btcusdt"), SymbolCheck::Tradable);
        assert_eq!(index.lookup("  BtcUsdt  "), SymbolCheck::Tradable);
    }

    #[test]
    fn a_symbol_the_venue_does_not_list_is_unknown_and_not_chartable() {
        let index = indexed();
        assert_eq!(index.lookup("NOTREALUSDT"), SymbolCheck::Unknown);
        assert!(!index.lookup("NOTREALUSDT").chartable());
    }

    #[test]
    fn search_ranks_an_exact_match_first() {
        // The failure a naive `contains` filter produces: searching `btc`
        // returns `WBTCUSDT` before `BTCUSDT`, which is technically a match and
        // useless in a dropdown.
        let index = indexed();
        let hits = index.search("btc", 10);
        let symbols: Vec<&str> = hits.iter().map(|i| i.symbol.as_str()).collect();
        assert_eq!(
            symbols.first(),
            Some(&"BTCUSDT"),
            "an exact symbol match must lead: {symbols:?}"
        );
        assert!(symbols.contains(&"WBTCUSDT"), "the wrapped token still matches");
        assert!(
            symbols.contains(&"SOLBTC"),
            "a quote-asset match still counts: {symbols:?}"
        );
    }

    #[test]
    fn search_matches_anywhere_not_only_at_the_start() {
        // A user typing `usdt` to see dollar pairs. A prefix-only search finds
        // nothing, and the answer to "which symbols can I chart" would then be
        // wrong for the most common query there is.
        let index = indexed();
        let hits = index.search("usdt", 10);
        let symbols: Vec<&str> = hits.iter().map(|i| i.symbol.as_str()).collect();
        assert_eq!(symbols.len(), 4, "every USDT pair: {symbols:?}");
        assert!(!symbols.contains(&"SOLBTC"), "SOLBTC is not a USDT pair");
    }

    #[test]
    fn search_is_case_insensitive_and_bounded() {
        let index = indexed();
        assert_eq!(index.search("BTC", 1).len(), 1);
        assert_eq!(index.search("btc", 2).len(), 2);
        // An empty query lists from the top rather than returning nothing --
        // which is what a dropdown shows before a user types.
        assert_eq!(index.search("", 2).len(), 2);
        assert_eq!(index.search("   ", 2).len(), 2);
    }

    #[test]
    fn search_order_is_stable_for_the_same_query() {
        // A dropdown whose entries move between keystrokes reads as broken.
        let index = indexed();
        let first = index.search("usdt", 10);
        let second = index.search("usdt", 10);
        assert_eq!(
            first.iter().map(|i| &i.symbol).collect::<Vec<_>>(),
            second.iter().map(|i| &i.symbol).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_empty_listing_is_refused_rather_than_installed() {
        // A captive portal or a proxy answering 200 with `{"symbols":[]}`.
        // Installing it would turn a transient network problem into "no
        // instrument this platform offers exists".
        let index = indexed();
        let err = index
            .install(r#"{"symbols":[]}"#, 2_000)
            .expect_err("an empty listing must not install");
        assert!(err.to_string().contains("no symbols"), "{err}");

        // And the previous data must survive the refusal.
        assert_eq!(index.lookup("BTCUSDT"), SymbolCheck::Tradable);
        assert_eq!(index.fetched_at(), Some(1_000), "the timestamp must not move");
    }

    #[test]
    fn a_malformed_payload_is_refused_rather_than_installed() {
        let index = indexed();
        assert!(index.install("not json at all", 2_000).is_err());
        assert_eq!(index.lookup("BTCUSDT"), SymbolCheck::Tradable);
    }

    #[test]
    fn freshness_is_a_clock_not_a_belief() {
        let index = indexed();
        let fetched = index.fetched_at().expect("fetched");
        let ttl = i64::try_from(INDEX_TTL.as_nanos()).expect("fits");

        assert!(!index.is_stale(fetched));
        assert!(!index.is_stale(fetched + ttl), "exactly at the TTL is not yet stale");
        assert!(index.is_stale(fetched + ttl + 1));
    }

    #[test]
    fn a_payload_without_a_status_field_is_treated_as_listed() {
        // The field has been present for years, but a payload missing it should
        // degrade to "listed" rather than fail to parse and leave the platform
        // with no index at all.
        let index = SymbolIndex::new();
        let body = r#"{"symbols":[{"symbol":"BTCUSDT","baseAsset":"BTC","quoteAsset":"USDT"}]}"#;
        assert_eq!(index.install(body, 1).expect("installs"), 1);
        assert_eq!(index.lookup("BTCUSDT"), SymbolCheck::Tradable);
    }

    #[test]
    fn every_indexed_instrument_carries_its_base_and_quote() {
        let index = indexed();
        let all = index.all();
        let btc = all
            .iter()
            .find(|i| i.symbol == "BTCUSDT")
            .expect("BTCUSDT is indexed");
        assert_eq!(btc.base, "BTC");
        assert_eq!(btc.quote, "USDT");
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn the_wire_field_names_are_the_venues_own_camel_case_ones() {
        // This test exists because the first version of `RawSymbol` declared
        // `base_asset` and serde looked for a field of that name, found none,
        // and fell back to `default` -- so every instrument indexed with an
        // **empty** base and quote, `search("btc")` matched nothing, and nothing
        // anywhere was an error. A rename in a serde struct is not a compile
        // error and not a runtime error, which is why it is pinned here.
        //
        // The payload is written with the venue's exact spelling, not through
        // the `payload()` helper, so a helper that agreed with the struct
        // instead of with Binance could not hide the bug.
        let index = SymbolIndex::new();
        let body = r#"{"symbols":[{"symbol":"ETHBTC","baseAsset":"ETH","quoteAsset":"BTC","status":"TRADING"}]}"#;
        index.install(body, 1).expect("installs");

        let eth = index.all().into_iter().next().expect("one instrument");
        assert_eq!(eth.base, "ETH", "baseAsset must be read from the venue's key");
        assert_eq!(eth.quote, "BTC", "quoteAsset must be read from the venue's key");
        // And the end-to-end consequence: search by asset now works.
        assert_eq!(index.search("eth", 10).len(), 1);
        assert_eq!(index.search("btc", 10).len(), 1, "matched on the quote asset");
    }
}
