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
    /// Whether it renders as an indicator sub-pane (the shell fetches its
    /// series and the engine positions it).
    pub pane: bool,
    /// Whether it renders as overlay lines on the price plot.
    pub overlay: bool,
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
            pane: c.pane,
            overlay: c.overlay,
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

/// `/concept-events/smt` query: a pair scan for SMT divergence.
#[derive(Debug, Deserialize)]
pub struct SmtQuery {
    /// Symbol A (the chart's instrument).
    pub symbol_a: String,
    /// Symbol B (the correlated market).
    pub symbol_b: String,
    /// The timeframe (required).
    pub timeframe: String,
    /// How many bars to compare (default 400).
    pub limit: Option<usize>,
}

/// `GET /concept-events/smt`: the pair scan.
///
/// SMT is a PAIR concept by construction -- it lives outside the per-symbol
/// engine stream, evaluated here over both series' swing structure. The
/// events carry A's divergent level (the chart's own price) with B's level
/// as the evidence of non-confirmation.
pub async fn smt(
    State(state): State<AppState>,
    Query(query): Query<SmtQuery>,
) -> Result<Json<ConceptEventsResponse>, ApiError> {
    let symbol_a = query.symbol_a.to_uppercase();
    let symbol_b = query.symbol_b.to_uppercase();
    let tf: analytics_core::types::Timeframe = query.timeframe.parse().map_err(|_| {
        ApiError::bad_request("TIMEFRAME_UNKNOWN", format!("`{}` is not a timeframe this platform serves", query.timeframe))
    })?;
    let limit = query.limit.unwrap_or(400).clamp(60, 1000);
    let to_ns = crate::now_ns();
    let from_ns = to_ns - limit as i64 * tf.nanos();
    let a = state
        .windows
        .candles_capped(&symbol_a, tf, from_ns, to_ns, limit)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles for {symbol_a}: {e}")))?
        .candles;
    let b = state
        .windows
        .candles_capped(&symbol_b, tf, from_ns, to_ns, limit)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles for {symbol_b}: {e}")))?
        .candles;
    let divergences = analytics_core::smt::detect_smt(&a, &b, 3);
    let events = divergences
        .iter()
        .map(|d| market_data::concepts::ConceptEvent {
            concept_id: "smc.smt_divergence".to_string(),
            concept_version: "1.0.0".to_string(),
            symbol: symbol_a.clone(),
            timeframe: query.timeframe.clone(),
            direction: match d.direction {
                analytics_core::types::Side::Buy => market_data::concepts::Direction::Bullish,
                analytics_core::types::Side::Sell => market_data::concepts::Direction::Bearish,
            },
            status: market_data::concepts::ConceptStatus::Confirmed,
            event_time_ns: d.bar_time,
            price_level: Some(d.a_level),
            zone_low: None,
            zone_high: None,
            evidence: serde_json::json!({
                "correlated_symbol": symbol_b,
                "b_level": d.b_level,
                "reading": "A swept the swing B did not confirm",
            }),
            bars: (d.prior_time, d.newest_time),
        })
        .collect();
    Ok(Json(ConceptEventsResponse { symbol: symbol_a, events }))
}

/// `/concept-events/scan` query: a full replay over the chart's own candles.
#[derive(Debug, Deserialize)]
pub struct ScanQuery {
    /// The instrument (required).
    pub symbol: String,
    /// The chart's timeframe (required: events are per-resolution).
    pub timeframe: String,
    /// How many bars to scan (default 500, capped at 1000).
    pub limit: Option<usize>,
}

/// `GET /concept-events/scan`: the explorer's backscan.
///
/// The live lane (`/concept-events`) only knows what the watcher recorded
/// SINCE it started watching -- a chart that opens on 500 candles of history
/// would show nothing until new bars closed. This route replays detection
/// over the chart's own window (the composer backtest's mechanism), so a
/// toggled concept renders across the candles already on screen.
pub async fn scan(
    State(state): State<AppState>,
    Query(query): Query<ScanQuery>,
) -> Result<Json<ConceptEventsResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    let tf: analytics_core::types::Timeframe = query.timeframe.parse().map_err(|_| {
        ApiError::bad_request("TIMEFRAME_UNKNOWN", format!("`{}` is not a timeframe this platform serves", query.timeframe))
    })?;
    let limit = query.limit.unwrap_or(500).clamp(50, 1000);
    let to_ns = crate::now_ns();
    let from_ns = to_ns - limit as i64 * tf.nanos();
    let window = state
        .windows
        .candles_capped(&symbol, tf, from_ns, to_ns, limit)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles for {symbol}: {e}")))?;
    let candles = window.candles;
    let config = analytics_core::events::EventConfig::default();
    let mut events: Vec<market_data::concepts::ConceptEvent> = Vec::new();
    // The replay: per bar, detect on the trailing window, keep only the
    // events that bar itself produced (the engine's own replay rule).
    for bar in 22..candles.len() {
        let raw = analytics_core::events::detect_events(&candles[..=bar], &symbol, &config);
        events.extend(
            raw.iter()
                .filter(|e| e.bar_time == candles[bar].open_time)
                .filter_map(market_data::concepts::as_concept_event),
        );
    }
    // Bounded PER CONCEPT, not globally: a dense family (a fib ladder emits
    // per bar) would otherwise push every sparse family (an RSI cross fires
    // a dozen times in 500 bars) out of the newest-N window entirely, and an
    // attached indicator would show no history exactly where it has some.
    let mut per_concept: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut kept: Vec<market_data::concepts::ConceptEvent> = Vec::new();
    for e in events.into_iter().rev() {
        let count = per_concept.entry(e.concept_id.clone()).or_insert(0);
        if *count < 50 {
            *count += 1;
            kept.push(e);
        }
    }
    kept.reverse();
    Ok(Json(ConceptEventsResponse {
        symbol,
        events: kept,
    }))
}

/// `/indicator-series` query: one pane concept's series over the chart's
/// own candles.
#[derive(Debug, Deserialize)]
pub struct IndicatorSeriesQuery {
    /// The instrument (required).
    pub symbol: String,
    /// The chart's timeframe (required).
    pub timeframe: String,
    /// The pane concept (`indicators.rsi` and friends; required).
    pub concept: String,
    /// How many bars to compute over (default 500, capped at 1000).
    pub limit: Option<usize>,
}

/// The series response: the spec the engine's generic series pane consumes,
/// plus the bars' open times so the shell aligns by timestamp and a mid-
/// flight bar close can never shift a line off its candle.
#[derive(Debug, Serialize)]
pub struct IndicatorSeriesResponse {
    /// The instrument.
    pub symbol: String,
    /// One open time per bar the values index into, oldest first.
    pub times: Vec<i64>,
    /// The pane spec (engine `SubPaneSpec::Series` field-for-field).
    pub spec: analytics_core::indicator_series::IndicatorSeries,
}

/// `GET /indicator-series`: a pane concept's measurements, computed by
/// analytics-core over the same window the chart draws.
pub async fn indicator_series(
    State(state): State<AppState>,
    Query(query): Query<IndicatorSeriesQuery>,
) -> Result<Json<IndicatorSeriesResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    let tf: analytics_core::types::Timeframe = query.timeframe.parse().map_err(|_| {
        ApiError::bad_request("TIMEFRAME_UNKNOWN", format!("`{}` is not a timeframe this platform serves", query.timeframe))
    })?;
    // The concept must be a registered pane concept: an unregistered id is a
    /// client bug, and a non-pane id is a category error, both 400s.
    let spec_entry = market_data::concepts::concept_registry()
        .iter()
        .find(|c| c.id == query.concept)
        .ok_or_else(|| ApiError::bad_request("CONCEPT_UNKNOWN", format!("`{}` is not a registered concept", query.concept)))?;
    if !spec_entry.pane && !spec_entry.overlay {
        return Err(ApiError::bad_request("CONCEPT_NOT_A_PANE", format!("`{}` has no indicator series", query.concept)));
    }
    let limit = query.limit.unwrap_or(500).clamp(50, 1000);
    let to_ns = crate::now_ns();
    let from_ns = to_ns - limit as i64 * tf.nanos();
    let window = state
        .windows
        .candles_capped(&symbol, tf, from_ns, to_ns, limit)
        .await
        .map_err(|e| ApiError::unavailable(format!("no candles for {symbol}: {e}")))?;
    let candles = window.candles;
    let spec = analytics_core::indicator_series::indicator_series(&query.concept, &candles)
        .ok_or_else(|| ApiError::bad_request("SERIES_UNAVAILABLE", format!("`{}` produced no series over this window", query.concept)))?;
    Ok(Json(IndicatorSeriesResponse {
        symbol,
        times: candles.iter().map(|c| c.open_time).collect(),
        spec,
    }))
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
    let config = market_data::composer::event_config_for(&request.document.parameters);
    let mut composer = Composer::new(request.document.clone());
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

/// `POST /compositions/generate` body: describe a strategy, get a
/// validated composition back (the AI-as-architect flow).
#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    /// The strategy in the user's words.
    pub description: String,
    /// The instrument it composes for (gates the catalog it may use).
    pub symbol: String,
}

/// `POST /compositions/generate`: the AI composition flow.
pub async fn generate(
    State(state): State<AppState>,
    user: crate::auth::UserContext,
    crate::extract::ApiJson(request): crate::extract::ApiJson<GenerateRequest>,
) -> Result<Json<crate::composition_codegen::GeneratedComposition>, ApiError> {
    crate::agent_routes::check_agent_limit(&state, &user)?;
    if request.description.trim().len() < 10 {
        return Err(ApiError::bad_request(
            "DESCRIPTION_THIN",
            "describe the strategy: what it waits for, in what order, and how it protects itself",
        ));
    }
    let symbol = request.symbol.to_uppercase();
    if market_data::concepts::available_for(&symbol).is_empty() {
        return Err(ApiError::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "VENUE_UNKNOWN",
            format!("no concepts are registered for `{symbol}` -- is the symbol spelled right?"),
        ));
    }
    let agent = crate::provider_routes::resolve_agent(&state, &user).await?;
    let generated = crate::composition_codegen::generate_composition(
        agent.llm().as_ref(),
        &symbol,
        &request.description,
        4096,
    )
    .await
    .map_err(|e| ApiError::unavailable(format!("the composition agent failed: {e}")))?;
    Ok(Json(generated))
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
