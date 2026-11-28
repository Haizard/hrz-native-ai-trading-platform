//! Public read-only pretrained Kronos forecast.

use axum::extract::State;
use axum::Json;
use kronos_intelligence::{ForecastConfig, KronosModel, ModelPaths};
use serde::Deserialize;
use serde_json::json;

use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::market_data::WindowMarketData;
use crate::AppState;

use ai_agent::MarketDataSource;

/// Query for `GET /kronos/forecast`.
#[derive(Debug, Deserialize)]
pub struct KronosQuery {
    /// Symbol to forecast.
    pub symbol: String,
    /// Historical resolution.
    pub timeframe: String,
    /// Historical bars, at most 512.
    pub lookback: Option<usize>,
    /// Future bars, at most 60.
    pub horizon: Option<usize>,
}

/// Return a deterministic pretrained-model forecast for one symbol.
pub async fn forecast(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<KronosQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let timeframe = query.timeframe.parse::<analytics_core::Timeframe>().map_err(|_| {
        ApiError::bad_request(
            "unknown_timeframe",
            format!("`{}` is not a timeframe this platform supports", query.timeframe),
        )
    })?;
    let symbol = query.symbol.to_uppercase();
    let lookback = query.lookback.unwrap_or(256).clamp(8, 512);
    let horizon = query.horizon.unwrap_or(12).clamp(1, 60);
    let data = WindowMarketData::new(state.windows.clone());
    let latest = data
        .latest_candle_time(&symbol, timeframe)
        .await
        .map_err(|error| ApiError::unavailable(format!("candles for {symbol} are not available: {error}")))?
        .ok_or_else(|| ApiError::unavailable(format!("no candles for {symbol} {timeframe} yet")))?;
    let from = latest - (lookback.saturating_sub(1) as i64) * timeframe.nanos();
    let candles = data
        .candles(&symbol, timeframe, from, latest.saturating_add(timeframe.nanos()))
        .await
        .map_err(|error| ApiError::unavailable(format!("candles for {symbol} are not available: {error}")))?;
    let model = KronosModel::load(&ModelPaths::default())
        .map_err(|error| ApiError::unavailable(format!("pretrained model could not be loaded: {error}")))?;
    let result = model
        .forecast(
            &symbol,
            &candles,
            &ForecastConfig {
                lookback: lookback.min(candles.len()),
                horizon,
                context: 512,
                ..ForecastConfig::default()
            },
        )
        .map_err(|error| ApiError::unavailable(format!("forecast failed: {error}")))?;
    Ok(Json(json!({
        "symbol": symbol,
        "timeframe": timeframe.as_str(),
        "bars_loaded": candles.len(),
        "forecast": result,
    })))
}
