//! `/footprint` -- the trade-level order-flow ladder (`docs/14`).
//!
//! ## Why this is its own route rather than a `/candles` parameter
//!
//! A footprint is built from **trades**, not candles, and the difference is not
//! cosmetic. `analytics-core` refuses to build one from candles at all
//! (`build_footprint_from_candle` returns nothing), because a uniformly spread
//! candle reproduces its own aggregate ratio at every price level -- so a 3:1
//! candle looks like a stack of imbalances it never had. A route that could
//! silently fall back to that would produce a chart that is confidently wrong.
//!
//! So this route requires trades, and says so plainly when there are none.
//!
//! ## The response is per-candle ladders, not one profile
//!
//! The reference footprint chart shows a bid x ask ladder *per candle*, with the
//! diagonal imbalances highlighted. That means the response has to carry every
//! level of every candle, which is a lot of numbers -- so the window is capped
//! rather than the response being paginated. A chart shows what is on screen.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use analytics_core::types::{Side, Timeframe};
use analytics_core::{build_footprints, detect_imbalances_with, ImbalanceConfig};

use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::AppState;

/// Most candles one request may return.
///
/// Each carries a ladder of levels, so this is the number that decides the
/// response size. 400 candles of ~40 levels is about a megabyte of JSON --
/// enough for a screen and not enough to hurt.
const MAX_CANDLES: usize = 400;

/// Rows a ladder aims for over the window's own price range.
///
/// The reference footprint shows around this many, and it is what keeps a cell
/// tall enough to hold `0.44 x 2.75`.
const TARGET_ROWS: usize = 45;

/// Query for `GET /footprint`.
#[derive(Debug, Deserialize)]
pub struct FootprintQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m`.
    pub timeframe: String,
    /// Window start, unix **milliseconds**, inclusive.
    pub from: Option<i64>,
    /// Window end, unix **milliseconds**, exclusive.
    pub to: Option<i64>,
    /// Candles to return when no window is given.
    pub limit: Option<usize>,
    /// Price bucket, in quote currency.
    pub bucket_size: Option<f64>,
    /// Imbalance ratio: how many times the opposing volume the dominant side
    /// needs. `3.0` is the usual convention.
    pub ratio: Option<f64>,
}

/// Render unix nanoseconds as `YYYY-MM-DD HH:MM UTC`, for a message a human
/// reads. A raw nanosecond count in an error is not an instruction.
fn iso(nanos: i64) -> String {
    let seconds = nanos.div_euclid(1_000_000_000);
    chrono::DateTime::from_timestamp(seconds, 0).map_or_else(
        || nanos.to_string(),
        |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

/// One price level of one candle.
#[derive(Debug, Serialize)]
pub struct LevelResponse {
    /// The bucket's midpoint.
    pub price: f64,
    /// Volume executed at the bid -- the seller aggressed.
    pub bid: f64,
    /// Volume executed at the ask -- the buyer aggressed.
    pub ask: f64,
    /// `ask - bid`.
    pub delta: f64,
    /// Whether this level is part of a diagonal imbalance.
    pub imbalance: Option<ImbalanceResponse>,
}

/// An imbalance at one level.
#[derive(Debug, Serialize)]
pub struct ImbalanceResponse {
    /// `buy` or `sell`.
    pub side: &'static str,
    /// Dominant over opposing.
    pub ratio: f64,
    /// How many consecutive same-side imbalances this belongs to.
    pub stacked: usize,
}

/// One candle's ladder.
#[derive(Debug, Serialize)]
pub struct FootprintCandleResponse {
    /// Bucket open time, unix nanoseconds.
    pub open_time: i64,
    /// Open.
    pub open: f64,
    /// High.
    pub high: f64,
    /// Low.
    pub low: f64,
    /// Close.
    pub close: f64,
    /// Total volume across the candle.
    pub volume: f64,
    /// Total bid volume.
    pub bid_volume: f64,
    /// Total ask volume.
    pub ask_volume: f64,
    /// `ask - bid` for the whole candle.
    pub delta: f64,
    /// The price with the most volume, when the ladder is non-empty.
    pub poc: Option<f64>,
    /// How many levels the ladder holds.
    pub levels: usize,
    /// The ladder, ascending by price.
    pub cells: Vec<LevelResponse>,
}

/// The whole response.
#[derive(Debug, Serialize)]
pub struct FootprintResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// Bucket size used.
    pub bucket_size: f64,
    /// Imbalance ratio used.
    pub ratio: f64,
    /// Trades that fell inside the window.
    pub trades: usize,
    /// One entry per candle, ascending by time.
    pub candles: Vec<FootprintCandleResponse>,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// Query for `GET /footprint/coverage`.
#[derive(Debug, Deserialize)]
pub struct CoverageQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
}

/// What trade data exists, so a chart can pick the window that works.
#[derive(Debug, Serialize)]
pub struct CoverageResponse {
    /// Instrument.
    pub symbol: String,
    /// Oldest trade, unix **milliseconds** -- what `/footprint` accepts.
    pub from: i64,
    /// Newest trade, unix **milliseconds**.
    pub to: i64,
    /// How many trades are stored.
    pub trades: i64,
    /// How many whole minutes the span covers, for a caller sizing a window.
    pub minutes: i64,
}

/// `GET /footprint/coverage`
///
/// Exists because a footprint needs the user to choose a window and the trades
/// are backfilled in capped chunks, so the newest candles almost never have any.
/// Without this, "select Footprint" is a dead end until the user works out which
/// window to ask for; with it, the chart can just ask.
///
/// # Errors
/// 404 when no trades are stored for the symbol at all.
pub async fn coverage(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<CoverageQuery>,
) -> Result<Json<CoverageResponse>, ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured"))?;
    let symbol = query.symbol.to_uppercase();

    let Some(coverage) = db::repositories::trades_coverage(database.pool(), &symbol).await? else {
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_TICK_DATA",
            format!(
                "no trades are stored for {symbol}. Run `cargo run -p xtask -- backfill-trades \
                 --symbol {symbol} --from <start> --to <end>` to collect some (the window is \
                 capped at 24h)."
            ),
        ));
    };

    Ok(Json(CoverageResponse {
        symbol,
        from: coverage.first / 1_000_000,
        // Inclusive of the last trade's own millisecond, so a window ending here
        // contains it.
        to: coverage.last / 1_000_000 + 1,
        trades: coverage.count,
        minutes: (coverage.last - coverage.first) / 60_000_000_000,
    }))
}

/// `GET /footprint`
///
/// # Errors
/// 404 when the window holds no trades -- which is a different answer from "no
/// imbalances", and the client has to be able to tell them apart.
pub async fn footprint(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<FootprintQuery>,
) -> Result<Json<FootprintResponse>, ApiError> {
    let database = state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured"))?;

    let timeframe: Timeframe = query.timeframe.parse().map_err(|e| {
        ApiError::bad_request(
            "TIMEFRAME_INVALID",
            format!("unknown timeframe `{}`: {e}", query.timeframe),
        )
    })?;
    let symbol = query.symbol.to_uppercase();
    let ratio = query.ratio.filter(|r| *r > 1.0).unwrap_or(3.0);

    // Same window rule as `/candles`: with no explicit range, the newest `limit`
    // candles -- not "the last limit before now", which is empty on a symbol
    // that stopped updating.
    let (from_ns, to_ns, limit) = match (query.from, query.to) {
        (Some(from), Some(to)) => (
            from.saturating_mul(1_000_000),
            to.saturating_mul(1_000_000),
            None,
        ),
        _ => {
            let limit = query.limit.unwrap_or(120).clamp(1, MAX_CANDLES);
            let Some((_earliest, latest)) =
                db::repositories::candles_range(database.pool(), &symbol, timeframe).await?
            else {
                return Err(ApiError::not_found(format!(
                    "no {} candles stored for {symbol}",
                    query.timeframe
                )));
            };
            let width = timeframe.nanos();
            let to_ns = latest + width;
            let from_ns = to_ns - i64::try_from(limit).unwrap_or(i64::MAX) * width;
            (from_ns, to_ns, Some(limit))
        }
    };

    let candles =
        db::repositories::load_candles(database.pool(), &symbol, timeframe, from_ns, to_ns).await?;
    if candles.is_empty() {
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_MARKET_DATA",
            format!(
                "no {} candles stored for {symbol} in that window",
                query.timeframe
            ),
        ));
    }

    let trades = db::repositories::load_trades(database.pool(), &symbol, from_ns, to_ns).await?;

    if trades.is_empty() {
        // The honest answer, and a different one from "nothing happened". A
        // footprint built from candles would be a fabrication, which is why
        // `analytics-core` refuses to build one.
        //
        // The message names the window that *does* have trades, because a
        // footprint is the one chart where the user has to choose the window
        // deliberately -- trades are backfilled in capped chunks, so the newest
        // candles usually have none.
        let coverage = db::repositories::trades_coverage(database.pool(), &symbol)
            .await
            .ok()
            .flatten()
            .map(|coverage| {
                format!(
                    " {} trades are stored for this symbol, from {} to {} -- ask for a window \
                     inside that.",
                    coverage.count,
                    iso(coverage.first),
                    iso(coverage.last)
                )
            })
            .unwrap_or_else(|| {
                format!(
                    " No trades are stored for {symbol} at all yet; run `cargo run -p xtask -- \
                     backfill-trades --symbol {symbol} --from <start> --to <end>` (capped at 24h)."
                )
            });

        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_TICK_DATA",
            format!(
                "no trades are stored for {symbol} in this window. A footprint needs trade-level \
                 data and it cannot be derived from candles.{coverage}"
            ),
        ));
    }

    // A caller that does not name a bucket gets one sized to the window. That
    // matters more here than for a profile: a fixed $10 is coarse on a $2
    // instrument and produces 800 unreadable rows on a $10,000 one, and the
    // caller usually does not know the price before asking.
    let bucket_size = query
        .bucket_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or_else(|| {
            let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
            let high = candles.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
            analytics_core::volume_profile::round_bucket(high - low, TARGET_ROWS)
        });

    let mut footprints = build_footprints(&candles, &trades, bucket_size);
    if let Some(limit) = limit {
        if footprints.len() > limit {
            footprints.drain(..footprints.len() - limit);
        }
    }

    // Re-run the imbalance detection with the caller's ratio: `build_footprints`
    // uses the default, and a chart that highlights 3:1 while the user asked for
    // 4:1 would be highlighting the wrong cells.
    let config = ImbalanceConfig {
        ratio_threshold: ratio,
        diagonal: true,
        min_stack: 1,
    };

    let response = footprints
        .iter()
        .map(|footprint| {
            let events = detect_imbalances_with(footprint, &config);
            let cells = footprint
                .cells
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    let event = events.iter().find(|event| event.index == index);
                    LevelResponse {
                        price: cell.price_level,
                        bid: cell.bid_volume,
                        ask: cell.ask_volume,
                        delta: cell.delta,
                        imbalance: event.map(|event| ImbalanceResponse {
                            side: match event.side {
                                Side::Buy => "buy",
                                Side::Sell => "sell",
                            },
                            ratio: event.ratio,
                            stacked: event.stacked,
                        }),
                    }
                })
                .collect::<Vec<_>>();

            let bid_volume: f64 = footprint.cells.iter().map(|cell| cell.bid_volume).sum();
            let ask_volume: f64 = footprint.cells.iter().map(|cell| cell.ask_volume).sum();
            let poc = footprint
                .cells
                .iter()
                .max_by(|a, b| {
                    let left = a.bid_volume + a.ask_volume;
                    let right = b.bid_volume + b.ask_volume;
                    left.partial_cmp(&right)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|cell| cell.price_level);

            FootprintCandleResponse {
                open_time: footprint.candle.open_time,
                open: footprint.candle.open,
                high: footprint.candle.high,
                low: footprint.candle.low,
                close: footprint.candle.close,
                volume: footprint.candle.volume,
                bid_volume,
                ask_volume,
                delta: ask_volume - bid_volume,
                poc,
                levels: cells.len(),
                cells,
            }
        })
        .collect::<Vec<_>>();

    let total_levels: usize = response.iter().map(|c| c.levels).sum();

    Ok(Json(FootprintResponse {
        symbol,
        timeframe: query.timeframe,
        bucket_size,
        ratio,
        trades: trades.len(),
        candles: response,
        note: if total_levels == 0 {
            Some("trades fell inside these candles but produced no price levels".into())
        } else {
            None
        },
    }))
}
