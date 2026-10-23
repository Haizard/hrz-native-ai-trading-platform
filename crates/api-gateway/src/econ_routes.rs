//! The Economic Event Intelligence Engine's routes: the releases (with the
//! surprise and the macro bias), the schedule, the forecast override, and
//! the market-reaction report. The engine's doctrine is the user's own: an
//! economic event is an information catalyst, not a signal -- and the
//! market's own reaction is the confirmation, never the headline.

use axum::{extract::Query, extract::State, Json};
use market_data::econ;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::AppState;

/// The BLS key: the environment overrides it; the operator-provided default
/// belongs to this deployment's own BLS registration.
const BLS_API_KEY_ENV: &str = "BLS_API_KEY";
const BLS_API_KEY_DEFAULT: &str = "541e1bb2a2bc4660995a896ac699c948";

fn api_key() -> String {
    std::env::var(BLS_API_KEY_ENV).unwrap_or_else(|_| BLS_API_KEY_DEFAULT.to_string())
}

/// The engine's store: the newest releases per kind, refreshed every six
/// hours, plus operator forecast overrides. Held in AppState, cloned into
/// the refresh task (the Arc is the whole of the sharing).
#[derive(Debug, Default, Clone)]
pub struct EconStore {
    /// The releases, newest-first per kind.
    pub releases: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<econ::EconRelease>>>>,
    /// Operator overrides: `{kind}:{period}` -> (forecast, source).
    pub overrides: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, (f64, String)>>>,
}

/// Rebuild one kind's release list from its BLS series. The NFP headline is
/// the monthly CHANGE of the level; the forecast is the trailing twelve
/// months' mean change, documented as the naive consensus until an operator
/// overrides it.
pub async fn refresh_releases(store: &EconStore, kind: &str) -> Result<usize, String> {
    let client = reqwest::Client::new();
    let key = api_key();
    let (series_id, headline_is_change) = match kind {
        "nfp" => (econ::SERIES_NFP_LEVEL, true),
        "cpi" => (econ::SERIES_CPI_LEVEL, true),
        _ => return Err(format!("unknown econ kind `{kind}`")),
    };
    let points = econ::fetch_series(&client, &key, series_id, 36).await?;
    if points.len() < 14 {
        return Err(format!("BLS returned {} points for {series_id}", points.len()));
    }
    // The values, oldest-first for the change math.
    let mut values: Vec<(String, f64)> = points
        .iter()
        .rev()
        .filter_map(|p| econ::point_value(p).map(|v| (format!("{}-{}", p.year, p.period.trim_start_matches('M')), v)))
        .collect();
    // The monthly changes (for change-series): actual and its trailing stats.
    let mut releases = Vec::new();
    let unemployment = if kind == "nfp" {
        Some(econ::fetch_series(&client, &key, econ::SERIES_UNEMPLOYMENT, 4).await?)
    } else {
        None
    };
    let earnings = if kind == "nfp" {
        Some(econ::fetch_series(&client, &key, econ::SERIES_EARNINGS, 4).await?)
    } else {
        None
    };
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let overrides = store.overrides.read().map_err(|e| e.to_string())?.clone();
    let mut changes: Vec<f64> = Vec::new();
    for w in values.windows(2) {
        changes.push(w[1].1 - w[0].1);
    }
    for i in 12..values.len() - 1 {
        let (period, value) = &values[i + 1];
        let (_prev_period, prev_value) = &values[i];
        let actual = if headline_is_change { value - prev_value } else { *value };
        let trailing = &changes[i.saturating_sub(12).max(1) - 1..i.saturating_sub(1).max(1)];
        let forecast_default = if trailing.is_empty() { 0.0 } else { trailing.iter().sum::<f64>() / trailing.len() as f64 };
        let (forecast, source) = overrides
            .get(&format!("{kind}:{period}"))
            .map(|(f, s)| (*f, s.clone()))
            .unwrap_or((forecast_default, "trailing_mean".to_string()));
        // The release timestamp: the NEXT month's first Friday 08:30 ET.
        // (The data describes `period`; it releases the following month.)
        let revision = None; // revisions land in the following release's note; stored when known
        let release_ns = {
            // Approximate: the first Friday of the month AFTER the period.
            // The schedule rule in econ::next_nfp_release is the canonical
            // forward-looking one; for historical records we anchor the
            // period's own end + the rule's weekday.
            let (y, m) = {
                let mut parts = period.split('-');
                (
                    parts.next().and_then(|y| y.parse::<i64>().ok()).unwrap_or(2026),
                    parts.next().and_then(|m| m.parse::<i64>().ok()).unwrap_or(1),
                )
            };
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            // The first Friday of (ny, nm) at 13:30 UTC.
            let days = days_from_civil(ny, nm, 1);
            let first_friday = (0..7)
                .find(|d| ((days + d).rem_euclid(7)) == 2) // 2 = Friday for the 1970-01-01 (Thursday) epoch
                .unwrap_or(0);
            (days + first_friday) * 3_600_000_000_000 * 24 + 13 * 3_600_000_000_000 + 30 * 60_000_000_000
        };
        releases.push(econ::EconRelease {
            id: format!("{kind}-{period}"),
            kind: kind.to_string(),
            period: period.clone(),
            release_ns,
            actual,
            unemployment: unemployment.as_ref().and_then(|u| u.first()).and_then(econ::point_value),
            earnings_change: earnings.as_ref().and_then(|e| {
                let pts: Vec<f64> = e.iter().filter_map(econ::point_value).collect();
                if pts.len() >= 2 && pts[1] > 0.0 {
                    Some((pts[0] / pts[1] - 1.0) * 100.0)
                } else {
                    None
                }
            }),
            forecast,
            forecast_source: source,
            revision,
            retrieved_ns: now_ns,
        });
    }
    let count = releases.len();
    store
        .releases
        .write()
        .map_err(|e| e.to_string())?
        .insert(kind.to_string(), releases);
    let _ = &mut values;
    Ok(count)
}

const FRED_API_KEY_ENV: &str = "FRED_API_KEY";
const FRED_API_KEY_DEFAULT: &str = "a62f9270e479cc5166b8e4a56344ad7c";

fn fred_key() -> String {
    std::env::var(FRED_API_KEY_ENV).unwrap_or_else(|_| FRED_API_KEY_DEFAULT.to_string())
}

/// Refresh the FRED-backed kinds: fomc (the funds target's change IS the
/// decision), claims (weekly), pce (YoY), gdp (quarterly annualized QoQ).
pub async fn refresh_fred(store: &EconStore, kind: &str) -> Result<usize, String> {
    let client = reqwest::Client::new();
    let key = fred_key();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let overrides = store.overrides.read().map_err(|e| e.to_string())?.clone();
    let mut releases = Vec::new();
    match kind {
        "fomc" => {
            let points = market_data::fred::fetch_fred(&client, &key, market_data::fred::SERIES_FUNDS_TARGET, 400).await?;
            if points.len() < 3 {
                return Err("FRED returned too few funds points".to_string());
            }
            for w in points.windows(2) {
                let change = w[0].value - w[1].value;
                if change.abs() > 1e-9 {
                    let period = w[0].date[..7].to_string();
                    let (forecast, source) = overrides
                        .get(&format!("fomc:{period}"))
                        .map(|(f, s)| (*f, s.clone()))
                        .unwrap_or((0.0, "hold_default".to_string()));
                    releases.push(econ::EconRelease {
                        id: format!("fomc-{period}"),
                        kind: "fomc".to_string(),
                        period,
                        release_ns: market_data::fred::date_ns(&w[0].date).unwrap_or(0),
                        actual: change,
                        unemployment: None,
                        earnings_change: None,
                        forecast,
                        forecast_source: source,
                        revision: None,
                        retrieved_ns: now_ns,
                    });
                }
            }
        }
        "claims" => {
            let points = market_data::fred::fetch_fred(&client, &key, market_data::fred::SERIES_CLAIMS, 60).await?;
            if points.len() < 10 {
                return Err("FRED returned too few claims points".to_string());
            }
            // The last twelve weekly releases: the sigma needs the history
            // (a store of one release has no trailing to normalize against).
            for (i, p) in points.iter().take(12).enumerate() {
                let trailing: Vec<f64> = points.iter().skip(i + 1).take(8).map(|q| q.value).collect();
                let forecast_default = if trailing.is_empty() { p.value } else { trailing.iter().sum::<f64>() / trailing.len() as f64 };
                let (forecast, source) = overrides
                    .get(&format!("claims:{}", p.date))
                    .map(|(f, s)| (*f, s.clone()))
                    .unwrap_or((forecast_default, "trailing_mean".to_string()));
                releases.push(econ::EconRelease {
                    id: format!("claims-{}", p.date),
                    kind: "claims".to_string(),
                    period: p.date[..7].to_string(),
                    release_ns: market_data::fred::date_ns(&p.date).unwrap_or(0),
                    actual: p.value,
                    unemployment: None,
                    earnings_change: None,
                    forecast,
                    forecast_source: source,
                    revision: None,
                    retrieved_ns: now_ns,
                });
            }
        }
        "pce" => {
            let points = market_data::fred::fetch_fred(&client, &key, market_data::fred::SERIES_PCE_LEVEL, 40).await?;
            if points.len() < 14 {
                return Err("FRED returned too few PCE points".to_string());
            }
            let yoy: Vec<(String, f64)> = (0..points.len().saturating_sub(12))
                .map(|i| (points[i].date[..7].to_string(), (points[i].value / points[i + 12].value - 1.0) * 100.0))
                .collect();
            for (i, (period, actual)) in yoy.iter().enumerate() {
                let trailing: Vec<f64> = yoy.iter().skip(i + 1).take(12).map(|(_, v)| *v).collect();
                let forecast_default = if trailing.is_empty() { *actual } else { trailing.iter().sum::<f64>() / trailing.len() as f64 };
                let (forecast, source) = overrides
                    .get(&format!("pce:{period}"))
                    .map(|(f, s)| (*f, s.clone()))
                    .unwrap_or((forecast_default, "trailing_mean".to_string()));
                releases.push(econ::EconRelease {
                    id: format!("pce-{period}"),
                    kind: "pce".to_string(),
                    period: period.clone(),
                    release_ns: market_data::fred::date_ns(&points[i].date).unwrap_or(0),
                    actual: *actual,
                    unemployment: None,
                    earnings_change: None,
                    forecast,
                    forecast_source: source,
                    revision: None,
                    retrieved_ns: now_ns,
                });
            }
        }
        "gdp" => {
            let points = market_data::fred::fetch_fred(&client, &key, market_data::fred::SERIES_REAL_GDP, 12).await?;
            if points.len() < 3 {
                return Err("FRED returned too few GDP points".to_string());
            }
            let annualized: Vec<f64> = (0..points.len() - 1).map(|i| (points[i].value / points[i + 1].value - 1.0) * 100.0 * 4.0).collect();
            for (i, actual) in annualized.iter().enumerate() {
                let trailing: Vec<f64> = annualized.iter().skip(i + 1).take(8).copied().collect();
                let forecast_default = if trailing.is_empty() { *actual } else { trailing.iter().sum::<f64>() / trailing.len() as f64 };
                let period = points[i].date[..7].to_string();
                let (forecast, source) = overrides
                    .get(&format!("gdp:{period}"))
                    .map(|(f, s)| (*f, s.clone()))
                    .unwrap_or((forecast_default, "trailing_mean".to_string()));
                releases.push(econ::EconRelease {
                    id: format!("gdp-{period}"),
                    kind: "gdp".to_string(),
                    period,
                    release_ns: market_data::fred::date_ns(&points[i].date).unwrap_or(0),
                    actual: *actual,
                    unemployment: None,
                    earnings_change: None,
                    forecast,
                    forecast_source: source,
                    revision: None,
                    retrieved_ns: now_ns,
                });
            }
        }
        _ => return Err(format!("unknown FRED kind `{kind}`")),
    }
    // The store is ASCENDING by release time (report_for reads the newest
    // off the end); the FRED windows walk newest-first, so sort on insert.
    releases.sort_by_key(|r| r.release_ns);
    let count = releases.len();
    store
        .releases
        .write()
        .map_err(|e| e.to_string())?
        .insert(kind.to_string(), releases);
    Ok(count)
}

/// `GET /econ/macro`: the Fed's own numbers as one context -- the funds
/// rate and its direction, the curve, the real yield, the dollar, and the
/// composite the Quant Brain reads before any instrument.
pub async fn macro_context(State(_state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let client = reqwest::Client::new();
    let key = fred_key();
    let f = market_data::fred::fetch_fred;
    let (funds, dgs10, dgs2, dfii10, dollar) = (
        f(&client, &key, market_data::fred::SERIES_FUNDS_TARGET, 300).await.map_err(ApiError::unavailable)?,
        f(&client, &key, market_data::fred::SERIES_DGS10, 5).await.map_err(ApiError::unavailable)?,
        f(&client, &key, market_data::fred::SERIES_DGS2, 5).await.map_err(ApiError::unavailable)?,
        f(&client, &key, market_data::fred::SERIES_DFII10, 5).await.map_err(ApiError::unavailable)?,
        f(&client, &key, market_data::fred::SERIES_DOLLAR, 300).await.map_err(ApiError::unavailable)?,
    );
    let ctx = market_data::fred::macro_context(&funds, &dgs10, &dgs2, &dfii10, &dollar)
        .ok_or_else(|| ApiError::unavailable("FRED series too short for the context"))?;
    Ok(Json(serde_json::json!({
        "funds_rate": ctx.funds_rate,
        "funds_direction": ctx.funds_direction,
        "curve_2s10s": ctx.curve,
        "real_yield_10y": ctx.real_yield,
        "dollar_change_pct": ctx.dollar_change_pct,
        "composite": ctx.composite,
        "label": market_data::fred::context_label(ctx.composite),
        "note": "the Fed's own numbers as one read: policy direction dominates, the curve warns, the real yield and the dollar weigh. This is the macro context; an instrument's reaction is still its own confirmation.",
    })))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// One release as the engine reports it: the record plus the surprise and
/// the bias, labeled the way a trader reads.
#[derive(Debug, Serialize)]
pub struct ReleaseReport {
    /// The stored record.
    pub release: econ::EconRelease,
    /// The normalized surprise.
    pub surprise: econ::Surprise,
    /// The composite macro bias (-2..2, USD-supportive positive).
    pub macro_bias: f64,
    /// The bias in words.
    pub bias_label: &'static str,
}

fn report_for(store: &EconStore, kind: &str) -> Result<ReleaseReport, ApiError> {
    let releases = store.releases.read().map_err(|e| ApiError::unavailable(e.to_string()))?;
    let list = releases
        .get(kind)
        .ok_or_else(|| ApiError::unavailable(format!("no `{kind}` releases stored yet (the refresh runs on boot and every 6h)")))?;
    let release = list
        .last()
        .ok_or_else(|| ApiError::unavailable(format!("empty `{kind}` release list")))?
        .clone();
    // The trailing changes for the sigma: the stored list's own history.
    let trailing: Vec<f64> = list
        .iter()
        .rev()
        .skip(1)
        .take(12)
        .map(|r| r.actual)
        .collect();
    let surprise = econ::surprise(release.actual, release.forecast, &trailing);
    let prev = list.iter().rev().nth(1);
    let bias = econ::macro_bias_with_context(
        &release,
        &surprise,
        prev.and_then(|p| p.unemployment),
        prev.and_then(|p| p.earnings_change),
    );
    Ok(ReleaseReport {
        release,
        surprise,
        macro_bias: bias,
        bias_label: econ::bias_label(bias),
    })
}

/// `/econ/latest` query.
#[derive(Debug, Deserialize)]
pub struct LatestQuery {
    /// The release kind (`nfp` default; `cpi` when its series is wired).
    pub kind: Option<String>,
}

/// `GET /econ/latest`: the newest release with its surprise and bias.
pub async fn latest(
    State(state): State<AppState>,
    Query(query): Query<LatestQuery>,
) -> Result<Json<ReleaseReport>, ApiError> {
    let kind = query.kind.unwrap_or_else(|| "nfp".to_string());
    report_for(&state.econ, &kind).map(Json)
}

/// `GET /econ/next`: the schedule, as the engine knows it.
pub async fn next(State(_state): State<AppState>) -> Json<serde_json::Value> {
    let now_ns = crate::now_ns();
    Json(serde_json::json!({
        "kind": "nfp",
        "next_release_ns": econ::next_nfp_release(now_ns),
        "rule": "first Friday of the month, 08:30 ET (13:30 UTC, winter offset documented)",
    }))
}

/// The forecast override body.
#[derive(Debug, Deserialize)]
pub struct ForecastOverride {
    /// The release kind.
    pub kind: String,
    /// The period the forecast describes (`2026-09`).
    pub period: String,
    /// The consensus value.
    pub forecast: f64,
    /// Where it came from (named for the record, e.g. `investing.com`).
    pub source: String,
}

/// `POST /econ/forecast`: store the REAL consensus, ending look-ahead bias.
/// The override is kept against its period and used in every later report.
pub async fn forecast_override(
    State(state): State<AppState>,
    Json(body): Json<ForecastOverride>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !(body.forecast.is_finite()) {
        return Err(ApiError::bad_request("FORECAST_INVALID", "the forecast must be a number"));
    }
    state
        .econ
        .overrides
        .write()
        .map_err(|e| ApiError::unavailable(e.to_string()))?
        .insert(format!("{}:{}", body.kind, body.period), (body.forecast, body.source.clone()));
    // Recompute the affected release if it's already stored.
    if let Ok(mut releases) = state.econ.releases.write() {
        if let Some(list) = releases.get_mut(&body.kind) {
            for r in list.iter_mut() {
                if r.period == body.period {
                    r.forecast = body.forecast;
                    r.forecast_source = body.source.clone();
                }
            }
        }
    }
    Ok(Json(serde_json::json!({
        "stored": true,
        "kind": body.kind,
        "period": body.period,
        "forecast": body.forecast,
        "source": body.source,
        "note": "the override is stored against its period; every later report and backtest reads it, never a later consensus",
    })))
}

/// `/econ/reaction` query.
#[derive(Debug, Deserialize)]
pub struct ReactionQuery {
    /// The instrument (required).
    pub symbol: String,
    /// The release kind (`nfp` default).
    pub kind: Option<String>,
    /// The chart's timeframe (default 15m).
    pub timeframe: Option<String>,
}

/// `GET /econ/reaction`: the market's own answer to the release -- the
/// price, volume, and delta in the hours after, against the hours before.
/// The headline said what it said; this is what the market did about it.
pub async fn reaction(
    State(state): State<AppState>,
    Query(query): Query<ReactionQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let kind = query.kind.unwrap_or_else(|| "nfp".to_string());
    let report = report_for(&state.econ, &kind)?;
    let symbol = query.symbol.to_uppercase();
    // The venue gate, enforced at the engine's edge too: synthetics do not
    // read the BLS (the taxonomy's whole point).
    let parsed = market_data::broker::VenueSymbol::parse(&symbol);
    let channels = parsed.channels().unwrap_or(market_data::broker::DataChannels::CANDLES);
    if !channels.supports(market_data::broker::DataChannels::MACRO) {
        return Err(ApiError::bad_request(
            "MACRO_UNDEFINED",
            format!("`{symbol}` has no macro channel -- a synthetic index's price is algorithmic and does not read economic releases"),
        ));
    }
    let tf: analytics_core::types::Timeframe = query
        .timeframe
        .unwrap_or_else(|| "15m".to_string())
        .parse()
        .map_err(|_| ApiError::bad_request("TIMEFRAME_UNKNOWN", "the timeframe is not served"))?;
    let release_ns = report.release.release_ns;
    let window_ns = 4 * 3_600_000_000_000_i64; // 4 hours each side
    let before = state
        .windows
        .candles_capped(&symbol, tf, release_ns - window_ns, release_ns, 200)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles before the release: {e}")))?;
    let after = state
        .windows
        .candles_capped(&symbol, tf, release_ns, release_ns + window_ns, 200)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles after the release: {e}")))?;
    let summarize = |candles: &[analytics_core::types::Candle]| {
        if candles.is_empty() {
            return serde_json::json!({"bars": 0});
        }
        let first = candles.first().expect("checked");
        let last = candles.last().expect("checked");
        let volume: f64 = candles.iter().map(|c| c.volume).sum();
        let delta: f64 = candles.iter().map(|c| c.buy_volume - c.sell_volume).sum();
        serde_json::json!({
            "bars": candles.len(),
            "first_close": first.close,
            "last_close": last.close,
            "change_pct": (last.close / first.close - 1.0) * 100.0,
            "volume": volume,
            "delta": delta,
        })
    };
    let before_j = summarize(&before.candles);
    let after_j = summarize(&after.candles);
    // The read: does the reaction CONFIRM the macro bias? The bias says
    // USD up; a macro-long-USD instrument should fall (BTC priced in USD).
    let (before_change, after_change) = (
        before_j["change_pct"].as_f64().unwrap_or(0.0),
        after_j["change_pct"].as_f64().unwrap_or(0.0),
    );
    let after_delta = after_j["delta"].as_f64().unwrap_or(0.0);
    let confirms = report.macro_bias.signum() == (-after_change).signum() && after_change.abs() > 0.05;
    let absorbed = after_change.signum() != 0.0 && after_change.signum() != before_change.signum() && after_delta.signum() == after_change.signum();
    Ok(Json(serde_json::json!({
        "kind": kind,
        "symbol": symbol,
        "release": report.release,
        "surprise": report.surprise,
        "macro_bias": report.macro_bias,
        "bias_label": report.bias_label,
        "before": before_j,
        "after": after_j,
        "reaction": {
            "confirms_macro_bias": confirms,
            "absorption_hint": absorbed,
            "note": "the headline is information; this is the market's own answer. Confirmation is a state, not a candle count -- the quant layer reads this report and waits for its own rules.",
        },
    })))
}
