//! `/candles` (`docs/12`) -- what the chart draws.
//!
//! ## Time is milliseconds on the wire
//!
//! Everything internal is unix **nanoseconds**, but `from`/`to` come in as
//! milliseconds because that is what `Date.now()` produces in a browser and
//! what every charting library expects. The conversion happens here, at the
//! edge, and nowhere else -- the same rule as the database boundary.
//!
//! `open_time` in the response stays in nanoseconds because it is the
//! `Candle` type's own field, and reformatting it here would mean the API and
//! the engine disagree about what a timestamp is.
//!
//! ## Nothing here is read from the database
//!
//! Market data is **not persisted**. The database is a free tier with 6 GB for
//! every symbol of every market this platform will carry, and one symbol's
//! trades are about 110 MB a day, so five symbols would fill it in under a
//! fortnight. Instead:
//!
//! * **recent** bars come from [`market_data::HistoryRegistry`], a bounded
//!   buffer in RAM fed by the live trade stream;
//! * **older** bars are fetched from the venue's REST API on demand and are
//!   not kept afterwards.
//!
//! So this route answers by merging two sources, and it says which part of the
//! window each one covered -- see [`CandlesResponse::source`]. A cold process
//! answers entirely from the venue, which is slower but correct; a warm one
//! answers the recent part with no network call at all.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::debug;

use analytics_core::types::Candle;
use analytics_core::Timeframe;
use ::market_data::BackfillClient;

use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::AppState;

/// Query parameters for `GET /candles`.
#[derive(Debug, Deserialize)]
pub struct CandlesQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m`.
    pub timeframe: String,
    /// Window start, unix **milliseconds**, inclusive.
    pub from: Option<i64>,
    /// Window end, unix **milliseconds**, exclusive.
    pub to: Option<i64>,
    /// How many candles to return when no window is given.
    pub limit: Option<usize>,
}

/// Where the bars in a response came from.
///
/// A single count cannot say whether the window was cheap or expensive, and
/// "the chart took four seconds to open" is a question about this, not about
/// the candles. `memory` bars cost nothing; `venue` bars cost one REST round
/// trip per thousand, and if a window is slow this is the number that explains
/// it.
#[derive(Debug, Serialize)]
pub struct CandleSourceResponse {
    /// Bars served from the in-memory buffer.
    pub memory: usize,
    /// Bars fetched from the venue because the buffer did not reach back far
    /// enough.
    pub venue: usize,
}

/// Response of `GET /candles`.
#[derive(Debug, Serialize)]
pub struct CandlesResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Candles, oldest first. `open_time` is unix nanoseconds.
    pub candles: Vec<Candle>,
    /// Where the bars came from.
    pub source: CandleSourceResponse,
}

/// Most candles a single request may return.
const MAX_LIMIT: usize = 5000;

/// Most bars one request will fetch from the venue.
///
/// The buffer is meant to cover the recent part of a chart, not to be the whole
/// history. Without a ceiling, a client asking for five years of `1m` asks this
/// process to page through ~180 venue requests and hold the result -- and one
/// such request per open chart is how a free-tier deployment gets throttled.
const MAX_VENUE_FETCH: usize = 5000;

/// `GET /candles`
pub async fn candles(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<CandlesQuery>,
) -> Result<Json<CandlesResponse>, ApiError> {
    let timeframe: Timeframe = query.timeframe.parse().map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            format!("unknown timeframe `{}`", query.timeframe),
        )
    })?;
    let symbol = query.symbol.to_uppercase();

    // A chart is entitled to a live feed even before any bot is running, and
    // without this the buffer never fills and every request pays for the venue.
    state.bots.ensure_feed_for(&symbol);

    let limit = query.limit.unwrap_or(500).clamp(1, MAX_LIMIT);
    let history = state.bots.history();

    let (from_ns, to_ns) = match (query.from, query.to) {
        (Some(from_ms), Some(to_ms)) => (ms_to_ns(from_ms), ms_to_ns(to_ms)),
        _ => window_ending_at_newest(&history, &symbol, timeframe, limit),
    };

    let (candles, source) = candles_in(&state, &symbol, timeframe, from_ns, to_ns).await;

    Ok(Json(CandlesResponse {
        symbol,
        timeframe: timeframe.to_string(),
        candles,
        source,
    }))
}

/// The window for "the most recent `limit` bars".
///
/// Measured from the newest bar this process knows about, not from `now` -- the
/// latter returns nothing at all on a symbol that stopped updating an hour ago,
/// which is indistinguishable from a symbol nobody has asked for yet.
pub(crate) fn window_ending_at_newest(
    history: &market_data::HistoryRegistry,
    symbol: &str,
    timeframe: Timeframe,
    limit: usize,
) -> (i64, i64) {
    let width = timeframe.nanos();
    let span = i64::try_from(limit).unwrap_or(i64::MAX) * width;

    match history.newest(symbol, timeframe) {
        Some(newest) => (newest - span, newest + width),
        None => {
            let now = crate::now_ns();
            (now - span, now)
        }
    }
}

/// Bars for `[from_ns, to_ns)`, from RAM first and the venue for the rest.
///
/// Shared by `/candles` and `/footprint` so the two charts cannot disagree
/// about what a candle is: one source of bars, two renderings of them.
pub(crate) async fn candles_in(
    state: &AppState,
    symbol: &str,
    timeframe: Timeframe,
    from_ns: i64,
    to_ns: i64,
) -> (Vec<Candle>, CandleSourceResponse) {
    let mut candles = state.bots.history().series(symbol, timeframe).range(from_ns, to_ns);
    let from_memory = candles.len();

    // Whatever the buffer could not cover, fetch from the venue. Deliberately
    // only the gap: re-fetching the whole window would throw away the one thing
    // the buffer buys, which is not paying for bars already in hand.
    let oldest_we_have = candles.first().map_or(to_ns, |c| c.open_time);
    let mut venue = Vec::new();
    if from_ns < oldest_we_have {
        let gap_end = oldest_we_have.min(to_ns);
        let wanted = ((gap_end - from_ns) / timeframe.nanos().max(1)) as usize;
        if wanted > MAX_VENUE_FETCH {
            debug!(
                symbol, %timeframe, wanted, MAX_VENUE_FETCH,
                "the window reaches further back than one request will fetch; \
                 serving the newest part of it"
            );
            // Clamp from the front, not the back: the newest end of a window is
            // the end the chart is scrolled to.
            let width = i64::try_from(MAX_VENUE_FETCH).unwrap_or(i64::MAX) * timeframe.nanos();
            venue = fetch_from_venue(&state.backfill, symbol, timeframe, gap_end - width, gap_end)
                .await;
        } else {
            venue = fetch_from_venue(&state.backfill, symbol, timeframe, from_ns, gap_end).await;
        }
    }
    let from_venue = venue.len();

    if !venue.is_empty() {
        candles.extend(venue);
        // Both sources are sorted, and the buffer's copy of a bucket they share
        // is the one built from this process's own trade stream -- so sort and
        // keep the first of each pair.
        candles.sort_by_key(|c| c.open_time);
        candles.dedup_by_key(|c| c.open_time);
    }

    (
        candles,
        CandleSourceResponse {
            memory: from_memory,
            venue: from_venue,
        },
    )
}

/// Bars for `[from_ns, to_ns)` straight from the exchange.
///
/// Returns empty rather than erroring when the venue refuses: a chart that can
/// draw the recent part of its window is better than one that draws nothing
/// because a deeper page failed, and the caller can see from
/// [`CandlesResponse::source`] that it is incomplete.
async fn fetch_from_venue(
    client: &BackfillClient,
    symbol: &str,
    timeframe: Timeframe,
    from_ns: i64,
    to_ns: i64,
) -> Vec<Candle> {
    if from_ns >= to_ns {
        return Vec::new();
    }

    match client
        .backfill_candles(
            symbol,
            timeframe,
            from_ns,
            to_ns,
            market_data::BackfillSource::Klines,
        )
        .await
    {
        Ok(candles) => candles,
        Err(e) => {
            tracing::warn!(
                symbol, %timeframe, error = %e,
                "the venue would not serve history for this window"
            );
            Vec::new()
        }
    }
}

/// Milliseconds to nanoseconds, saturating rather than wrapping.
fn ms_to_ns(ms: i64) -> i64 {
    ms.saturating_mul(1_000_000)
}

// ---------------------------------------------------------------------------
// GET /symbols -- what is loaded, and over what window
// ---------------------------------------------------------------------------

/// One resolution's coverage, as a client sees it.
#[derive(Debug, Serialize)]
pub struct TimeframeCoverageResponse {
    /// The resolution, as stored.
    pub timeframe: String,
    /// How many candles are stored.
    pub candles: i64,
    /// The oldest candle's open time, unix nanoseconds.
    pub first: i64,
    /// The newest candle's open time, unix nanoseconds.
    pub last: i64,
    /// How many candles a complete series over that span would hold, when the
    /// resolution is one this platform knows.
    pub expected: Option<i64>,
    /// How many are missing from the span.
    ///
    /// This is the number that matters, and the reason the route exists. A
    /// count alone is reassuring and says nothing: this project has already
    /// lost real time to a table holding six months of 5m and two days of 1m,
    /// where a strategy's coarse timeframe silently resampled to twelve candles
    /// and a six-month backtest reported zero trades.
    pub missing: Option<i64>,
}

/// Everything loaded for one symbol.
#[derive(Debug, Serialize)]
pub struct SymbolResponse {
    /// The instrument.
    pub symbol: String,
    /// One entry per resolution, ordered by resolution.
    pub timeframes: Vec<TimeframeCoverageResponse>,
    /// A warning when one resolution covers much less history than another.
    ///
    /// Absent when the spans are comparable. Present, it says which resolution
    /// is thin and by how much -- because that is the fact that turns into a
    /// backtest reporting zero trades, and it is invisible from a candle count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_note: Option<String>,
}

/// The instruments this deployment is willing to chart.
///
/// Read once from `MARKET_SYMBOLS` (comma-separated). It exists because
/// `GET /symbols` cannot be answered from the buffer alone any more: the buffer
/// is empty on a cold start, and a page whose instrument list is empty cannot
/// ask for anything -- so it can never fill the buffer. The watchlist breaks
/// that deadlock by naming what the platform *can* chart, which is anything the
/// venue serves, since history is fetched on demand.
///
/// Defaults to `BTCUSDT` so a deployment with no configuration is a working
/// chart rather than an empty one.
pub fn watchlist() -> &'static [String] {
    static WATCHLIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

    WATCHLIST.get_or_init(|| {
        let raw = std::env::var("MARKET_SYMBOLS").unwrap_or_default();
        let mut out: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        if out.is_empty() {
            out.push("BTCUSDT".to_string());
        }
        out
    })
}

/// `GET /symbols`
///
/// Reports the **watchlist**, with what the buffer already holds per symbol --
/// not the other way round. Market data is not persisted, so the buffer is
/// empty on a cold start, and a route that listed only what it had buffered
/// would answer `[]` on every fresh boot: the page would have no instrument to
/// ask for, and the buffer would never fill.
///
/// A symbol with nothing buffered still reports the standard ladder, at zero
/// bars with `coverage_note` saying so. Asking for it is answered from the
/// venue, so "zero buffered" is not "unavailable" -- and a client that cannot
/// tell those apart would grey out a chart it can perfectly well draw.
pub async fn symbols(State(state): State<AppState>) -> Result<Json<Vec<SymbolResponse>>, ApiError> {
    let history = state.bots.history();

    let mut names: Vec<String> = watchlist().to_vec();
    for buffered in history.symbols() {
        if !names.contains(&buffered) {
            names.push(buffered);
        }
    }

    let mut out = Vec::new();
    for symbol in names {
        let mut timeframes = Vec::new();

        for timeframe in market_data::STANDARD_TIMEFRAMES {
            let series = history.series(&symbol, timeframe);
            let Some((first, last)) = series.earliest().zip(series.newest()) else {
                // Not buffered. Still offered: `/candles` fetches it.
                timeframes.push(TimeframeCoverageResponse {
                    timeframe: timeframe.to_string(),
                    candles: 0,
                    first: 0,
                    last: 0,
                    expected: None,
                    missing: None,
                });
                continue;
            };
            let width = timeframe.nanos().max(1);
            let expected = (last - first) / width + 1;
            let missing = expected - series.len() as i64;
            timeframes.push(TimeframeCoverageResponse {
                timeframe: timeframe.to_string(),
                candles: series.len() as i64,
                first,
                last,
                expected: Some(expected),
                missing: Some(missing.max(0)),
            });
        }

        // Whether anything is buffered is a question about **closed** bars, not
        // about whether the series exists. The recorder calls `record_forming`
        // the moment the feed starts, which registers all six resolutions while
        // every one of them still holds nothing -- so asking "are any series
        // registered?" answers `true` on a cold start and the note below never
        // appeared on a live boot at all, only in tests that seeded nothing.
        let closed: i64 = timeframes.iter().map(|frame| frame.candles).sum();

        out.push(SymbolResponse {
            coverage_note: (closed == 0).then(|| {
                format!(
                    "nothing is buffered for {symbol} yet. It is still chartable: asking for it \
                     starts its feed and fetches history from the venue."
                )
            }),
            symbol,
            timeframes,
        });
    }

    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// GET /orderbook -- the newest stored snapshot
// ---------------------------------------------------------------------------

/// Query parameters for `GET /orderbook`.
#[derive(Debug, Deserialize)]
pub struct OrderBookQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
}

/// One side of the book.
#[derive(Debug, Serialize)]
pub struct LevelResponse {
    /// Price of this level.
    pub price: f64,
    /// Resting quantity.
    pub quantity: f64,
}

/// The newest snapshot, as a client sees it.
#[derive(Debug, Serialize)]
pub struct OrderBookResponse {
    /// The instrument.
    pub symbol: String,
    /// Snapshot time, unix nanoseconds.
    pub timestamp: i64,
    /// Bids, best first.
    pub bids: Vec<LevelResponse>,
    /// Asks, best first.
    pub asks: Vec<LevelResponse>,
    /// Best ask minus best bid, when the snapshot has both sides.
    ///
    /// `null` rather than `0.0` for a one-sided book: zero reads as a perfectly
    /// tight market, which is the opposite of what a missing side means.
    pub spread: Option<f64>,
}

/// `GET /orderbook`
///
/// This is the **REST** view of the live book: the newest snapshot the feed has
/// published, held in memory. It is not read from Postgres, which no longer
/// holds market data at all -- so this answers with the book as it is *now*
/// rather than the last snapshot a pump happened to write. The streaming ladder
/// is `/ws/orderbook/{symbol}`.
///
/// # Errors
/// 404 when no book has arrived for the symbol yet.
pub async fn orderbook(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<OrderBookQuery>,
) -> Result<Json<OrderBookResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    state.bots.ensure_feed_for(&symbol);

    let Some(snapshot) = state.bots.live().book(&symbol) else {
        // An empty book would read as "no liquidity", which is a different and
        // much more alarming claim than "no book has arrived yet".
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_ORDERBOOK_DATA",
            format!(
                "no order-book snapshot has arrived for {symbol}. Depth comes from the live \
                 feed: asking for a symbol starts its feed, and the first snapshot lands a \
                 second or two later -- so retry once before concluding anything."
            ),
        ));
    };

    let spread = db::market::best_bid_ask(&snapshot).map(|(bid, ask)| ask.price - bid.price);

    Ok(Json(OrderBookResponse {
        symbol: snapshot.symbol,
        timestamp: snapshot.timestamp,
        bids: snapshot
            .bids
            .into_iter()
            .map(|level| LevelResponse {
                price: level.price,
                quantity: level.quantity,
            })
            .collect(),
        asks: snapshot
            .asks
            .into_iter()
            .map(|level| LevelResponse {
                price: level.price,
                quantity: level.quantity,
            })
            .collect(),
        spread,
    }))
}
