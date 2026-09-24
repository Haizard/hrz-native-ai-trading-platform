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
//! ## The history is append-only, and records transitions
//!
//! Nothing here deletes. `venue_opt_ins` records the action, when, and an
//! optional reason; the current state is the newest row per venue. A boolean
//! column would have destroyed exactly the history an incident review needs.
//!
//! It records a *transition*, though, not a click. Repeating an opt-in for a
//! venue that is already enabled writes nothing and answers 200, because the
//! state cannot change and a second row would only make the trail ambiguous --
//! and because the `WARN` below asserts "live trading was enabled", which on a
//! repeat is simply false. Both the decision and the lock that makes it hold
//! under concurrent requests live in [`db::live::record_venue_opt_in`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trading_engine::credentials::var_names;
use trading_engine::GateRequirements;

use crate::auth::UserContext;
use crate::broker_routes;
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
    /// Whether **this deployment** holds its own credentials for the venue.
    ///
    /// Since `0007`, this is not what lets a user trade -- a live bot trades
    /// the broker account its owner connected. It is still reported because the
    /// deployment's own keys are what `xtask` and the CLI tools use, and an
    /// operator asking "is this box configured" needs the answer.
    ///
    /// Deliberately kept alongside [`VenueResponse::you_can_trade`] rather than
    /// replaced by it. The two failures look identical from outside -- "I opted
    /// in and it still refused" -- and the fixes are different and are on
    /// different pages: one is a checkbox here, the other is a broker account a
    /// user connects at `/brokers`.
    pub credentials_configured: bool,
    /// How many **verified** broker accounts the caller has on this venue.
    ///
    /// The user-level answer, and the one a venue panel should lead with: an
    /// operator's environment variable is not a user's account.
    pub your_accounts: usize,
    /// Whether the caller can start a live bot here right now.
    ///
    /// A derived field rather than one a client computes, because it is the
    /// question the UI actually asks and it depends on two facts on two pages
    /// (the opt-in and a verified account). A client deriving it is a client
    /// that goes out of step the day a third condition is added.
    pub you_can_trade: bool,
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
    let accounts = verified_accounts_by_venue(database, user.user_id).await?;
    Ok(Json(
        KNOWN_VENUES
            .iter()
            .map(|venue| describe(venue, &opted_in, &accounts))
            .collect(),
    ))
}

/// How many verified broker accounts the caller holds, per venue.
///
/// Read once for the whole list rather than once per venue: the list is a
/// handful of rows today and would be one query per row otherwise, which is the
/// shape that stays correct and gets slower every time a venue is added.
async fn verified_accounts_by_venue(
    database: &db::Database,
    user_id: Uuid,
) -> Result<std::collections::HashMap<String, usize>, ApiError> {
    let accounts = db::broker_accounts::list_broker_accounts(database.pool(), user_id).await?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for account in accounts {
        if broker_routes::may_trade(&account.status) {
            *counts.entry(account.venue).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

/// `POST /venues/{venue}/opt-in`
///
/// Idempotent: opting in to a venue that is already enabled changes nothing,
/// writes nothing, and says so at `INFO` rather than claiming an event.
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

    let changed = db::live::record_venue_opt_in(
        database.pool(),
        user.user_id,
        &venue,
        "opt_in",
        request.reason.as_deref(),
    )
    .await?;

    if changed {
        tracing::warn!(%venue, "live trading was enabled for a venue by an operator");
    } else {
        // Deliberately not a warning. This line used to fire unconditionally,
        // so a double-click -- two overlapping requests -- put two
        // "live trading was enabled" lines in the log for one decision, and the
        // log is what an incident review reads. The state is unchanged either
        // way; only the claim was wrong.
        tracing::info!(%venue, "live trading was already enabled for this venue");
    }

    let opted_in = db::live::opted_in_venues(database.pool(), user.user_id).await?;
    let accounts = verified_accounts_by_venue(database, user.user_id).await?;
    Ok(Json(describe(&venue, &opted_in, &accounts)))
}

/// `POST /venues/{venue}/revoke`
///
/// Idempotent in the same way as the opt-in, with one deliberate difference:
/// the kill switch runs whether or not the state changed. Revoking is the safe
/// direction, so a revoke that finds a live bot running on an already-revoked
/// venue must still stop it rather than reason that the trail already said so.
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

    let changed = db::live::record_venue_opt_in(
        database.pool(),
        user.user_id,
        &venue,
        "revoke",
        request.reason.as_deref(),
    )
    .await?;

    if changed {
        tracing::info!(%venue, "live trading was disabled for a venue by an operator");
    } else {
        tracing::info!(%venue, "live trading was already disabled for this venue");
    }

    // Acted on after being recorded, and unconditionally. The other order would
    // leave a window where a bot had been liquidated but the trail still said
    // the venue was enabled -- and the trail is what an incident review reads.
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
fn describe(
    venue: &str,
    opted_in: &[String],
    accounts: &std::collections::HashMap<String, usize>,
) -> VenueResponse {
    let requirements = GateRequirements::default();
    let opted_in_here = opted_in.iter().any(|v| v == venue);
    let your_accounts = accounts.get(venue).copied().unwrap_or(0);
    VenueResponse {
        venue: venue.to_string(),
        opted_in: opted_in_here,
        credentials_configured: credentials_present(venue),
        your_accounts,
        // Both conditions, and they are genuinely independent: consenting to a
        // venue without an account is a checkbox with nothing behind it, and
        // connecting an account without the opt-in is a key nothing will spend.
        you_can_trade: opted_in_here && your_accounts > 0,
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
///
/// The names come from [`var_names`] rather than being formatted here, so this
/// answer and the variables an order is actually signed with cannot drift
/// apart. Two derivations of the same name is the shape this repository keeps
/// finding: both compile, both look right, and the disagreement is visible only
/// in production.
#[must_use]
pub fn credentials_present(venue: &str) -> bool {
    let (key_var, secret_var) = var_names(venue);
    let present = |name: &str| {
        std::env::var(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    };
    present(&key_var) && present(&secret_var)
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
    fn every_known_venue_has_a_usable_environment_prefix() {
        // Derived from `var_names`, not re-formatted here, so the check covers
        // the function the rest of the system actually uses. The property is
        // that a venue added with a character that cannot appear in an
        // environment variable name would be permanently unconfigured and never
        // trade -- `GET /venues` would report `credentials_configured: false`
        // for a venue whose keys are set.
        for venue in KNOWN_VENUES {
            let (key_var, secret_var) = var_names(venue);
            for name in [key_var, secret_var] {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                    "{name} is not a usable environment variable name"
                );
            }
        }
    }

    /// The suffix pair, pinned.
    ///
    /// `docs/14`'s rule applied to the credential names: nothing checks a
    /// string against anything, so this is the only place a rename is caught.
    /// The deployment sets `BINANCE_API_KEY` because `docs/17` says so.
    #[test]
    fn the_credential_names_are_the_ones_the_deployment_sets() {
        assert_eq!(
            var_names("binance"),
            (
                "BINANCE_API_KEY".to_string(),
                "BINANCE_API_SECRET".to_string()
            )
        );
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
            your_accounts: 0,
            you_can_trade: false,
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
        assert_eq!(body["your_accounts"], 0);
        assert_eq!(body["you_can_trade"], false);
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
