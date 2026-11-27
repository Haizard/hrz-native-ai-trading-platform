//! Deterministic multiscale opportunity scan.
//!
//! A public read, like `/scan`: it loads completed candles for the requested
//! timeframe ladder, builds one strict multiscale observation per symbol, and
//! returns the scanner's verdict. Setups are returned as chart-ready price
//! overlays so the shell can display them without a new scene field. Nothing
//! here places a trade; the existing strategy and risk gates still apply.

use analytics_core::opportunity_scanner::{scan_instrument, InstrumentProfile, ScanVerdict};
use analytics_core::{all_disciplines, MultiTimeframeObservation, Timeframe};
use axum::extract::State;
use axum::Json;
use chart_engine::{Overlay, OverlayRole};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::market_data::WindowMarketData;
use crate::AppState;

use ai_agent::MarketDataSource;

const MAX_SYMBOLS: usize = 50;
const MAX_TIMEFRAMES: usize = 6;
const DEFAULT_LOOKBACK: usize = 120;
const MAX_LOOKBACK: usize = 2000;
const DEFAULT_DISCIPLINE: &str = "bayesian";

/// Query for `GET /scan/multiscale`.
#[derive(Debug, Deserialize)]
pub struct MultiscaleScanQuery {
    /// Comma-separated symbols. Required, to avoid an unbounded venue sweep.
    pub symbols: String,
    /// Comma-separated timeframe ladder, ordered coarse to fine by the core.
    pub timeframes: String,
    /// Discipline name. Defaults to `bayesian`.
    pub discipline: Option<String>,
    /// Bars loaded per timeframe. Defaults to 120.
    pub lookback: Option<usize>,
}

/// One symbol's result.
#[derive(Debug, Serialize)]
pub struct MultiscaleScanRow {
    /// Symbol scanned.
    pub symbol: String,
    /// Scanner recommendation; subject to strategy rules and the risk gate.
    pub recommendation: String,
    /// Deepest screening stage reached.
    pub level_reached: String,
    /// Ranking score in `[0, 1]`; not a probability.
    pub ranking_score: f64,
    /// Reasons recorded by each stage.
    pub reasons: Vec<String>,
    /// Setup proposal, when the setup stage was reached.
    pub setup: Option<serde_json::Value>,
    /// Chart overlays for a proposed setup.
    pub overlays: Vec<Overlay>,
}

/// Failed symbol with the reason, kept separate from successful rows.
#[derive(Debug, Serialize)]
pub struct MultiscaleScanFailure {
    /// Symbol that could not be scanned.
    pub symbol: String,
    /// Why.
    pub error: String,
}

/// Response body of `GET /scan/multiscale`.
#[derive(Debug, Serialize)]
pub struct MultiscaleScanResponse {
    /// Discipline used for every symbol.
    pub discipline: String,
    /// Timeframes used, coarsest first.
    pub timeframes: Vec<String>,
    /// Successful scans.
    pub rows: Vec<MultiscaleScanRow>,
    /// Symbols whose candles could not be loaded or observed.
    pub failures: Vec<MultiscaleScanFailure>,
    /// Explicit limitation of this response.
    pub note: String,
}

fn comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>()
}

fn timeframes_from(raw: &str) -> Result<Vec<Timeframe>, ApiError> {
    let mut values = Vec::new();
    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let timeframe = token.parse::<Timeframe>().map_err(|_| {
            ApiError::bad_request(
                "unknown_timeframe",
                format!("`{token}` is not a timeframe this platform supports"),
            )
            .with_details(json!({
                "timeframe": token,
                "known": Timeframe::ALL.iter().map(|timeframe| timeframe.as_str()).collect::<Vec<_>>(),
            }))
        })?;
        if !values.contains(&timeframe) {
            values.push(timeframe);
        }
    }
    if values.is_empty() || values.len() > MAX_TIMEFRAMES {
        return Err(ApiError::bad_request(
            "invalid_timeframes",
            format!("provide between 1 and {MAX_TIMEFRAMES} distinct timeframes"),
        ));
    }
    Ok(values)
}

fn overlays_for(verdict: &ScanVerdict) -> Vec<Overlay> {
    let Some(setup) = &verdict.setup else {
        return Vec::new();
    };
    let mut overlays = vec![Overlay {
        price: setup.entry_low,
        label: format!("{} entry zone", setup.timeframe),
        role: OverlayRole::Entry,
        band_to: Some(setup.entry_high),
        filled: true,
    }];
    overlays.push(Overlay {
        price: setup.stop,
        label: "stop".to_string(),
        role: OverlayRole::Stop,
        band_to: None,
        filled: false,
    });
    for (index, target) in setup.targets.iter().enumerate() {
        overlays.push(Overlay {
            price: *target,
            label: format!("target {}", index + 1),
            role: OverlayRole::Target,
            band_to: None,
            filled: false,
        });
    }
    overlays
}

async fn observe_symbol(
    data: &WindowMarketData,
    symbol: &str,
    ladder: &[Timeframe],
    lookback: usize,
) -> Result<MultiTimeframeObservation, String> {
    let mut series = Vec::with_capacity(ladder.len());
    let mut decision_timestamp = 0_i64;
    for timeframe in ladder {
        let latest = data
            .latest_candle_time(symbol, *timeframe)
            .await
            .map_err(|error| format!("{timeframe}: {error}"))?
            .ok_or_else(|| format!("{timeframe}: no candles yet"))?;
        let from = latest - (lookback.saturating_sub(1) as i64) * timeframe.nanos();
        let candles = data
            .candles(
                symbol,
                *timeframe,
                from,
                latest.saturating_add(timeframe.nanos()),
            )
            .await
            .map_err(|error| format!("{timeframe}: {error}"))?;
        if let Some(last) = candles.last() {
            decision_timestamp =
                decision_timestamp.max(last.open_time.saturating_add(timeframe.nanos()));
        }
        series.push((*timeframe, candles));
    }
    MultiTimeframeObservation::from_series(
        symbol.to_string(),
        decision_timestamp,
        series,
        "gateway-market-data",
        2,
    )
    .map_err(|error| error.to_string())
}

/// Run the deterministic multiscale scanner over an explicit symbol list.
pub async fn scan_multiscale(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<MultiscaleScanQuery>,
) -> Result<Json<MultiscaleScanResponse>, ApiError> {
    let symbols = comma_list(&query.symbols);
    if symbols.is_empty() || symbols.len() > MAX_SYMBOLS {
        return Err(ApiError::bad_request(
            "invalid_symbols",
            format!("provide between 1 and {MAX_SYMBOLS} comma-separated symbols"),
        ));
    }
    let ladder = timeframes_from(&query.timeframes)?;
    let discipline = query
        .discipline
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_DISCIPLINE);
    let known: Vec<&str> = all_disciplines().iter().map(|(name, _)| *name).collect();
    if !known.contains(&discipline) {
        return Err(ApiError::bad_request(
            "unknown_discipline",
            format!("`{discipline}` is not a Core Analytics discipline"),
        )
        .with_details(json!({ "known": known })));
    }
    let lookback = query
        .lookback
        .unwrap_or(DEFAULT_LOOKBACK)
        .clamp(2, MAX_LOOKBACK);
    let data = WindowMarketData::new(state.windows.clone());

    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for symbol in symbols {
        let observation = match observe_symbol(&data, &symbol, &ladder, lookback).await {
            Ok(observation) => observation,
            Err(error) => {
                failures.push(MultiscaleScanFailure { symbol, error });
                continue;
            }
        };
        let profile = InstrumentProfile::new(symbol.clone(), ladder.clone());
        let verdict = scan_instrument(&profile, &observation, discipline);
        rows.push(MultiscaleScanRow {
            symbol: symbol.clone(),
            recommendation: format!("{:?}", verdict.recommendation),
            level_reached: format!("{:?}", verdict.level_reached),
            ranking_score: verdict.ranking_score,
            reasons: verdict.reasons.clone(),
            setup: verdict
                .setup
                .as_ref()
                .and_then(|setup| serde_json::to_value(setup).ok()),
            overlays: overlays_for(&verdict),
        });
    }

    Ok(Json(MultiscaleScanResponse {
        discipline: discipline.to_string(),
        timeframes: ladder
            .iter()
            .map(|timeframe| timeframe.as_str().to_string())
            .collect(),
        rows,
        failures,
        note: "setups are proposals only: strategy rules and the risk gate still apply; ranking is not a probability"
            .to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_list_is_uppercased_and_deduplicated_by_caller() {
        assert_eq!(
            comma_list(" btcusdt, ETHUSDT ,"),
            vec!["BTCUSDT", "ETHUSDT"]
        );
        assert!(comma_list(" , ,").is_empty());
    }

    #[test]
    fn timeframes_are_parsed_and_deduplicated() {
        let ladder = timeframes_from("1h,15m,5m, 15m").unwrap();
        assert_eq!(ladder, vec![Timeframe::H1, Timeframe::M15, Timeframe::M5]);
        assert!(timeframes_from("nonsense").is_err());
        assert!(timeframes_from("").is_err());
    }

    #[test]
    fn overlays_come_only_from_a_proposed_setup() {
        let empty = ScanVerdict {
            symbol: "BTCUSDT".to_string(),
            level_reached: analytics_core::ScreeningLevel::DataQuality,
            recommendation: analytics_core::ScanRecommendation::Skip,
            ranking_score: 0.0,
            candidate: None,
            setup: None,
            reasons: Vec::new(),
        };
        assert!(overlays_for(&empty).is_empty());
    }
}
