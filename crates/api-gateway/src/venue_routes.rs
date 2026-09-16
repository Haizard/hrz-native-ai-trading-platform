//! `/venues` — the per-venue opt-in that gates live trading (`docs/15`).
//!
//! ## Why this is its own resource rather than a field on the bot
//!
//! `docs/15` makes the opt-in **per venue, per account**, and revocable. A
//! field on a bot would make it per bot, which is a weaker promise in exactly
//! the case that matters: an operator who has lost confidence in a venue wants
//! to stop *everything* trading there, not to remember how many bots they
//! started.
//!
//! ## Revoking stops the bots that are already running
//!
//! The interesting half. A revoke that only changed what *future* bots may do
//! would leave every bot already running on that venue trading an account the
//! operator has just withdrawn consent for -- and "I revoked it and it kept
//! trading" is the failure this endpoint exists to prevent. So a revoke
//! throws the kill switch on every running live bot on that venue and reports
//! which ones.
//!
//! ## The history is append-only
//!
//! Nothing here deletes. `venue_opt_ins` records the action, when, and an
//! optional reason; the current state is the newest row per venue. A boolean
//! column would have destroyed exactly the history an incident review needs.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use trading_engine::GateRequirements;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// Venues the platform can actually trade.
///
/// A closed list rather than free text, because the value is compared against
/// what an adapter reports and a typo would produce a venue that is opted in
/// and can never be traded. Adding one is a code change with an adapter behind
/// it, which is the honest coupling.
pub const KNOWN_VENUES: [&str; 1] = ["binance"];

/// `POST /venues/{venue}/opt-in` and `/revoke`.
#[derive(Debug, Default, Deserialize)]
pub struct OptInRequest {
    /// Why. Recorded verbatim in the trail; optional because a UI checkbox
    /// should not require a paragraph.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One venue, as a client sees it.
#[derive(Debug, Serialize)]
pub struct VenueResponse {
    /// Lowercase venue name.
    pub venue: String,
    /// Whether live trading is enabled here.
    pub opted_in: bool,
    /// Whether this deployment holds credentials for it.
    ///
    /// Reported separately from `opted_in` because the two failures look
    /// identical from the outside -- "I opted in and it still refused" -- and
    /// the fix is different: one is a checkbox, the other is an environment
    /// variable on the API process.
    pub credentials_configured: bool,
    /// The gate's thresholds, so a UI can show how far along a strategy is
    /// without hardcoding them.
    pub requirements: RequirementResponse,
}

/// The gate's thresholds.
#[derive(Debug, Serialize)]
pub struct RequirementResponse {
    /// Minimum closed paper trades.
    pub min_paper_trades: usize,
    /// Minimum hours of paper trading.
    pub min_paper_hours: f64,
    /// How bad the paper result may be before going live is refused, in R.
    pub max_paper_loss_r: f64,
}

/// The result of a revoke, including what it stopped.
#[derive(Debug, Serialize)]
pub struct RevokeResponse {
    /// The venue.
    pub venue: String,
    /// It is off.
    pub opted_in: bool,
    /// Bots whose kill switch was thrown as a result.
    ///
    /// Empty is a normal answer and not a failure: it means nothing was running
    /// on that venue.
    pub bots_killed: Vec<String>,
}

/// `GET /venues`
///
/// # Errors
/// 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<VenueResponse>>, ApiError> {
    let database = database(&state)?;
    let opted_in = db::live::opted_in_venues(database.pool(), user.user_id).await?;
    Ok(Json(
        KNOWN_VENUES
            .iter()
            .map(|venue| describe(venue, &opted_in))
            .collect(),
    ))
}

/// `POST /venues/{venue}/opt-in`
///
/// # Errors
/// 404 for a venue with no adapter behind it; 503 without a database.
pub async fn opt_in(
    State(state): State<AppState>,
    user: UserContext,
    Path(venue): Path<String>,
    ApiJson(request): ApiJson<OptInRequest>,
) -> Result<Json<VenueResponse>, ApiError> {
    let database = database(&state)?;
    let venue = known(&venue)?;

    db::live::record_venue_opt_in(
        database.pool(),
        user.user_id,
        &venue,
        "opt_in",
        request.reason.as_deref(),
    )
    .await?;

    tracing::warn!(%venue, "live trading was enabled for a venue by an operator");

    let opted_in = db::live::opted_in_venues(database.pool(), user.user_id).await?;
    Ok(Json(describe(&venue, &opted_in)))
}

/// `POST /venues/{venue}/revoke`
///
/// # Errors
/// 404 for an unknown venue; 503 without a database.
pub async fn revoke(
    State(state): State<AppState>,
    user: UserContext,
    Path(venue): Path<String>,
    ApiJson(request): ApiJson<OptInRequest>,
) -> Result<Json<RevokeResponse>, ApiError> {
    let database = database(&state)?;
    let venue = known(&venue)?;

    db::live::record_venue_opt_in(
        database.pool(),
        user.user_id,
        &venue,
        "revoke",
        request.reason.as_deref(),
    )
    .await?;

    // Recorded first, acted on second. The other order would leave a window
    // where a bot had been liquidated but the trail still said the venue was
    // enabled -- and the trail is what an incident review reads.
    let killed = state.bots.kill_venue(&venue);
    if !killed.is_empty() {
        tracing::warn!(
            %venue,
            bots = killed.len(),
            "revoking a venue stopped the live bots running on it"
        );
        for bot_id in &killed {
            db::bots::set_status(database.pool(), user.user_id, *bot_id, "killed").await?;
        }
    }

    Ok(Json(RevokeResponse {
        venue,
        opted_in: false,
        bots_killed: killed.into_iter().map(|id| id.to_string()).collect(),
    }))
}

/// Build the description of one venue.
fn describe(venue: &str, opted_in: &[String]) -> VenueResponse {
    let requirements = GateRequirements::default();
    VenueResponse {
        venue: venue.to_string(),
        opted_in: opted_in.iter().any(|v| v == venue),
        credentials_configured: credentials_present(venue),
        requirements: RequirementResponse {
            min_paper_trades: requirements.min_paper_trades,
            min_paper_hours: requirements.min_paper_hours,
            max_paper_loss_r: requirements.max_paper_loss_r,
        },
    }
}

/// Whether this process holds credentials for a venue.
///
/// Checks for the *presence* of the variables, never their value. A boolean
/// derived from reading the secret is the only safe way to answer this over
/// HTTP: returning anything about the secret itself would be the leak.
#[must_use]
pub fn credentials_present(venue: &str) -> bool {
    let prefix = venue.to_ascii_uppercase();
    let has = |suffix: &str| {
        std::env::var(format!("{prefix}_{suffix}"))
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    };
    has("API_KEY") && has("API_SECRET")
}

/// Normalise and validate a venue name.
fn known(raw: &str) -> Result<String, ApiError> {
    let venue = raw.trim().to_ascii_lowercase();
    if KNOWN_VENUES.contains(&venue.as_str()) {
        return Ok(venue);
    }
    Err(ApiError::coded(
        StatusCode::NOT_FOUND,
        "VENUE_UNKNOWN",
        format!(
            "`{raw}` is not a venue this platform can trade. Known: {}",
            KNOWN_VENUES.join(", ")
        ),
    ))
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state
        .db
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("no database configured; venues cannot be managed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_venue_is_a_404_naming_the_known_ones() {
        let error = known("kraken").expect_err("must refuse");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "VENUE_UNKNOWN");
    }

    #[test]
    fn venue_names_are_normalised() {
        assert_eq!(known("  BINANCE ").expect("known"), "binance");
    }

    #[test]
    fn every_known_venue_has_a_capitalised_environment_prefix() {
        // The prefix is what `credentials_present` looks for, so a venue added
        // with a character that cannot appear in an environment variable name
        // would be permanently unconfigured and never trade.
        for venue in KNOWN_VENUES {
            let prefix = venue.to_ascii_uppercase();
            assert!(
                prefix.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "{prefix} is not a usable environment variable prefix"
            );
        }
    }

    /// The field names the live-trading panel reads.
    ///
    /// `docs/14`'s rule, and the reason it applies here: the shell renders this
    /// response and nothing checks a JSON key against anything. Renaming
    /// `credentials_configured` is not a compile error anywhere -- it is a
    /// panel that shows every venue as unconfigured, or, worse, `undefined`
    /// rendered into a `class="ok"` span so it reads as *configured*.
    #[test]
    fn the_venue_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(VenueResponse {
            venue: "binance".into(),
            opted_in: false,
            credentials_configured: false,
            requirements: RequirementResponse {
                min_paper_trades: 20,
                min_paper_hours: 48.0,
                max_paper_loss_r: -10.0,
            },
        })
        .expect("serializes");

        assert_eq!(body["venue"], "binance");
        assert_eq!(body["opted_in"], false);
        assert_eq!(body["credentials_configured"], false);
        assert_eq!(body["requirements"]["min_paper_trades"], 20);
        assert_eq!(body["requirements"]["min_paper_hours"], 48.0);
        assert_eq!(body["requirements"]["max_paper_loss_r"], -10.0);
    }

    /// The keys a revoke reports.
    ///
    /// `bots_killed` is what the panel counts to decide whether to re-read the
    /// bot list. A rename would make it count `undefined`, so a revoke that
    /// stopped three bots would silently leave the panel showing them running.
    #[test]
    fn the_revoke_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(RevokeResponse {
            venue: "binance".into(),
            opted_in: false,
            bots_killed: vec!["8f3a".into(), "9c21".into()],
        })
        .expect("serializes");

        assert_eq!(body["venue"], "binance");
        assert_eq!(body["opted_in"], false);
        assert_eq!(body["bots_killed"].as_array().map(Vec::len), Some(2));
    }
}
