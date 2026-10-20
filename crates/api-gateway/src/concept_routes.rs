//! The Concept Registry's public surface.
//!
//! Two routes, both read projections (nothing here mutates state):
//!
//! - `GET /concepts` — the catalog: every registered concept with its
//!   library, version, parameters, and data declaration. With `?symbol=` the
//!   per-concept availability on that instrument rides along (the channel
//!   rule), so the chart's explorer can grey out what cannot run instead of
//!   letting the trader click something that will never detect.
//! - `GET /concept-events` — the live event stream wrapped in the contract:
//!   the same events `/events` serves, projected through the registry so the
//!   chart, the thesis agent, and the skills read one shape.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::AppState;

/// `/concepts` query: the optional instrument for availability.
#[derive(Debug, Deserialize)]
pub struct ConceptsQuery {
    /// The instrument to compute availability against (bare = binance:spot).
    pub symbol: Option<String>,
}

/// One concept as the catalog serves it.
#[derive(Debug, Serialize)]
pub struct ConceptEntry {
    /// The catalog id.
    pub id: &'static str,
    /// The variant version.
    pub version: &'static str,
    /// Its library.
    pub library: market_data::concepts::Library,
    /// The display name.
    pub name: &'static str,
    /// The one-sentence definition.
    pub description: &'static str,
    /// The variant's parameters.
    pub parameters: &'static [market_data::concepts::ConceptParam],
    /// The channels it requires, as readable names.
    pub required_channels: Vec<&'static str>,
    /// Whether it can run on the queried symbol (absent without `?symbol=`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
}

/// `GET /concepts`: the catalog.
pub async fn concepts(
    State(_state): State<AppState>,
    Query(query): Query<ConceptsQuery>,
) -> Result<Json<Vec<ConceptEntry>>, ApiError> {
    let available: Option<Vec<&'static str>> = query
        .symbol
        .as_deref()
        .map(|symbol| {
            market_data::concepts::available_for(symbol)
                .into_iter()
                .map(|c| c.id)
                .collect()
        });
    let entries = market_data::concepts::concept_registry()
        .iter()
        .map(|c| ConceptEntry {
            id: c.id,
            version: c.version,
            library: c.library,
            name: c.name,
            description: c.description,
            parameters: c.parameters,
            required_channels: c.required_channels.names(),
            available: available.as_ref().map(|ids| ids.contains(&c.id)),
        })
        .collect();
    Ok(Json(entries))
}

/// `/concept-events` query.
#[derive(Debug, Deserialize)]
pub struct ConceptEventsQuery {
    /// The instrument (required).
    pub symbol: String,
    /// Cursor: only events at or after this nanosecond open time.
    pub since: Option<i64>,
}

/// The wrapped stream.
#[derive(Debug, Serialize)]
pub struct ConceptEventsResponse {
    /// The symbol served.
    pub symbol: String,
    /// The events, in the contract shape, chronological.
    pub events: Vec<market_data::concepts::ConceptEvent>,
}

/// `GET /concept-events`: the live stream in the contract shape.
pub async fn concept_events(
    State(state): State<AppState>,
    Query(query): Query<ConceptEventsQuery>,
) -> Result<Json<ConceptEventsResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    let lane = state.events.watch(&symbol, state.bots.history());
    let events = lane
        .since(query.since)
        .iter()
        .filter_map(market_data::concepts::as_concept_event)
        .collect();
    Ok(Json(ConceptEventsResponse { symbol, events }))
}

/// `POST /compositions/backtest` body.
#[derive(Debug, Deserialize)]
pub struct BacktestRequest {
    /// The composition document.
    pub document: market_data::composer::Composition,
    /// The instrument to replay.
    pub symbol: String,
    /// How many bars to replay (default 500).
    #[serde(default = "default_backtest_limit")]
    pub limit: usize,
}

fn default_backtest_limit() -> usize {
    500
}

/// One fired signal as the response reports it.
#[derive(Debug, Serialize)]
pub struct SignalReport {
    /// The bar index it fired on.
    pub bar: usize,
    /// Long or short.
    pub long: bool,
    /// The stop level (structure stops), when set.
    pub stop_level: Option<f64>,
    /// The satisfying concepts.
    pub evidence: Vec<String>,
}

/// `POST /compositions/backtest`: validate, replay, simulate, report.
pub async fn backtest(
    State(state): State<AppState>,
    crate::extract::ApiJson(request): crate::extract::ApiJson<BacktestRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use market_data::composer::{Composer, SizeKind, StopKind};

    let errors = market_data::composer::validate(&request.document);
    if let Err(errors) = errors {
        return Ok(Json(serde_json::json!({
            "valid": false,
            "errors": errors.iter().map(|e| format!("{e}")).collect::<Vec<_>>(),
        })));
    }
    let tf = request
        .document
        .timeframe
        .parse::<analytics_core::types::Timeframe>()
        .expect("validated above");
    let symbol = request.symbol.to_uppercase();
    let to_ns = crate::now_ns();
    let from_ns = to_ns - request.limit as i64 * tf.nanos();
    let window = state
        .windows
        .candles_capped(&symbol, tf, from_ns, to_ns, request.limit)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles for {symbol}: {e}")))?;
    let candles = window.candles;
    if candles.len() < 30 {
        return Err(ApiError::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "BACKTEST_WINDOW_THIN",
            format!("only {} bars for {symbol} {tf} -- a composition needs history to mean anything", candles.len()),
        ));
    }

    // Replay: per bar, detect on the trailing window, wrap, feed the state
    // machine. The engine's own config is the default (the composition's
    // parameters ride the registry's defaults in v1).
    let config = analytics_core::events::EventConfig::default();
    let mut composer = Composer::new(&request.document);
    let mut signals: Vec<market_data::composer::Signal> = Vec::new();
    let warmup = 22; // the structure lookback's own minimum window
    for bar in warmup..candles.len() {
        let raw = analytics_core::events::detect_events(&candles[..=bar], &symbol, &config);
        let events: Vec<_> = raw
            .iter()
            .filter(|e| e.bar_time == candles[bar].open_time)
            .filter_map(market_data::concepts::as_concept_event)
            .collect();
        if let Some(signal) = composer.on_bar(bar, &events) {
            signals.push(signal);
        }
    }

    // Signals become sim intents: entry at the signal bar (next-open fill,
    // like every other path), the structure stop from the satisfying level
    // or the ATR multiple.
    let atrs = analytics_core::indicators::atr(&candles, 14);
    let mut intents: Vec<(usize, pine_lite::interp::Intent)> = Vec::new();
    for signal in &signals {
        let stop = match &request.document.risk.stop {
            StopKind::Structure => signal.stop_level,
            StopKind::AtrMultiple { multiple } => {
                let entry_bar = (signal.bar + 1).min(candles.len() - 1);
                atrs[entry_bar].map(|atr| {
                    let entry = candles[entry_bar].open;
                    if signal.long { entry - multiple * atr } else { entry + multiple * atr }
                })
            }
        };
        intents.push((
            signal.bar,
            pine_lite::interp::Intent::Entry {
                id: "C".to_string(),
                long: signal.long,
                qty: match &request.document.risk.sizing {
                    SizeKind::Fixed { quantity } => Some(*quantity),
                    SizeKind::FixedFractional { .. } => None,
                },
            },
        ));
        if let Some(stop) = stop {
            intents.push((
                signal.bar,
                pine_lite::interp::Intent::Exit {
                    id: "C".to_string(),
                    stop: Some(stop),
                    limit: None,
                    trail_points: None,
                    trail_offset: None,
                    trail_price: None,
                    qty_percent: None,
                },
            ));
        }
    }
    let knobs = pine_lite::sim::StrategyHeader {
        default_qty_type: match &request.document.risk.sizing {
            SizeKind::FixedFractional { percent } => pine_lite::sim::QtyType::PercentOfEquity,
            SizeKind::Fixed { .. } => pine_lite::sim::QtyType::Fixed,
        },
        default_qty_value: match &request.document.risk.sizing {
            SizeKind::FixedFractional { percent } => *percent,
            SizeKind::Fixed { quantity } => *quantity,
        },
        // Instrument flavor stamps like every other path: Deriv symbols
        // backtest as contracts, not spot positions.
        contracts: market_data::broker::VenueSymbol::parse(&symbol).broker == "deriv",
        ..pine_lite::sim::StrategyHeader::default()
    };
    let simulation = pine_lite::sim::simulate(&intents, &candles, &knobs);

    Ok(Json(serde_json::json!({
        "valid": true,
        "name": request.document.name,
        "symbol": symbol,
        "timeframe": request.document.timeframe,
        "bars": candles.len(),
        "signals": signals.iter().map(|s| SignalReport {
            bar: s.bar,
            long: s.long,
            stop_level: s.stop_level,
            evidence: s.evidence.clone(),
        }).collect::<Vec<_>>(),
        "simulation": {
            "net_profit": simulation.report.net_profit,
            "total_trades": simulation.report.total_trades,
            "win_rate": simulation.report.win_rate,
            "profit_factor": simulation.report.profit_factor,
            "max_drawdown": simulation.report.max_drawdown,
        },
    })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalog_covers_the_seed_concepts_with_channel_names() {
        let entries: Vec<&str> = market_data::concepts::concept_registry()
            .iter()
            .map(|c| c.id)
            .collect();
        for expected in [
            "smc.bos",
            "smc.choch",
            "smc.liquidity_sweep",
            "smc.fvg",
            "sd.zone_tested",
            "volume.spike",
            "order_flow.delta_shift",
        ] {
            assert!(entries.contains(&expected), "{expected} registered");
        }
    }

    #[test]
    fn availability_matches_the_channel_rule_for_deriv_and_spot() {
        let deriv = market_data::concepts::available_for("DERIV:Volatility 75");
        let spot = market_data::concepts::available_for("BTCUSDT");
        assert!(deriv.len() < spot.len(), "synthetics serve fewer concepts");
        assert_eq!(
            spot.len() - deriv.len(),
            1,
            "exactly delta_shift separates them"
        );
    }
}
