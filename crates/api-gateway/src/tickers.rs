//! `GET /tickers` -- the venue's 24-hour statistics, one answer per watchlist.
//!
//! A watchlist row is three numbers -- last price, 24h change, 24h volume --
//! and it wants them for *many* symbols at once, refreshed every couple of
//! seconds. `GET /candles` answers about one symbol and rebuilds a window per
//! call, which is the wrong cost shape for twenty rows on a one-second clock.
//! So this route turns one venue request (`/api/v3/ticker/24hr`) into every
//! watchlist's answer, behind a short cache.
//!
//! ## Why the cache is 5 seconds and not 1
//!
//! The rows update at 1s on the venue itself; a proxy that re-fetched at 1s
//! would put the venue's own polling load on this process times the number of
//! connected users, to save four seconds of staleness nobody can act on. Five
//! seconds is the compromise `docs/12`'s rate-limit tables already assume for
//! venue-backed reads, and the cache is per-process -- the first user to ask
//! pays the round trip, everyone else rides along.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::AppState;
use crate::error::ApiError;
use crate::extract::ApiQuery;

/// How long one venue answer is reused before the next request re-fetches.
///
/// The cache also carries this as a field ([`TickerCache::ttl`]); the field
/// exists so a test can set it to *zero* and drive the expiry path
/// deterministically instead of sleeping real seconds. Production builds it
/// through [`TickerCache::new`], which fixes it at this value.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// The venue call gets this long before the route falls back to stale data.
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Most rows a single answer carries when the caller did not name symbols.
///
/// Browsing "everything the venue lists" uncapped would ship a multi-megabyte
/// JSON body on every cache expiry; a watchlist that wants more asks for its
/// symbols explicitly, and an explicit list is never capped by this.
const DEFAULT_LIMIT: usize = 50;

/// The venue's own `/ticker/24hr` accepts at most this many symbols per call
/// (`symbols` parameter). Beyond it the route fetches everything and filters
/// locally rather than stitching venue calls together.
const MAX_FILTERED_FETCH: usize = 100;

/// One symbol's watchlist numbers.
#[derive(Debug, Clone, Serialize)]
pub struct TickerResponse {
    /// The symbol as the venue spells it, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Last traded price.
    pub last_price: f64,
    /// 24-hour change, percent.
    pub price_change_percent: f64,
    /// 24-hour high.
    pub high_price: f64,
    /// 24-hour low.
    pub low_price: f64,
    /// 24-hour traded volume, in the quote asset.
    pub quote_volume: f64,
}

/// Query parameters of `GET /tickers`.
#[derive(Debug, Deserialize)]
pub struct TickersQuery {
    /// Comma-separated symbols to answer about, e.g. `BTCUSDT,ETHUSDT`.
    /// Absent or empty means "the venue's most active markets".
    pub symbols: Option<String>,
    /// Most rows to return when no symbol list was given.
    pub limit: Option<usize>,
}

/// One cached venue answer.
struct Entry {
    at: Instant,
    tickers: HashMap<String, TickerResponse>,
}

/// The route's cache, shared by every handler call through [`AppState`].
///
/// A [`tokio::sync::Mutex`] rather than a std one because a miss fetches the
/// venue *while holding the lock*: two thousand watchlists arriving together
/// produce one venue request instead of two thousand. The cost is that
/// requests during a fetch wait for it -- which is exactly the ride-along the
/// cache exists for, and the fetch itself is bounded by [`FETCH_TIMEOUT`].
#[derive(Default)]
pub struct TickerCache {
    /// How long [`Entry`] stays fresh. A field rather than the constant so a
    /// test can collapse it to zero; see [`TickerCache::new`].
    ttl: Duration,
    entry: Mutex<Option<Entry>>,
}

impl TickerCache {
    /// An empty cache with the production TTL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ttl: CACHE_TTL,
            entry: Mutex::new(None),
        }
    }

    /// A cache pre-loaded with `tickers`, still fresh for `ttl`.
    ///
    /// A test that wants the *serve* paths does not want the network: this
    /// hands it rows with a chosen freshness, so the fresh answer, the stale
    /// fallback and the expiry re-fetch are all reachable without a venue.
    /// The same constructor is a warm-start seam: a deployment can seed its
    /// watchlist from a snapshot at boot and answer the first requests without
    /// waiting for the venue.
    #[must_use]
    pub fn seeded(tickers: Vec<TickerResponse>, ttl: Duration) -> Self {
        let mut map = HashMap::with_capacity(tickers.len());
        for t in tickers {
            map.insert(t.symbol.clone(), t);
        }
        Self {
            ttl,
            entry: Mutex::new(Some(Entry {
                at: Instant::now(),
                tickers: map,
            })),
        }
    }
}

/// The whole answer, so a client can tell a fresh row from a carried one.
#[derive(Debug, Serialize)]
pub struct TickersResponse {
    /// The rows, most-active first when the caller did not name symbols.
    pub tickers: Vec<TickerResponse>,
    /// Whether the answer outlived the cache TTL because the venue could not
    /// be reached for a fresh one. Absent when fresh -- a client that has
    /// never been lied to should not have to check.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

/// `GET /tickers`
///
/// Public: market data is not user data (`docs/12`'s explicit decision).
pub async fn tickers(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<TickersQuery>,
) -> Result<Json<TickersResponse>, ApiError> {
    let asked: Vec<String> = query
        .symbols
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .collect();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 1_000);

    let mut cache = state.tickers.entry.lock().await;
    let now = Instant::now();

    if let Some(entry) = cache.as_ref() {
        if now.duration_since(entry.at) < state.tickers.ttl {
            return Ok(Json(serve(entry, &asked, limit, false)));
        }
    }

    // Miss (or expired): the fetch holds the lock, so concurrent watchlists
    // ride along on this one round trip rather than each opening their own.
    match fetch_tickers(&state.binance_base_url, &asked).await {
        Ok(tickers) => {
            let entry = Entry { at: now, tickers };
            let response = serve(&entry, &asked, limit, false);
            *cache = Some(entry);
            Ok(Json(response))
        }
        Err(error) => {
            // A cache with *any* age beats an error for a watchlist: prices a
            // few seconds old are still prices, while a 503 blanks every row
            // at once. Name the staleness instead of hiding it.
            if let Some(entry) = cache.as_ref() {
                tracing::warn!(%error, "serving stale tickers");
                return Ok(Json(serve(entry, &asked, limit, true)));
            }
            Err(ApiError::unavailable(format!(
                "could not reach the venue for ticker statistics: {error}"
            )))
        }
    }
}

/// Reduce a cached entry to the rows this request asked for.
///
/// A named list keeps the caller's order (row 1 of a watchlist should not
/// reshuffle because the venue sorted its payload differently); an unnamed one
/// is "what is most active", so it sorts by 24h quote volume and caps at
/// `limit`.
fn serve(entry: &Entry, asked: &[String], limit: usize, stale: bool) -> TickersResponse {
    let tickers = if asked.is_empty() {
        let mut rows: Vec<TickerResponse> = entry.tickers.values().cloned().collect();
        rows.sort_by(|a, b| b.quote_volume.total_cmp(&a.quote_volume));
        rows.truncate(limit);
        rows
    } else {
        asked
            .iter()
            .filter_map(|s| entry.tickers.get(s).cloned())
            .collect()
    };
    TickersResponse { tickers, stale }
}

/// One venue call, already reduced to [`TickerResponse`]s.
///
/// With at most [`MAX_FILTERED_FETCH`] symbols named, the venue filters for us
/// in one small request; with more (or none), the whole book is fetched and
/// filtered here. Parsing is defensive per field because the venue adds fields
/// more often than it removes them, and a watchlist should not 500 because a
/// new column arrived.
async fn fetch_tickers(
    base_url: &str,
    asked: &[String],
) -> Result<HashMap<String, TickerResponse>, String> {
    let filter = !asked.is_empty() && asked.len() <= MAX_FILTERED_FETCH;
    let mut request = reqwest::Client::new()
        .get(format!("{base_url}/api/v3/ticker/24hr"))
        .timeout(FETCH_TIMEOUT);
    if filter {
        // The venue's `symbols` parameter is a JSON array, verbatim.
        request = request.query(&[(
            "symbols",
            serde_json::to_string(asked).unwrap_or_else(|_| "[]".to_string()),
        )]);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("venue returned an error: {e}"))?;

    // Read the body as text and parse it ourselves rather than letting
    // reqwest decode straight into the value. When the venue answers something
    // that is not JSON -- an HTML blocklist page, a proxy error, a rate-limit
    // prose body -- `decode failed: error decoding response body` says nothing
    // about *what* came back, and the incident is undiagnosable from the log.
    // The first bytes of the body name the culprit immediately.
    let body = response
        .text()
        .await
        .map_err(|e| format!("reading the response body failed: {e}"))?;
    let response: Value = serde_json::from_str(&body).map_err(|e| {
        let snippet: String = body.chars().take(160).collect();
        format!("decode failed: {e}; body starts with: {snippet:?}")
    })?;

    // Unfiltered answers (and multi-symbol ones) are arrays; a filtered call
    // that named exactly one symbol comes back as one object.
    let rows: Vec<Value> = match response {
        Value::Array(rows) => rows,
        row @ Value::Object(_) => vec![row],
        other => return Err(format!("unexpected payload shape: {other}")),
    };

    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let Some(symbol) = row.get("symbol").and_then(Value::as_str) else {
            continue;
        };
        let number = |field: &str| {
            row.get(field)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        out.insert(
            symbol.to_string(),
            TickerResponse {
                symbol: symbol.to_string(),
                last_price: number("lastPrice"),
                price_change_percent: number("priceChangePercent"),
                high_price: number("highPrice"),
                low_price: number("lowPrice"),
                quote_volume: number("quoteVolume"),
            },
        );
    }
    if out.is_empty() {
        return Err("the venue answered with no tickers".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use serde_json::json;
    use std::sync::Arc;

    /// One BTC row with a known quote volume, so ordering tests can tell it
    /// apart from ETH's.
    fn btc(volume: f64) -> TickerResponse {
        TickerResponse {
            symbol: "BTCUSDT".into(),
            last_price: 60_000.0,
            price_change_percent: 1.5,
            high_price: 61_000.0,
            low_price: 59_000.0,
            quote_volume: volume,
        }
    }

    fn eth(volume: f64) -> TickerResponse {
        TickerResponse {
            symbol: "ETHUSDT".into(),
            last_price: 3_000.0,
            price_change_percent: -1.25,
            high_price: 3_100.0,
            low_price: 2_950.0,
            quote_volume: volume,
        }
    }

    /// An `AppState` good for nothing except this route, which is the point:
    /// every venue-reaching field points at a port that refuses instantly, so
    /// a test that unexpectedly fetches fails fast instead of passing.
    fn state_with(base_url: &str, cache: TickerCache) -> AppState {
        let supervisor = Arc::new(crate::bots::BotSupervisor::new(crate::bots::FeedMode::Off));
        AppState {
            db: None,
            agent: None,
            skills: Arc::new(ai_agent::SkillLibrary::new()),
            auth: None,
            bots: Arc::clone(&supervisor),
            backfill: market_data::BackfillClient::new("http://127.0.0.1:1"),
            windows: market_data::WindowService::new(
                supervisor.history(),
                supervisor.live(),
                market_data::BackfillClient::new("http://127.0.0.1:1"),
            ),
            symbols: market_data::SymbolIndex::new(),
            agent_limits: Arc::new(crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimit::default(),
            )),
            metrics: Arc::new(observability::metrics::Registry::new()),
            alert_queue: None,
            binance_base_url: base_url.to_string(),
            tickers: Arc::new(cache),
            vault: None,
            sandbox: Arc::new(sandbox::Sandbox::new().expect("the embedded guest module compiles")),
        }
    }

    /// A stub venue serving `payload` from the real path the route calls.
    async fn spawn_venue(payload: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/api/v3/ticker/24hr",
            get(move || {
                let payload = payload.clone();
                async move { axum::Json(payload) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fresh_rows_are_served_without_touching_the_venue() {
        // Port 1 refuses instantly, so any fetch would error and the route
        // would answer from the cache as stale. Fresh + non-stale proves the
        // cache answered.
        let state = state_with(
            "http://127.0.0.1:1",
            TickerCache::seeded(vec![btc(1.0)], Duration::from_secs(3600)),
        );
        let response = tickers(
            State(state),
            ApiQuery(TickersQuery {
                symbols: Some("BTCUSDT".into()),
                limit: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0.tickers[0].last_price, 60_000.0);
        assert!(!response.0.stale);
    }

    /// A venue that answers `200 OK` with a body that is not JSON -- an HTML
    /// blocklist page, a proxy's prose error. This is the shape behind the
    /// undiagnosable `decode failed: error decoding response body` in the
    /// logs: the status was fine, the *content* was not.
    async fn spawn_html_venue() -> String {
        let app = axum::Router::new().route(
            "/api/v3/ticker/24hr",
            get(|| async { axum::response::Html("<html><body>Request blocked by WAF</body></html>") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_non_json_body_is_named_in_the_error() {
        let venue = spawn_html_venue().await;
        // No cache at all: with one, the route would correctly fall back to
        // stale rows and the error would never surface; here the error itself
        // is the answer, and it must quote the body rather than just say
        // "decode failed".
        let state = state_with(&venue, TickerCache::default());
        let error = tickers(
            State(state),
            ApiQuery(TickersQuery {
                symbols: None,
                limit: None,
            }),
        )
        .await
        .unwrap_err();
        let message = error.message().to_string();
        assert!(
            message.contains("decode failed"),
            "the error must still name the failure mode: {message}"
        );
        assert!(
            message.contains("Request blocked"),
            "the body snippet must be in the error so the log is diagnosable: {message}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_venue_serves_stale_rows_and_says_so() {
        let state = state_with(
            "http://127.0.0.1:1",
            TickerCache::seeded(vec![btc(1.0)], Duration::ZERO),
        );
        let response = tickers(
            State(state),
            ApiQuery(TickersQuery {
                symbols: Some("BTCUSDT".into()),
                limit: None,
            }),
        )
        .await
        .unwrap();
        // Old prices with a flag beat a blank watchlist: the row is still a
        // price, and the client can grey it out.
        assert!(response.0.stale);
        assert_eq!(response.0.tickers[0].symbol, "BTCUSDT");
    }

    #[tokio::test]
    async fn an_expired_cache_is_replaced_by_what_the_venue_answers() {
        let venue = spawn_venue(json!({
            "symbol": "ETHUSDT",
            "lastPrice": "3000.5",
            "priceChangePercent": "-1.25",
            "highPrice": "3100",
            "lowPrice": "2950",
            "quoteVolume": "800000"
        }))
        .await;
        let state = state_with(&venue, TickerCache::seeded(vec![btc(1.0)], Duration::ZERO));
        let response = tickers(
            State(state),
            ApiQuery(TickersQuery {
                symbols: Some("BTCUSDT,ETHUSDT".into()),
                limit: None,
            }),
        )
        .await
        .unwrap();
        // The venue's answer replaces the cache wholesale: a symbol it did not
        // name is gone rather than served from the expired entry, which is the
        // difference between a cache and a memory.
        assert!(!response.0.stale);
        let served: Vec<&str> = response
            .0
            .tickers
            .iter()
            .map(|t| t.symbol.as_str())
            .collect();
        assert_eq!(served, vec!["ETHUSDT"]);
        assert_eq!(response.0.tickers[0].last_price, 3000.5);
    }

    #[tokio::test]
    async fn an_unnamed_request_ranks_by_24h_activity() {
        let state = state_with(
            "http://127.0.0.1:1",
            TickerCache::seeded(
                vec![btc(1_000_000.0), eth(5_000_000.0)],
                Duration::from_secs(3600),
            ),
        );
        let response = tickers(
            State(state),
            ApiQuery(TickersQuery {
                symbols: None,
                limit: None,
            }),
        )
        .await
        .unwrap();
        let served: Vec<&str> = response
            .0
            .tickers
            .iter()
            .map(|t| t.symbol.as_str())
            .collect();
        assert_eq!(served, vec!["ETHUSDT", "BTCUSDT"]);
    }
}
