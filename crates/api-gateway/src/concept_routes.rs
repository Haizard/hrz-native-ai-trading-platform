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
