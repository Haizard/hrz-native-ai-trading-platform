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

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use analytics_core::types::Candle;
use analytics_core::Timeframe;
use db::repositories::{candles_range, load_candles};

use crate::error::ApiError;
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
    Query(query): Query<CandlesQuery>,
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
