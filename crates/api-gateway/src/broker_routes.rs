//! `/brokers` — a user's own exchange accounts (`docs/15-RISK-COMPLIANCE.md`).
//!
//! ## The shape of the product this route set exists for
//!
//! The platform is where analysis happens; the trading happens on the user's
//! account. So the user picks a broker the platform supports, hands over a key
//! scoped to trading, and from then on a live bot places *their* orders. Before
//! `0007`, "connect a venue" meant the operator set `BINANCE_API_KEY` on the
//! deployment, so every bot on the platform traded one account that belonged to
//! nobody in particular. This is the difference between a demo and a product.
//!
//! ## What happens to a key, and where it cannot go
//!
//! 1. It arrives in a request body and is sealed immediately, in this process,
//!    by [`trading_engine::SecretVault`].
//! 2. The plaintext exists for the length of one request and is then zeroized
//!    on drop. It is never logged, never returned, and never written to the
//!    audit trail -- `docs/15` is explicit that the trail must not contain one,
//!    and the failure it guards against is a `tracing::debug!` somebody adds
//!    while debugging.
//! 3. The ciphertext is bound to `(user_id, venue)`, so a row moved between
//!    users cannot be decrypted at all.
//!
//! The only thing that ever leaves this module about a key is *whether one
//! exists* and *whether it works*, which is what [`BrokerAccountResponse`]
//! carries.
//!
//! ## Verifying is not optional, and it refuses withdrawal keys
//!
//! [`connect`] checks the key against the venue before returning, because a key
//! that is merely stored produces a bot that starts, places nothing, and
//! reports a 401 into a log nobody is reading -- and the user's mental model is
//! that they are trading. `AccountCheck::refusals` is where the policy lives:
//! a key that cannot trade is useless, and a key that *can withdraw* is refused
//! outright. The platform places orders and never moves funds, so withdrawal
//! permission on a key another system holds is pure downside -- and `docs/15`
//! asks for trading-only scoping, which is enforceable here rather than merely
//! advised.
//!
//! ## Disconnect stops the bots that are already running
//!
//! The same reasoning as `venue_routes::revoke`, and the same order: record,
//! then act. A disconnect that only changed what *future* bots may do would
//! leave a live bot trading an account its owner has just withdrawn consent
//! for, with real orders, which is the failure this endpoint exists to prevent.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use trading_engine::{AccountCheck, BinanceRest, SealedCredentials, SecretScope};

use crate::AppState;
use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::venue_routes::KNOWN_VENUES;

/// The longest label a user may give an account.
///
/// The label is a column and appears in a bot list, so an unbounded one is both
/// an unbounded row and a UI that cannot lay out. 120 is past any real desk
/// name and short enough that a pasted document is refused rather than stored.
const MAX_LABEL: usize = 120;

/// The longest API key or secret accepted.
///
/// Not a validation of the exchange's format -- that is the venue's business and
/// it is checked against the venue. This is a bound on what one authenticated
/// request can make the process hold, which is the version of the problem this
/// server is responsible for.
const MAX_CREDENTIAL: usize = 512;

/// What the platform knows about a venue it can connect.
///
/// A closed list, keyed by the same lowercase names as
/// [`KNOWN_VENUES`], because the venus is compared against what an adapter
/// reports and free text would produce an account that is connected and can
/// never be traded. [`every_known_venue_is_described`] holds the two lists
/// together -- adding a venue to `KNOWN_VENUES` without a description here is
/// an account a user can opt into and cannot find in the picker.
///
/// [`every_known_venue_is_described`]: tests::every_known_venue_is_described
struct BrokerDescription {
    venue: &'static str,
    name: &'static str,
    /// Where the user creates a trading-only key.
    keys_url: &'static str,
    /// What to tell them while they are on that page.
    guidance: &'static str,
}

const SUPPORTED: [BrokerDescription; 1] = [BrokerDescription {
    venue: "binance",
    name: "Binance",
    keys_url: "https://www.binance.com/en/my/settings/api-management",
    guidance: "Create an API key with **Enable Reading** and **Enable Spot & Margin Trading** only. \
               Leave **Enable Withdrawals** off -- this platform places orders and never moves \
               funds, and a key that can withdraw is refused here.",
}];

/// `POST /brokers`
#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    /// Which broker. Must be one of [`KNOWN_VENUES`].
    pub venue: String,
    /// The user's name for this account, so two accounts on one venue are
    /// distinguishable in a bot list.
    pub label: String,
    /// The exchange API key. Read once, sealed, and never stored in plaintext.
    pub api_key: String,
    /// The exchange API secret. Same treatment.
    pub api_secret: String,
}

/// One account, as a client sees it.
///
/// There is deliberately no field that could hold a key or a secret. A response
/// type that cannot carry one is a response type that cannot leak one, whatever
/// a later edit to a handler does.
#[derive(Debug, Serialize)]
pub struct BrokerAccountResponse {
    /// The account id.
    pub id: String,
    /// Lowercase venue name.
    pub venue: String,
    /// The user's name for it.
    pub label: String,
    /// `pending`, `verified` or `invalid`.
    pub status: String,
    /// Whether this account may be used to start a live bot.
    ///
    /// Derived from `status == "verified"`, and sent as its own field because it
    /// is the question the UI actually asks -- "can I press Start?" -- and a
    /// client deriving it from a status string is a client that breaks the day a
    /// fourth status is added.
    pub may_trade: bool,
    /// What the venue said the key may do, verbatim, or `null` before a check.
    pub permissions: Value,
    /// When it was last checked against the venue, unix nanos.
    pub last_verified_at: Option<i64>,
    /// Why the last check failed. Never a credential.
    pub last_error: Option<String>,
    /// When it was connected, unix nanos.
    pub created_at: i64,
}

/// One broker the platform supports.
#[derive(Debug, Serialize)]
pub struct AvailableBrokerResponse {
    /// Lowercase venue name, which `POST /brokers` takes.
    pub venue: String,
    /// Display name.
    pub name: String,
    /// Where to create a key.
    pub keys_url: String,
    /// What to tick on that page, and what not to.
    pub guidance: String,
    /// Whether live trading needs the per-venue opt-in as well as an account.
    pub requires_venue_opt_in: bool,
}

/// The result of a disconnect.
#[derive(Debug, Serialize)]
pub struct DisconnectResponse {
    /// The account that is gone.
    pub id: String,
    /// Bots whose kill switch was thrown as a result.
    ///
    /// Empty is a normal answer and not a failure: it means nothing was running
    /// on that account.
    pub bots_killed: Vec<String>,
}

/// `GET /brokers`
///
/// # Errors
/// 503 without a database.
pub async fn list(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<BrokerAccountResponse>>, ApiError> {
    let database = database(&state)?;
    let rows = db::broker_accounts::list_broker_accounts(database.pool(), user.user_id).await?;
    Ok(Json(rows.iter().map(respond).collect()))
}

/// `GET /brokers/{id}`
///
/// # Errors
/// 404 when the account does not exist or belongs to someone else; 503 without a
/// database.
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BrokerAccountResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;
    let row = db::broker_accounts::get_broker_account(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such broker account"))?;
    Ok(Json(respond(&row)))
}

/// `GET /brokers/available`
///
/// The brokers a user may pick. Public in the sense that it names no user and
/// holds no secret, but behind a token like the rest of `/brokers`: a client
/// that has not signed in has nowhere to put the answer.
pub async fn available() -> Json<Vec<AvailableBrokerResponse>> {
    Json(
        KNOWN_VENUES
            .iter()
            .filter_map(|venue| describe(venue))
            .map(|description| AvailableBrokerResponse {
                venue: description.venue.to_string(),
                name: description.name.to_string(),
                keys_url: description.keys_url.to_string(),
                guidance: description.guidance.to_string(),
                // The opt-in is per venue and separate from the account, on
                // purpose: consent to trade a venue is not the same decision as
                // connecting a key, and revoking one should not require the
                // other.
                requires_venue_opt_in: true,
            })
            .collect(),
    )
}

/// `POST /brokers`
///
/// Seals the key, stores it, and then checks it against the venue. The order
/// matters: the row exists before the check, so a check that fails leaves an
/// account the user can see, diagnose and re-verify, rather than a request that
/// failed and told them nothing about where their key went.
///
/// # Errors
/// 404 for a venue with no adapter; 422 for a malformed or unusable key, naming
/// every reason at once; 503 without a database or without a vault configured.
pub async fn connect(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(request): ApiJson<ConnectRequest>,
) -> Result<(StatusCode, Json<BrokerAccountResponse>), ApiError> {
    let database = database(&state)?;
    let vault = vault(&state)?;

    let venue = known(&request.venue)?;
    let label = checked_label(&request.label)?;
    let (api_key, api_secret) = checked_credentials(&request.api_key, &request.api_secret)?;

    let scope = SecretScope::new(user.user_id, &venue);
    let credentials =
        trading_engine::ExchangeCredentials::from_parts(&venue, &api_key, &api_secret);
    let sealed = vault.seal_credentials(&scope, &credentials)?;

    let id = db::broker_accounts::create_broker_account(
        database.pool(),
        user.user_id,
        &db::broker_accounts::NewBrokerAccount {
            venue: &venue,
            label: &label,
            key_ciphertext: sealed.key_ciphertext,
            secret_ciphertext: sealed.secret_ciphertext,
            kek_fingerprint: &sealed.kek_fingerprint,
        },
    )
    .await?
    .ok_or_else(|| {
        ApiError::coded(
            StatusCode::CONFLICT,
            "BROKER_LABEL_TAKEN",
            format!("you already have a {venue} account labelled `{label}`"),
        )
    })?;

    // Logged without the key, the label, or the venue's answer. What an operator
    // needs from this line is that somebody connected an exchange account and
    // which of our rows it is; everything else is in the audit trail, which is
    // owner-scoped.
    tracing::info!(%id, %venue, %user.user_id, "a user connected a broker account");

    let outcome = check_and_record(&state, database, user.user_id, id, &venue, credentials).await;

    let row = db::broker_accounts::get_broker_account(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::internal("the account just stored could not be read back"))?;

    match outcome {
        // Stored, checked, and refused by the venue or by policy. This is a 422
        // with the account in the body's detail: the row exists and is visible
        // in `GET /brokers`, so answering 201 would claim a working account and
        // answering nothing would hide the diagnosis.
        Err(error) => Err(error.with_details(json!({
            "broker_account": respond(&row),
        }))),
        Ok(()) => Ok((StatusCode::CREATED, Json(respond(&row)))),
    }
}

/// `POST /brokers/{id}/verify`
///
/// Re-check a stored account against the venue. The key is opened, used, and
/// dropped; the only thing that changes is the row's status.
///
/// # Errors
/// 404 when the account is not the caller's; 422 when the venue refuses it, with
/// the account in the detail so a UI can render the state it just wrote.
pub async fn verify(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<BrokerAccountResponse>, ApiError> {
    let database = database(&state)?;
    let vault = vault(&state)?;
    let id = parse_id(&id)?;

    let stored = db::broker_accounts::broker_account_secrets(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such broker account"))?;

    let scope = SecretScope::new(user.user_id, &stored.venue);
    let credentials = vault.open_credentials(
        &scope,
        &SealedCredentials {
            key_ciphertext: stored.key_ciphertext,
            secret_ciphertext: stored.secret_ciphertext,
            kek_fingerprint: stored.kek_fingerprint,
        },
    )?;

    let venue = stored.venue.clone();
    check_and_record(&state, database, user.user_id, id, &venue, credentials).await?;

    let row = db::broker_accounts::get_broker_account(database.pool(), user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::internal("the account just checked could not be read back"))?;
    Ok(Json(respond(&row)))
}

/// `DELETE /brokers/{id}`
///
/// Kills the running bots that trade this account, then removes it. That order
/// is deliberate and the same as `venue_routes::revoke`: a disconnect that only
/// changed what *future* bots may do would leave a running bot placing real
/// orders on an account its owner has just withdrawn, and "I disconnected it and
/// it kept trading" is the failure this endpoint exists to prevent.
///
/// # Errors
/// 404 when the account does not exist or is not the caller's; 503 without a
/// database.
pub async fn disconnect(
    State(state): State<AppState>,
    user: UserContext,
    Path(id): Path<String>,
) -> Result<Json<DisconnectResponse>, ApiError> {
    let database = database(&state)?;
    let id = parse_id(&id)?;

    if db::broker_accounts::get_broker_account(database.pool(), user.user_id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("no such broker account"));
    }

    // Acted on before the delete, so the kill switch cannot be lost to a failed
    // delete. The other order would leave orders flowing for the length of a
    // database round trip after the owner asked for them to stop.
    let killed = state.bots.kill_broker_account(user.user_id, id);

    let removed =
        db::broker_accounts::delete_broker_account(database.pool(), user.user_id, id).await?;
    if !removed {
        // Another request removed it between the read and the delete. The bots
        // are already stopped, which is the safe direction, so this is a 404
        // rather than a rollback.
        return Err(ApiError::not_found("no such broker account"));
    }

    for bot_id in &killed {
        db::bots::set_status(database.pool(), user.user_id, *bot_id, "killed").await?;
    }

    tracing::info!(
        %id,
        %user.user_id,
        bots = killed.len(),
        "a user disconnected a broker account"
    );

    Ok(Json(DisconnectResponse {
        id: id.to_string(),
        bots_killed: killed.into_iter().map(|bot| bot.to_string()).collect(),
    }))
}

/// Check a key against its venue and write the answer down.
///
/// The one place a venue check happens, so `connect` and `verify` cannot drift
/// on what a refusal means or on what gets stored when one occurs.
async fn check_and_record(
    state: &AppState,
    database: &db::Database,
    user_id: Uuid,
    id: Uuid,
    venue: &str,
    credentials: trading_engine::ExchangeCredentials,
) -> Result<(), ApiError> {
    let check = match venue {
        "binance" => {
            BinanceRest::for_account(&state.binance_base_url, credentials)
                .verify_credentials()
                .await
        }
        // Unreachable while `known` and this match agree, and an error rather
        // than a panic because the day they stop agreeing should be a refused
        // connection, not a dead process holding a user's key.
        other => {
            return Err(ApiError::coded(
                StatusCode::NOT_IMPLEMENTED,
                "VENUE_CHECK_UNSUPPORTED",
                format!("there is no credential check for `{other}`"),
            ));
        }
    };

    let (status, permissions, error, refusals) = match check {
        Err(execution) => {
            let message = execution.to_string();
            ("invalid", json!({}), Some(message.clone()), vec![message])
        }
        Ok(check) => {
            let refusals = check.refusals();
            let status = if refusals.is_empty() {
                "verified"
            } else {
                "invalid"
            };
            (
                status,
                permissions_json(&check),
                refusals.first().cloned(),
                refusals,
            )
        }
    };

    db::broker_accounts::set_broker_account_status(
        database.pool(),
        user_id,
        id,
        status,
        &permissions,
        error.as_deref(),
    )
    .await?;

    if refusals.is_empty() {
        tracing::info!(%id, %venue, "a broker account was verified against the venue");
        return Ok(());
    }

    tracing::warn!(
        %id,
        %venue,
        // The reasons, which are ours and the venue's words -- never the key.
        reasons = %refusals.join("; "),
        "a broker account was refused"
    );

    Err(ApiError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "BROKER_CREDENTIALS_REFUSED",
        format!(
            "the {venue} account could not be used: {}",
            refusals.join("; ")
        ),
    ))
}

/// The venue's answer, as stored JSON. Never carries the key.
fn permissions_json(check: &AccountCheck) -> Value {
    json!({
        "can_trade": check.can_trade,
        "can_withdraw": check.can_withdraw,
        "can_deposit": check.can_deposit,
        "account_type": check.account_type,
        "permissions": check.permissions,
        "account_id": check.account_id,
        "maker_commission": check.maker_commission,
        "taker_commission": check.taker_commission,
    })
}

/// Build the client-facing view of a stored row.
fn respond(row: &db::broker_accounts::BrokerAccountRow) -> BrokerAccountResponse {
    BrokerAccountResponse {
        id: row.id.to_string(),
        venue: row.venue.clone(),
        label: row.label.clone(),
        status: row.status.clone(),
        may_trade: may_trade(&row.status),
        permissions: row.permissions.clone(),
        last_verified_at: row.last_verified_at,
        last_error: row.last_error.clone(),
        created_at: row.created_at,
    }
}

/// Whether a status permits starting a live bot.
///
/// One function, because it is asked in two places that must agree -- the
/// response body and `bot_routes::start_live` -- and a client told
/// `may_trade: true` by one and refused by the other is a bug report nobody can
/// reproduce.
#[must_use]
pub fn may_trade(status: &str) -> bool {
    status == "verified"
}

/// Look up a venue's description.
fn describe(venue: &str) -> Option<&'static BrokerDescription> {
    SUPPORTED.iter().find(|entry| entry.venue == venue)
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
            "`{raw}` is not a broker this platform can connect. Known: {}",
            KNOWN_VENUES.join(", ")
        ),
    ))
}

/// Validate a label.
fn checked_label(raw: &str) -> Result<String, ApiError> {
    let label = raw.trim();
    if label.is_empty() {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BROKER_LABEL_EMPTY",
            "a broker account needs a label: it is how you tell two accounts on one venue apart \
             in a bot list",
        ));
    }
    if label.chars().count() > MAX_LABEL {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BROKER_LABEL_TOO_LONG",
            format!("a label may be at most {MAX_LABEL} characters"),
        ));
    }
    Ok(label.to_string())
}

/// Validate a key/secret pair.
///
/// Both problems are reported together: a user fixing one field and being told
/// about the other on the next attempt is the round-trip-per-requirement shape
/// the live gate already avoids.
fn checked_credentials(api_key: &str, api_secret: &str) -> Result<(String, String), ApiError> {
    let mut problems: Vec<String> = Vec::new();
    let key = api_key.trim();
    let secret = api_secret.trim();

    if key.is_empty() {
        problems.push("`api_key` is empty".to_string());
    }
    if secret.is_empty() {
        problems.push("`api_secret` is empty".to_string());
    }
    if key.chars().count() > MAX_CREDENTIAL {
        problems.push(format!(
            "`api_key` is longer than {MAX_CREDENTIAL} characters"
        ));
    }
    if secret.chars().count() > MAX_CREDENTIAL {
        problems.push(format!(
            "`api_secret` is longer than {MAX_CREDENTIAL} characters"
        ));
    }

    if problems.is_empty() {
        return Ok((key.to_string(), secret.to_string()));
    }

    // The detail names the *fields*, never what was in them. A validation error
    // that echoed the value would put a key in a log the moment somebody pasted
    // the wrong thing into the wrong box.
    Err(ApiError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "BROKER_CREDENTIALS_INVALID",
        "the exchange credentials are not usable. Check the key and secret from your exchange's \
         API management page.",
    )
    .with_details(json!({ "problems": problems })))
}

/// Parse an account id from a path.
fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ApiError::coded(
            StatusCode::NOT_FOUND,
            "BROKER_NOT_FOUND",
            format!("`{raw}` is not a broker account id"),
        )
    })
}

fn database(state: &AppState) -> Result<&std::sync::Arc<db::Database>, ApiError> {
    state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable("no database configured; broker accounts cannot be managed")
    })
}

fn vault(state: &AppState) -> Result<&std::sync::Arc<trading_engine::SecretVault>, ApiError> {
    state.vault.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "BROKER_KEK is not set in this deployment, so no exchange credential can be stored \
             or read. Set it to 32 random bytes, base64-encoded.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_venue_is_a_404_naming_the_known_ones() {
        let error = known("kraken").expect_err("must refuse");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "VENUE_UNKNOWN");
        assert!(error.message().contains("binance"), "{}", error.message());
    }

    #[test]
    fn venue_names_are_normalised() {
        assert_eq!(known("  BINANCE ").expect("known"), "binance");
    }

    /// The two lists are one decision, held in two places.
    ///
    /// `KNOWN_VENUES` is what `/venues` and the opt-in accept; `SUPPORTED` is
    /// what the broker picker shows. A venue in the first and not the second is
    /// a venue a user can opt into trading and cannot connect an account to,
    /// which reads as a broken picker rather than a missing description.
    #[test]
    fn every_known_venue_is_described() {
        for venue in KNOWN_VENUES {
            let description =
                describe(venue).unwrap_or_else(|| panic!("{venue} has no entry in SUPPORTED"));
            assert!(!description.name.is_empty(), "{venue} has no display name");
            assert!(
                description.keys_url.starts_with("https://"),
                "{venue}'s keys_url is not a URL a user can follow: {}",
                description.keys_url
            );
            assert!(!description.guidance.is_empty(), "{venue} has no guidance");
        }
        assert_eq!(
            KNOWN_VENUES.len(),
            SUPPORTED.len(),
            "SUPPORTED describes a venue KNOWN_VENUES does not list, so it can never be reached"
        );
    }

    /// The refusal that makes `docs/15`'s trading-only scoping enforceable.
    #[test]
    fn a_key_that_can_withdraw_is_refused_even_when_it_can_trade() {
        let check = AccountCheck::from_body(&json!({
            "canTrade": true,
            "canWithdraw": true,
            "canDeposit": true,
            "accountType": "SPOT",
            "permissions": ["SPOT"],
        }));
        let refusals = check.refusals();
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert!(refusals[0].contains("withdrawal"), "{}", refusals[0]);
    }

    #[test]
    fn a_key_that_cannot_trade_is_refused() {
        let check = AccountCheck::from_body(&json!({
            "canTrade": false,
            "canWithdraw": false,
            "canDeposit": false,
        }));
        let refusals = check.refusals();
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert!(refusals[0].contains("trading enabled"), "{}", refusals[0]);
    }

    #[test]
    fn a_trading_only_key_is_accepted() {
        let check = AccountCheck::from_body(&json!({
            "canTrade": true,
            "canWithdraw": false,
            "canDeposit": true,
            "accountType": "SPOT",
            "permissions": ["SPOT"],
            "uid": 1234567,
        }));
        assert!(check.refusals().is_empty(), "{:?}", check.refusals());
        assert_eq!(check.account_id.as_deref(), Some("1234567"));
        assert_eq!(check.account_type.as_deref(), Some("SPOT"));
    }

    /// A body that omits a permission grants nothing.
    ///
    /// The failure this prevents: a venue changing its response shape and this
    /// build reading the missing field as *allowed*. For a check that gates real
    /// money the only safe default is the refusing one.
    #[test]
    fn a_missing_permission_flag_fails_closed() {
        let check = AccountCheck::from_body(&json!({}));
        assert!(!check.can_trade);
        assert!(!check.can_withdraw);
        assert_eq!(check.refusals().len(), 1, "an unknown body must not verify");
    }

    #[test]
    fn a_label_is_required_and_bounded() {
        let empty = checked_label("   ").expect_err("must refuse");
        assert_eq!(empty.code(), "BROKER_LABEL_EMPTY");

        let long = "x".repeat(MAX_LABEL + 1);
        let error = checked_label(&long).expect_err("must refuse");
        assert_eq!(error.code(), "BROKER_LABEL_TOO_LONG");

        assert_eq!(checked_label("  main  ").expect("ok"), "main");
    }

    #[test]
    fn credentials_are_not_echoed_back_in_a_validation_error() {
        // The failure this prevents: `api_key` pasted into the secret box, and
        // the validation message repeating it into a log.
        let error = checked_credentials("", "SECRET-should-not-appear").expect_err("must refuse");
        assert_eq!(error.code(), "BROKER_CREDENTIALS_INVALID");
        assert!(
            !error.message().contains("SECRET-should-not-appear"),
            "{}",
            error.message()
        );

        // The detail carries field *names*, and the test proves it by checking
        // the rendered detail rather than trusting the comment above: the field
        // that was wrong is named, and the value that was sent is not present
        // anywhere in it.
        let rendered = serde_json::to_string(error.details().expect("a detail")).expect("json");
        assert!(!rendered.contains("SECRET-should-not-appear"), "{rendered}");
        assert!(rendered.contains("api_key"), "{rendered}");
    }

    #[test]
    fn both_credential_problems_are_reported_at_once() {
        let error = checked_credentials("", "").expect_err("must refuse");
        let details = error.details().expect("a detail");
        let problems = details["problems"].as_array().expect("an array");
        assert_eq!(problems.len(), 2, "{details}");
    }

    #[test]
    fn may_trade_agrees_with_the_status_the_check_writes() {
        // The pair that must not drift: `connect` writes `verified` and the
        // response says `may_trade: true`. A client told it may trade and then
        // refused by `start_live` is a bug report nobody can reproduce.
        assert!(may_trade("verified"));
        assert!(!may_trade("pending"));
        assert!(!may_trade("invalid"));
        for status in db::broker_accounts::STATUSES {
            assert_eq!(
                may_trade(status),
                status == "verified",
                "may_trade disagrees with the status vocabulary for `{status}`"
            );
        }
    }

    #[test]
    fn a_bad_id_is_a_404_and_not_a_500() {
        let error = parse_id("not-a-uuid").expect_err("must refuse");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }

    /// The keys a client reads.
    ///
    /// `docs/14`'s rule, and the same reason `venue_routes` pins its own: the
    /// shell renders this response and nothing checks a JSON key against
    /// anything. A renamed `may_trade` is not a compile error anywhere -- it is
    /// a Start button that stays disabled for every verified account.
    #[test]
    fn the_broker_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(BrokerAccountResponse {
            id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
            venue: "binance".into(),
            label: "main".into(),
            status: "verified".into(),
            may_trade: true,
            permissions: json!({"can_trade": true, "can_withdraw": false}),
            last_verified_at: Some(1),
            last_error: None,
            created_at: 2,
        })
        .expect("serializes");

        assert_eq!(body["venue"], "binance");
        assert_eq!(body["label"], "main");
        assert_eq!(body["status"], "verified");
        assert_eq!(body["may_trade"], true);
        assert_eq!(body["permissions"]["can_withdraw"], false);
        assert_eq!(body["last_verified_at"], 1);
        assert_eq!(body["created_at"], 2);
        // And there is no field a key could be in, whatever a later edit does.
        let rendered = body.to_string();
        assert!(!rendered.contains("ciphertext"), "{rendered}");
        assert!(!rendered.contains("api_secret"), "{rendered}");
    }

    #[test]
    fn the_available_response_pins_the_keys_the_picker_reads() {
        let body = serde_json::to_value(AvailableBrokerResponse {
            venue: "binance".into(),
            name: "Binance".into(),
            keys_url: "https://example.invalid/keys".into(),
            guidance: "trading only".into(),
            requires_venue_opt_in: true,
        })
        .expect("serializes");

        assert_eq!(body["venue"], "binance");
        assert_eq!(body["name"], "Binance");
        assert_eq!(body["keys_url"], "https://example.invalid/keys");
        assert_eq!(body["requires_venue_opt_in"], true);
    }

    #[test]
    fn the_disconnect_response_pins_the_keys_the_panel_reads() {
        let body = serde_json::to_value(DisconnectResponse {
            id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
            bots_killed: vec!["8f3a".into()],
        })
        .expect("serializes");

        assert_eq!(body["id"], "0f8fad5b-d9cb-469f-a165-70867728950e");
        assert_eq!(body["bots_killed"].as_array().map(Vec::len), Some(1));
    }
}
