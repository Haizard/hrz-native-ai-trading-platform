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

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use analytics_core::types::Candle;
use analytics_core::Timeframe;
use db::repositories::{candles_range, load_candles};

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

/// Response of `GET /candles`.
#[derive(Debug, Serialize)]
pub struct CandlesResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Candles, oldest first. `open_time` is unix nanoseconds.
    pub candles: Vec<Candle>,
}

/// Most candles a single request may return.
const MAX_LIMIT: usize = 5000;

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

    let db = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured"))?;

    let (from_ns, to_ns) = resolve_window(db, &query, timeframe).await?;
    let candles = load_candles(db.pool(), &query.symbol, timeframe, from_ns, to_ns)
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading candles failed: {e}"),
            )
        })?;

    Ok(Json(CandlesResponse {
        symbol: query.symbol,
        timeframe: timeframe.to_string(),
        candles,
    }))
}

/// Work out the window in nanoseconds.
///
/// With no `from`/`to`, the window is the **most recent** `limit` candles
/// ending at the newest candle stored -- not "the last limit candles before
/// now", which would return nothing at all on a symbol that stopped updating
/// an hour ago.
async fn resolve_window(
    db: &db::Database,
    query: &CandlesQuery,
    timeframe: Timeframe,
) -> Result<(i64, i64), ApiError> {
    if let (Some(from_ms), Some(to_ms)) = (query.from, query.to) {
        return Ok((ms_to_ns(from_ms), ms_to_ns(to_ms)));
    }

    let Some((_earliest, latest)) = candles_range(db.pool(), &query.symbol, timeframe)
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading candle range failed: {e}"),
            )
        })?
    else {
        return Err(ApiError::not_found(format!(
            "no {} candles stored for {}",
            timeframe, query.symbol
        )));
    };

    let limit = query.limit.unwrap_or(500).clamp(1, MAX_LIMIT);
    let width = timeframe.nanos();
    let to_ns = latest + width; // the latest candle's own bucket is half-open
    let from_ns = to_ns - i64::try_from(limit).unwrap_or(i64::MAX) * width;
    Ok((from_ns, to_ns))
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

/// `GET /symbols`
///
/// # Errors
/// 503 without a database.
pub async fn symbols(State(state): State<AppState>) -> Result<Json<Vec<SymbolResponse>>, ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured"))?;

    let symbols = db::market::list_symbols(database.pool()).await?;
    Ok(Json(
        symbols
            .into_iter()
            .map(|coverage| SymbolResponse {
                coverage_note: coverage.coverage_note(),
                symbol: coverage.symbol,
                timeframes: coverage
                    .timeframes
                    .into_iter()
                    .map(|tf| TimeframeCoverageResponse {
                        expected: tf.expected_candles(),
                        missing: tf.missing_candles(),
                        timeframe: tf.timeframe,
                        candles: tf.candles,
                        first: tf.first,
                        last: tf.last,
                    })
                    .collect(),
            })
            .collect(),
    ))
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
/// This is the **REST** view: the newest snapshot that was stored. The live
/// ladder is `/ws/orderbook/{symbol}`, which does not exist yet -- so on a
/// deployment with no tick collection this answers 404 and says so, rather than
/// returning an empty book that looks like a market with no liquidity.
///
/// # Errors
/// 404 when the symbol has no stored snapshots, 503 without a database.
pub async fn orderbook(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<OrderBookQuery>,
) -> Result<Json<OrderBookResponse>, ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured"))?;
    let symbol = query.symbol.to_uppercase();

    let Some(snapshot) = db::market::latest_orderbook(database.pool(), &symbol).await? else {
        // Say which of the two it is. "No data" and "no data *for this symbol*"
        // need different fixes, and an empty book would hide both.
        let any = db::market::orderbook_snapshot_count(database.pool(), &symbol).await?;
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_ORDERBOOK_DATA",
            if any == 0 {
                format!(
                    "no order-book snapshots are stored for {symbol}. Depth is collected from \
                     the trade stream, so a deployment that has not run the collector has none."
                )
            } else {
                format!("no order-book snapshot could be read for {symbol}")
            },
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
