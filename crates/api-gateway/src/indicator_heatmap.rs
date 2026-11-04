//! Multi-timeframe family heatmap for the chart inspector.

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;

use analytics_core::types::Timeframe;

use crate::{AppState, error::ApiError};

#[derive(Debug, Deserialize)]
pub struct HeatmapQuery {
    pub symbol: String,
    pub timeframe: String,
}

/// `GET /indicator-heatmap`: five family readings across the active timeframe
/// and two comparison rungs, with the backend's common analytics implementation.
pub async fn get(
    State(state): State<AppState>,
    Query(query): Query<HeatmapQuery>,
) -> Result<Json<analytics_core::indicator_heatmap::IndicatorHeatmap>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    let base: Timeframe = query.timeframe.parse().map_err(|_| {
        ApiError::bad_request(
            "TIMEFRAME_UNKNOWN",
            format!("unknown timeframe `{}`", query.timeframe),
        )
    })?;
    let timeframes = timeframe_ladder(base);
    let to_ns = crate::now_ns();
    let windows = futures::future::join_all(timeframes.iter().map(|timeframe| {
        let from_ns = to_ns.saturating_sub(300 * timeframe.nanos());
        state
            .windows
            .candles_capped(&symbol, *timeframe, from_ns, to_ns, 300)
    }))
    .await;
    let mut inputs = Vec::new();
    for (timeframe, result) in timeframes.into_iter().zip(windows) {
        if let Ok(window) = result {
            inputs.push((timeframe, window.candles, None));
        }
    }
    if inputs.is_empty() {
        return Err(ApiError::unavailable(format!(
            "no candle history is available for {symbol}"
        )));
    }

    // Deriv's market data is a quote stream without trade size. Count observed
    // quote updates into each timeframe's bars and label the family as proxy
    // activity. Binance continues to use the venue's reported candle volume.
    if market_data::broker::VenueSymbol::parse(&symbol).broker == "deriv" {
        if let Some(code) = market_data::deriv::code_for_namespaced(&symbol) {
            if let (Some(first), Some(last)) = (
                inputs
                    .iter()
                    .flat_map(|(_, candles, _)| candles.first().map(|c| c.open_time))
                    .min(),
                inputs
                    .iter()
                    .filter_map(|(_, candles, _)| candles.last())
                    .max_by_key(|c| c.open_time),
            ) {
                let end_ns = last.open_time.saturating_add(last.timeframe.nanos());
                if let Ok(points) = market_data::deriv::DerivClient::from_env()
                    .quote_history(code, first, end_ns, 5_000)
                    .await
                {
                    for (timeframe, candles, activity) in &mut inputs {
                        *activity = Some(count_quotes_per_candle(candles, &points, *timeframe));
                    }
                }
            }
        }
    }
    Ok(Json(analytics_core::indicator_heatmap::calculate(
        &symbol, base, &inputs,
    )))
}

fn timeframe_ladder(base: Timeframe) -> Vec<Timeframe> {
    let mut all = Timeframe::ALL.to_vec();
    all.push(base);
    all.sort();
    all.dedup();
    let mut selected = vec![base];
    for multiplier in [4_i64, 16] {
        let next = all
            .iter()
            .copied()
            .find(|timeframe| timeframe.nanos() >= base.nanos().saturating_mul(multiplier))
            .or_else(|| {
                all.iter()
                    .copied()
                    .filter(|timeframe| *timeframe > base)
                    .max()
            });
        if let Some(next) = next {
            if !selected.contains(&next) {
                selected.push(next);
            }
        }
    }
    if selected.len() < 3 {
        let lower = all
            .iter()
            .copied()
            .filter(|timeframe| *timeframe < base)
            .max();
        if let Some(lower) = lower {
            if !selected.contains(&lower) {
                selected.push(lower);
            }
        }
    }
    selected.sort();
    selected
}

fn count_quotes_per_candle(
    candles: &[analytics_core::Candle],
    points: &[(i64, f64)],
    timeframe: Timeframe,
) -> Vec<f64> {
    let mut sorted = points.to_vec();
    sorted.sort_by_key(|(time, _)| *time);
    let mut counts = vec![0.0; candles.len()];
    for (time, _) in sorted {
        let index = candles
            .partition_point(|candle| candle.open_time.saturating_add(timeframe.nanos()) <= time);
        if let Some(candle) = candles.get(index) {
            if time >= candle.open_time {
                counts[index] += 1.0;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeframe_ladder_uses_active_and_higher_frames() {
        assert_eq!(
            timeframe_ladder(Timeframe::M15),
            vec![Timeframe::M15, Timeframe::H1, Timeframe::H4]
        );
        assert_eq!(
            timeframe_ladder(Timeframe::H4),
            vec![Timeframe::H4, Timeframe::D1, Timeframe::W1]
        );
    }

    #[test]
    fn quote_activity_is_bucketed_by_candle_open_and_timeframe() {
        let candles = vec![
            analytics_core::Candle {
                symbol: "DERIV:V75".into(),
                timeframe: Timeframe::M1,
                open_time: 0,
                open: 1.0,
                high: 2.0,
                low: 0.0,
                close: 1.0,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            },
            analytics_core::Candle {
                symbol: "DERIV:V75".into(),
                timeframe: Timeframe::M1,
                open_time: Timeframe::M1.nanos(),
                open: 1.0,
                high: 2.0,
                low: 0.0,
                close: 1.0,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            },
        ];
        let points = [
            (1, 1.0),
            (Timeframe::M1.nanos() + 1, 1.0),
            (2 * Timeframe::M1.nanos(), 1.0),
        ];
        assert_eq!(
            count_quotes_per_candle(&candles, &points, Timeframe::M1),
            vec![1.0, 1.0]
        );
    }
}
