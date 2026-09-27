//! Order-flow intelligence routes (`docs/22`): bar delta statistics, delta by
//! order size, profile with memory, iceberg detection and VPIN.
//!
//! All five answer from the **same** tape and candle window service the
//! footprint and the agent tools read, so a chart and a thesis can never
//! disagree about what the flow did. Trade-derived stats say "unavailable"
//! when the window holds no trades -- a fabricated zero would read as data.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use analytics_core::types::{Side, Timeframe};
use analytics_core::{
    bar_delta_stats_window, build_profile_memory, calculate_cvd_by_size,
    calculate_vpin_series, delta_by_size_per_candle, detect_icebergs, SizeClass,
    SizeClassConfig, VpinConfig,
};

use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::AppState;

/// The shared window resolution for every route here: symbol + timeframe +
/// limit, parsed once by [`parse_window`].
#[derive(Debug, Deserialize)]
pub struct WindowQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m`.
    pub timeframe: String,
    /// Candles to cover, oldest kept. Ignored when `from`/`to` are given.
    pub limit: Option<usize>,
    /// Window start, unix **milliseconds**, inclusive -- the footprint route's
    /// convention, so a caller can align two routes on one window.
    pub from: Option<i64>,
    /// Window end, unix **milliseconds**, exclusive.
    pub to: Option<i64>,
}

/// Most candles any route here covers. The bar-stats response carries one
/// small object per candle, so 1,000 is ~a hundred kilobytes; the size and
/// memory routes carry per-class / per-level detail and sit at 400.
pub const MAX_CANDLES: usize = 1_000;

/// The parsed shape of [`WindowQuery`].
pub(crate) struct ParsedWindow {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub limit: usize,
    /// Explicit window in nanoseconds, when the caller gave `from`/`to` (ms).
    pub from_to: Option<(i64, i64)>,
}

/// Parse and normalize the shared window query.
fn parse_window(query: &WindowQuery) -> Result<ParsedWindow, ApiError> {
    let timeframe: Timeframe = query.timeframe.parse().map_err(|e| {
        ApiError::bad_request(
            "TIMEFRAME_INVALID",
            format!("unknown timeframe `{}`: {e}", query.timeframe),
        )
    })?;
    Ok(ParsedWindow {
        symbol: query.symbol.to_uppercase(),
        timeframe,
        limit: query.limit.unwrap_or(120).clamp(1, MAX_CANDLES),
        from_to: match (query.from, query.to) {
            (Some(from), Some(to)) => Some((from.saturating_mul(1_000_000), to.saturating_mul(1_000_000))),
            _ => None,
        },
    })
}

/// Load candles for the trailing window and the tape's trades inside it.
///
/// Mirrors the footprint route's source of truth: candles through the shared
/// window service, trades from the live tape. Returns the candle window and
/// the trades, plus whether trades existed at all -- the honest-unavailable
/// signal.
async fn load_window(
    state: &AppState,
    window: &ParsedWindow,
) -> Result<(Vec<analytics_core::Candle>, Vec<analytics_core::Trade>, bool), ApiError> {
    state.bots.ensure_feed_for(&window.symbol);
    let (candles, _) = match window.from_to {
        Some((from_ns, to_ns)) => {
            crate::market_routes::candles_in(state, &window.symbol, window.timeframe, from_ns, to_ns)
                .await
        }
        None => {
            let (from_ns, to_ns) = crate::market_routes::window_ending_at_newest(
                &state.bots.history(),
                &window.symbol,
                window.timeframe,
                window.limit,
            );
            crate::market_routes::candles_in(state, &window.symbol, window.timeframe, from_ns, to_ns)
                .await
        }
    };
    if candles.is_empty() {
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_MARKET_DATA",
            format!(
                "no {} candles are available for {} in that window",
                window.timeframe, window.symbol
            ),
        ));
    }
    let tape = state.bots.live().tape(&window.symbol);
    let (from_ns, to_ns) = window.from_to.unwrap_or((
        candles.first().map_or(0, |c| c.open_time),
        candles.last().map_or(i64::MAX, |c| c.open_time + c.timeframe.nanos()),
    ));
    let trades = tape.range(from_ns, to_ns);
    let has_trades = !trades.is_empty();
    Ok((candles, trades, has_trades))
}

const NO_TICK_NOTE: &str =
    "no trades on the tape for this window: per-trade statistics are unavailable \
     (candles alone cannot produce them). Recent windows only -- trades are not stored.";

// ---------------------------------------------------------------------------
// GET /bar-delta-stats
// ---------------------------------------------------------------------------

/// One candle's intra-bar statistics.
#[derive(Debug, Serialize)]
pub struct BarDeltaStatsResponse {
    /// Candle open time, unix nanoseconds.
    pub open_time: i64,
    /// Closing delta (`buy - sell`).
    pub delta: f64,
    /// Highest running delta reached inside the bar.
    pub max_delta: f64,
    /// Lowest running delta reached inside the bar.
    pub min_delta: f64,
    /// Where the close sits between the extremes, `0..1`, when measurable.
    pub delta_close_position: Option<f64>,
    /// Volume-weighted average trade price inside the bar.
    pub intrabar_vwap: Option<f64>,
    /// Trades in the bar.
    pub trades: usize,
}

/// The `GET /bar-delta-stats` response.
#[derive(Debug, Serialize)]
pub struct BarDeltaWindowResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// One entry per candle, ascending by time.
    pub candles: Vec<BarDeltaStatsResponse>,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// `GET /bar-delta-stats` -- max/min intra-bar delta and intrabar VWAP per
/// candle (`docs/22` phase 2).
///
/// # Errors
/// 404 when the window holds no candles.
pub async fn bar_delta_stats(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<WindowQuery>,
) -> Result<Json<BarDeltaWindowResponse>, ApiError> {
    let window = parse_window(&query)?;
    let (candles, trades, has_trades) = load_window(&state, &window).await?;

    let stats = bar_delta_stats_window(&candles, &trades);
    let candles = stats
        .iter()
        .map(|stat| BarDeltaStatsResponse {
            open_time: stat.open_time,
            delta: stat.delta,
            max_delta: stat.max_delta,
            min_delta: stat.min_delta,
            delta_close_position: stat.delta_close_position(),
            intrabar_vwap: if stat.is_empty() {
                None
            } else {
                Some(stat.intrabar_vwap)
            },
            trades: stat.trades,
        })
        .collect();

    Ok(Json(BarDeltaWindowResponse {
        symbol: window.symbol,
        timeframe: query.timeframe,
        candles,
        note: (!has_trades).then(|| NO_TICK_NOTE.into()),
    }))
}

// ---------------------------------------------------------------------------
// GET /delta-by-size
// ---------------------------------------------------------------------------

/// Query for `GET /delta-by-size`.
#[derive(Debug, Deserialize)]
pub struct SizeQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m`.
    pub timeframe: String,
    /// Candles to cover, oldest kept.
    pub limit: Option<usize>,
    /// Small-class notional ceiling, in quote currency.
    pub small_below: Option<f64>,
    /// Medium-class notional ceiling, in quote currency.
    pub medium_below: Option<f64>,
    /// Reset per-class CVD at each UTC day (the default).
    #[serde(default = "default_true")]
    pub reset_at_session: bool,
    /// Window start, unix milliseconds, inclusive.
    pub from: Option<i64>,
    /// Window end, unix milliseconds, exclusive.
    pub to: Option<i64>,
}

/// `serde(default = ..)` helper: sessions reset by default.
fn default_true() -> bool {
    true
}

/// Per-class numbers at one point.
#[derive(Debug, Serialize)]
pub struct ClassPoint {
    /// Class name: `small`, `medium` or `large`.
    pub class: &'static str,
    /// Cumulative delta (buy - sell) up to this point, when a series point.
    pub cvd: Option<f64>,
    /// This candle's buy volume in the class.
    pub buy: f64,
    /// This candle's sell volume in the class.
    pub sell: f64,
    /// This candle's delta in the class.
    pub delta: f64,
    /// Trades in the class within the candle.
    pub trades: usize,
}

/// One candle's size-classed breakdown.
#[derive(Debug, Serialize)]
pub struct SizeCandleResponse {
    /// Candle open time, unix nanoseconds.
    pub open_time: i64,
    /// One entry per class, small to large.
    pub classes: Vec<ClassPoint>,
}

/// The `GET /delta-by-size` response.
#[derive(Debug, Serialize)]
pub struct SizeResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// The thresholds actually used, in quote currency.
    pub small_below: f64,
    /// The medium threshold actually used.
    pub medium_below: f64,
    /// One entry per candle, ascending by time.
    pub candles: Vec<SizeCandleResponse>,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// `GET /delta-by-size` -- per-candle delta and per-class CVD by order size
/// (`docs/22` phase 1). The "CVD by order size" feature: are big players
/// buying or selling, and are small players doing the opposite?
///
/// # Errors
/// 404 when the window holds no candles.
pub async fn delta_by_size(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<SizeQuery>,
) -> Result<Json<SizeResponse>, ApiError> {
    let window = parse_window(&WindowQuery {
        symbol: query.symbol.clone(),
        timeframe: query.timeframe.clone(),
        limit: query.limit,
        from: query.from,
        to: query.to,
    })?;
    let config = SizeClassConfig {
        small_below: query.small_below.filter(|v| *v > 0.0).unwrap_or(SizeClassConfig::default().small_below),
        medium_below: query.medium_below.filter(|v| *v > 0.0).unwrap_or(SizeClassConfig::default().medium_below),
    };
    let (candles, trades, has_trades) = load_window(&state, &window).await?;

    let per_candle = delta_by_size_per_candle(&candles, &trades, &config);
    let cvd = calculate_cvd_by_size(&candles, &trades, &config, query.reset_at_session);

    let response = candles
        .iter()
        .zip(&per_candle)
        .zip(&cvd)
        .map(|((candle, breakdown), point)| SizeCandleResponse {
            open_time: candle.open_time,
            classes: SizeClass::all()
                .iter()
                .map(|class| {
                    let delta = breakdown.class(*class);
                    ClassPoint {
                        class: class.as_str(),
                        cvd: Some(point.classes[class.index()]),
                        buy: delta.buy,
                        sell: delta.sell,
                        delta: delta.delta(),
                        trades: delta.trades,
                    }
                })
                .collect(),
        })
        .collect();

    Ok(Json(SizeResponse {
        symbol: window.symbol,
        timeframe: query.timeframe,
        small_below: config.small_below,
        medium_below: config.medium_below,
        candles: response,
        note: (!has_trades).then(|| NO_TICK_NOTE.into()),
    }))
}

// ---------------------------------------------------------------------------
// GET /profile-memory
// ---------------------------------------------------------------------------

/// Query for `GET /profile-memory`.
#[derive(Debug, Deserialize)]
pub struct MemoryQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m`.
    pub timeframe: String,
    /// Candles to cover, oldest kept.
    pub limit: Option<usize>,
    /// Price bucket, in quote currency. Auto-sized when absent.
    pub bucket_size: Option<f64>,
    /// Window start, unix milliseconds, inclusive.
    pub from: Option<i64>,
    /// Window end, unix milliseconds, exclusive.
    pub to: Option<i64>,
}

/// One level's session history.
#[derive(Debug, Serialize)]
pub struct LevelMemoryResponse {
    /// Bucket midpoint price.
    pub price_level: f64,
    /// Session volume at this level.
    pub volume: f64,
    /// Session delta at this level (`buy - sell`).
    pub delta: f64,
    /// Whether the level's cumulative delta flipped sign over the session.
    pub flipped_control: bool,
    /// Per-candle delta history, oldest first: `(open_time_ns, delta)`.
    pub visits: Vec<(i64, f64)>,
}

/// One session's profile with memory.
#[derive(Debug, Serialize)]
pub struct SessionMemoryResponse {
    /// Session start (UTC day), unix nanoseconds.
    pub session_open_time: i64,
    /// Bucket size used.
    pub bucket_size: f64,
    /// Total session volume.
    pub total_volume: f64,
    /// Levels ascending by price.
    pub levels: Vec<LevelMemoryResponse>,
}

/// The `GET /profile-memory` response.
#[derive(Debug, Serialize)]
pub struct MemoryResponse {
    /// Instrument.
    pub symbol: String,
    /// Resolution.
    pub timeframe: String,
    /// One entry per session, oldest first.
    pub sessions: Vec<SessionMemoryResponse>,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// `GET /profile-memory` -- the volume profile with memory: per-level delta
/// history over each UTC-day session, with control-flip flags (`docs/22`
/// phase 3).
///
/// # Errors
/// 404 when the window holds no candles.
pub async fn profile_memory(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<MemoryQuery>,
) -> Result<Json<MemoryResponse>, ApiError> {
    let window = parse_window(&WindowQuery {
        symbol: query.symbol.clone(),
        timeframe: query.timeframe.clone(),
        limit: query.limit,
        from: query.from,
        to: query.to,
    })?;
    let (candles, trades, has_trades) = load_window(&state, &window).await?;

    let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let high = candles.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
    let bucket_size = query
        .bucket_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or_else(|| analytics_core::volume_profile::round_bucket(high - low, 60));

    let sessions = build_profile_memory(&candles, &trades, bucket_size);
    let sessions = sessions
        .into_iter()
        .map(|session| SessionMemoryResponse {
            session_open_time: session.session_open_time,
            bucket_size: session.bucket_size,
            total_volume: session.total_volume,
            levels: session
                .levels
                .iter()
                .map(|level| LevelMemoryResponse {
                    price_level: level.price_level,
                    volume: level.volume,
                    delta: level.delta(),
                    flipped_control: level.flipped_control(),
                    visits: level
                        .visits
                        .iter()
                        .map(|visit| (visit.open_time, visit.delta))
                        .collect(),
                })
                .collect(),
        })
        .collect();

    Ok(Json(MemoryResponse {
        symbol: window.symbol,
        timeframe: query.timeframe,
        sessions,
        note: (!has_trades).then(|| NO_TICK_NOTE.into()),
    }))
}

// ---------------------------------------------------------------------------
// GET /icebergs
// ---------------------------------------------------------------------------

/// Query for `GET /icebergs`.
#[derive(Debug, Deserialize)]
pub struct IcebergQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Executed-to-visible ratio threshold. Default 3.0.
    pub ratio: Option<f64>,
    /// Trailing seconds of depth history + tape to scan. Default 120.
    pub seconds: Option<u64>,
}

/// One iceberg candidate.
#[derive(Debug, Serialize)]
pub struct IcebergResponse {
    /// The price level that showed refill behavior.
    pub price: f64,
    /// `buy` = hidden buyer absorbing sells; `sell` = hidden seller.
    pub side: &'static str,
    /// Executed volume against that side at that price.
    pub executed: f64,
    /// Largest size ever displayed there in the window.
    pub max_visible: f64,
    /// `executed / max_visible` -- the confidence proxy.
    pub ratio: f64,
}

/// The `GET /icebergs` response.
#[derive(Debug, Serialize)]
pub struct IcebergsResponse {
    /// Instrument.
    pub symbol: String,
    /// Candidates, highest confidence first.
    pub icebergs: Vec<IcebergResponse>,
    /// Depth snapshots scanned.
    pub snapshots: usize,
    /// Trades scanned.
    pub trades: usize,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// `GET /icebergs` -- hidden-liquidity candidates from the Resistance method
/// (`docs/22` phase 4). Every event is a *probabilistic* candidate; a level
/// refilling faster than it displays is the behavior, not a proven order.
///
/// # Errors
/// 404 when no depth history exists yet for the symbol.
pub async fn icebergs(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<IcebergQuery>,
) -> Result<Json<IcebergsResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    state.bots.ensure_feed_for(&symbol);
    let seconds = query.seconds.unwrap_or(120).clamp(5, 600) as i64;
    let window_ns = seconds * 1_000_000_000;

    let live = state.bots.live();
    let snapshots = live.book_history(&symbol);
    if snapshots.is_empty() {
        return Err(ApiError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "NO_DEPTH_HISTORY",
            format!(
                "no order-book history for {symbol} yet. Iceberg detection compares executed \
                 volume against the largest size ever *displayed* at a level, which needs depth \
                 history, not just the newest book. It becomes available seconds after the \
                 symbol's feed starts."
            ),
        ));
    }

    let newest = snapshots.last().map_or(0, |s| s.timestamp);
    let from = newest - window_ns;
    let snapshots: Vec<_> = snapshots
        .iter()
        .filter(|s| s.timestamp >= from)
        .cloned()
        .collect();
    let tape = live.tape(&symbol);
    let trades: Vec<_> = tape
        .range(from, i64::MAX)
        .into_iter()
        .filter(|t| t.timestamp >= from)
        .collect();

    let config = analytics_core::IcebergConfig {
        ratio_threshold: query.ratio.filter(|r| *r > 1.0).unwrap_or(3.0),
        ..analytics_core::IcebergConfig::default()
    };
    let events = detect_icebergs(&snapshots, &trades, config);

    let icebergs = events
        .iter()
        .map(|event| IcebergResponse {
            price: event.price,
            side: match event.side {
                Side::Buy => "buy",
                Side::Sell => "sell",
            },
            executed: event.executed,
            max_visible: event.max_visible,
            ratio: event.ratio,
        })
        .collect();

    Ok(Json(IcebergsResponse {
        symbol,
        icebergs,
        snapshots: snapshots.len(),
        trades: trades.len(),
        note: Some(
            "candidates are probabilistic: a level absorbing far more than it ever displayed \
             is iceberg *behavior*, not a proven order."
                .into(),
        ),
    }))
}

// ---------------------------------------------------------------------------
// GET /vpin
// ---------------------------------------------------------------------------

/// Query for `GET /vpin`.
#[derive(Debug, Deserialize)]
pub struct VpinQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Resolution, e.g. `5m` -- selects how far back the tape is read.
    pub timeframe: String,
    /// Traded volume per bucket, in base quantity.
    pub bucket_volume: Option<f64>,
    /// Buckets in the rolling mean. Default 50.
    pub buckets: Option<usize>,
    /// Candles' worth of tape to scan. Default 120.
    pub limit: Option<usize>,
    /// Window start, unix milliseconds, inclusive.
    pub from: Option<i64>,
    /// Window end, unix milliseconds, exclusive.
    pub to: Option<i64>,
}

/// One VPIN reading.
#[derive(Debug, Clone, Serialize)]
pub struct VpinPointResponse {
    /// Timestamp the reading was taken at, unix nanoseconds.
    pub timestamp: i64,
    /// Cumulative volume up to this point.
    pub cumulative_volume: f64,
    /// The VPIN value, `0..1`.
    pub vpin: f64,
}

/// The `GET /vpin` response.
#[derive(Debug, Serialize)]
pub struct VpinResponse {
    /// Instrument.
    pub symbol: String,
    /// The bucket volume used.
    pub bucket_volume: f64,
    /// The rolling-window size used.
    pub buckets: usize,
    /// The newest reading, when the tape warmed up enough to produce one.
    pub latest: Option<VpinPointResponse>,
    /// A preview of recent readings, oldest first.
    pub series: Vec<VpinPointResponse>,
    /// A caveat, when there is one.
    pub note: Option<String>,
}

/// `GET /vpin` -- order-flow toxicity over volume buckets (`docs/22` phase 5).
/// High VPIN precedes volatility bursts; it is a risk input, not a direction.
///
/// # Errors
/// 404 when the window holds no candles.
pub async fn vpin(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<VpinQuery>,
) -> Result<Json<VpinResponse>, ApiError> {
    let window = parse_window(&WindowQuery {
        symbol: query.symbol.clone(),
        timeframe: query.timeframe.clone(),
        limit: query.limit,
        from: query.from,
        to: query.to,
    })?;
    let (candles, trades, has_trades) = load_window(&state, &window).await?;

    let config = VpinConfig {
        bucket_volume: query
            .bucket_volume
            .filter(|v| *v > 0.0)
            .unwrap_or_else(|| {
                // One bucket per ~0.5% of the window's volume: activity-paced
                // sampling without hard-coding an instrument-specific size.
                let total: f64 = candles.iter().map(|c| c.volume).sum();
                (total / 200.0).max(1.0)
            }),
        buckets: query.buckets.unwrap_or(50).clamp(2, 500),
    };
    let series = calculate_vpin_series(&trades, &config);
    let points = series
        .iter()
        .map(|point| VpinPointResponse {
            timestamp: point.timestamp,
            cumulative_volume: point.cumulative_volume,
            vpin: point.vpin,
        })
        .collect::<Vec<_>>();
    let latest = points.last().cloned();

    Ok(Json(VpinResponse {
        symbol: window.symbol,
        bucket_volume: config.bucket_volume,
        buckets: config.buckets,
        latest,
        series: points,
        note: (!has_trades).then(|| NO_TICK_NOTE.into()),
    }))
}
