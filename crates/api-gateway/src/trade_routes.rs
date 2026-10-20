//! Manual Deriv trading: a contract without a bot.
//!
//! Everything in `broker_routes` and `bot_routes` assumes orders are born in
//! a strategy loop; this module is the other door. A trader clicks buy or
//! sell on a Deriv chart, and the platform turns that click into the exact
//! contract the bot path would have placed: the same OTP gateway, the same
//! `Deriv-App-ID` attribution tag, the same sealed-token discipline.
//!
//! Three routes:
//!
//! - `POST /trade/order` — `{symbol, side, stake, multiplier?}` buys a
//!   `MULTUP`/`MULTDOWN` at the venue's ask (bounded slippage), gated by the
//!   venue opt-in like every live order path.
//! - `GET /trade/portfolio` — the open contracts and their live PnL.
//! - `POST /trade/sell` — an early exit on one contract.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;
use trading_engine::ExchangeAdapter as _;

/// The leverage ladder Deriv offers on multipliers.
pub const MULTIPLIER_LADDER: [f64; 5] = [10.0, 20.0, 50.0, 100.0, 200.0];

/// A sane manual-order cap: no click should risk more than this by accident.
pub const MAX_MANUAL_STAKE: f64 = 10_000.0;

/// `POST /trade/order` body.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderRequest {
    /// The namespaced Deriv symbol (`DERIV:Volatility 75`).
    pub symbol: String,
    /// `buy` (MULTUP) or `sell` (MULTDOWN).
    pub side: String,
    /// Stake in account currency.
    pub stake: f64,
    /// Leverage from the ladder; defaults to the adapter's (env or 50).
    pub multiplier: Option<f64>,
    /// Optional client idempotency key: a browser retry with the same key
    /// returns the first contract instead of buying twice.
    pub idempotency_key: Option<String>,
}

/// What a placed manual order reports back.
#[derive(Debug, Serialize)]
pub struct OrderResponse {
    /// The venue's contract id.
    pub contract_id: u64,
    /// Our client order id (deterministic from the idempotency key when given).
    pub client_order_id: String,
    /// The ask price paid.
    pub price: f64,
    /// Stake in account currency.
    pub stake: f64,
    /// Leverage used.
    pub multiplier: f64,
    /// The contract type (`MULTUP`/`MULTDOWN`).
    pub contract_type: String,
}

/// One open contract on the portfolio route.
#[derive(Debug, Serialize)]
pub struct PortfolioEntry {
    /// The venue's contract id.
    pub contract_id: u64,
    /// Live mark-to-market profit, when the venue reports it.
    pub profit: Option<f64>,
    /// `1` when the venue accepts an early sell now.
    pub is_sold: Option<f64>,
}

/// `POST /trade/sell` body.
#[derive(Debug, Deserialize)]
pub struct SellRequest {
    /// The venue's contract id to close early.
    pub contract_id: u64,
}

/// What a sell reports back.
#[derive(Debug, Serialize)]
pub struct SellResponse {
    /// What the venue paid for the early exit.
    pub sold_for: f64,
}

/// The shared validation of a manual order's shape: everything that can be
/// wrong before an account is touched, with the order-id mapping the rest of
/// the route relies on. Kept separate so the rules are testable without a
/// venue or a database.
pub(crate) fn checked_order(
    request: &OrderRequest,
) -> Result<(trading_engine::OrderRequest, &'static str, f64), ApiError> {
    let side = match request.side.as_str() {
        "buy" => trading_engine::OrderSide::Buy,
        "sell" => trading_engine::OrderSide::Sell,
        other => {
            return Err(ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "TRADE_SIDE_UNKNOWN",
                format!("`{other}` is not a side: buy or sell"),
            ));
        }
    };
    let code = ::market_data::deriv::code_for_namespaced(&request.symbol).ok_or_else(|| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VENUE_UNKNOWN",
            format!(
                "`{}` is not a Deriv instrument this platform trades",
                request.symbol
            ),
        )
    })?;
    if !(request.stake.is_finite() && request.stake > 0.0) {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TRADE_STAKE_INVALID",
            "stake must be a positive number, in account currency",
        ));
    }
    if request.stake > MAX_MANUAL_STAKE {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TRADE_STAKE_TOO_LARGE",
            format!("a manual order stakes at most {MAX_MANUAL_STAKE} -- larger size goes through a bot's risk engine"),
        ));
    }
    let multiplier = match request.multiplier {
        None => 0.0, // the adapter's default (env or 50)
        Some(m) if MULTIPLIER_LADDER.contains(&m) => m,
        Some(m) => {
            return Err(ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "TRADE_MULTIPLIER_UNKNOWN",
                format!(
                    "multiplier {m} is not on Deriv's ladder: {}",
                    MULTIPLIER_LADDER
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    };
    let client_order_id = request
        .idempotency_key
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let order = trading_engine::OrderRequest {
        client_order_id,
        symbol: request.symbol.to_uppercase(),
        side,
        quantity: request.stake,
        order_type: trading_engine::OrderType::Market,
    };
    Ok((order, code, multiplier))
}

/// Resolve the user's verified Deriv account and open its credentials, or the
/// named refusal for each way that cannot hold. The plaintext token never
/// leaves the returned adapter.
async fn deriv_adapter_for(
    state: &AppState,
    database: &db::Database,
    user: &UserContext,
    code: &'static str,
    multiplier: f64,
) -> Result<trading_engine::deriv::DerivAdapter, ApiError> {
    let vault = state.vault.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "BROKER_KEK is not set in this deployment, so no stored broker credential can be opened",
        )
    })?;
    let accounts = db::broker_accounts::list_broker_accounts(database.pool(), user.user_id).await?;
    let row = accounts
        .into_iter()
        .find(|row| row.venue == "deriv" && row.status == "verified")
        .ok_or_else(|| {
            ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BROKER_NOT_CONNECTED",
                "no verified Deriv account on your profile -- connect one under Brokers first",
            )
        })?;
    let loginid = row
        .permissions
        .get("account_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BROKER_ACCOUNT_INCOMPLETE",
                "this Deriv account has no loginid recorded; disconnect and reconnect it",
            )
        })?;
    let stored = db::broker_accounts::broker_account_secrets(database.pool(), user.user_id, row.id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such broker account"))?;
    let scope = trading_engine::SecretScope::new(user.user_id, &stored.venue);
    let credentials = vault
        .open_credentials(
            &scope,
            &trading_engine::SealedCredentials {
                key_ciphertext: stored.key_ciphertext,
                secret_ciphertext: stored.secret_ciphertext,
                kek_fingerprint: stored.kek_fingerprint,
            },
        )
        .map_err(|e| ApiError::unavailable(format!("the stored credential could not be opened: {e}")))?;
    let mut adapter = trading_engine::deriv::DerivAdapter::new(
        ::market_data::deriv_trade::DerivTradingGateway::from_env(),
        loginid,
        credentials.key().to_string(),
        code,
        "USD",
    );
    if multiplier > 0.0 {
        adapter = adapter.with_leverage(multiplier);
    }
    Ok(adapter)
}

/// The opt-in check: manual orders ride the same consent list as bot orders.
async fn require_opt_in(
    state: &AppState,
    database: &db::Database,
    user: &UserContext,
) -> Result<(), ApiError> {
    let _ = state;
    let venues = db::live::opted_in_venues(database.pool(), user.user_id).await?;
    if venues.iter().any(|v| v == "deriv") {
        return Ok(());
    }
    Err(ApiError::coded(
        StatusCode::FORBIDDEN,
        "VENUE_NOT_OPTED_IN",
        "opt into Deriv trading first (the venue toggle under Brokers) -- manual orders ride the same consent as bot orders",
    ))
}

/// `POST /trade/order`: a manual multiplier contract.
pub async fn order(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(request): ApiJson<OrderRequest>,
) -> Result<(StatusCode, Json<OrderResponse>), ApiError> {
    let database = crate::bot_routes::database(&state)?;
    require_opt_in(&state, database, &user).await?;
    let (order, code, multiplier) = checked_order(&request)?;
    let adapter = deriv_adapter_for(&state, database, &user, code, multiplier).await?;
    let ack = adapter.place_order(order.clone()).await.map_err(|e| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TRADE_ORDER_REFUSED",
            format!("the venue refused the order: {e}"),
        )
    })?;
    let multiplier_used = adapter.leverage();
    let (contract_id, contract_type) = match ack.status {
        _ if order.side == trading_engine::OrderSide::Buy => {
            (ack.exchange_order_id.parse().unwrap_or(0), "MULTUP")
        }
        _ => (ack.exchange_order_id.parse().unwrap_or(0), "MULTDOWN"),
    };
    Ok((
        StatusCode::CREATED,
        Json(OrderResponse {
            contract_id,
            client_order_id: ack.client_order_id,
            price: ack.avg_price.unwrap_or_default(),
            stake: request.stake,
            multiplier: multiplier_used,
            contract_type: contract_type.to_string(),
        }),
    ))
}

/// `GET /trade/portfolio`: open contracts with live PnL.
pub async fn portfolio(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Vec<PortfolioEntry>>, ApiError> {
    let database = crate::bot_routes::database(&state)?;
    // Any Deriv instrument works for a portfolio read; the gateway is
    // account-scoped, not symbol-scoped.
    let adapter = deriv_adapter_for(&state, database, &user, "R_75", 0.0).await?;
    let entries = adapter.portfolio().await.map_err(|e| {
        ApiError::unavailable(format!("the venue's portfolio is unreachable: {e}"))
    })?;
    Ok(Json(
        entries
            .into_iter()
            .map(|c| PortfolioEntry {
                contract_id: c.contract_id,
                profit: c.profit,
                is_sold: c.is_sold,
            })
            .collect(),
    ))
}

/// `POST /trade/sell`: an early exit on one contract.
pub async fn sell(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(request): ApiJson<SellRequest>,
) -> Result<Json<SellResponse>, ApiError> {
    let database = crate::bot_routes::database(&state)?;
    let adapter = deriv_adapter_for(&state, database, &user, "R_75", 0.0).await?;
    let sold_for = adapter.sell(request.contract_id, 0.0).await.map_err(|e| {
        ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TRADE_SELL_REFUSED",
            format!("the venue refused the sell: {e}"),
        )
    })?;
    Ok(Json(SellResponse { sold_for }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_order_maps_to_the_engine_shape() {
        let request = OrderRequest {
            symbol: "DERIV:Volatility 75".to_string(),
            side: "buy".to_string(),
            stake: 25.0,
            multiplier: Some(100.0),
            idempotency_key: Some("ui-abc".to_string()),
        };
        let (order, code, multiplier) = checked_order(&request).expect("valid");
        assert_eq!(code, "R_75");
        assert_eq!(multiplier, 100.0);
        assert_eq!(order.client_order_id, "ui-abc");
        assert_eq!(order.quantity, 25.0);
        assert_eq!(order.symbol, "DERIV:VOLATILITY 75");
        assert!(matches!(order.order_type, trading_engine::OrderType::Market));
    }

    #[test]
    fn bad_sides_symbols_stakes_and_multipliers_are_named() {
        let base = OrderRequest {
            symbol: "DERIV:Volatility 75".to_string(),
            side: "buy".to_string(),
            stake: 25.0,
            multiplier: None,
            idempotency_key: None,
        };
        let err = checked_order(&OrderRequest { side: "up".to_string(), ..base.clone() }).expect_err("side");
        assert!(err.message().contains("buy or sell"), "{}", err.message());

        let err = checked_order(&OrderRequest {
            symbol: "BTCUSDT".to_string(),
            ..base.clone()
        })
        .expect_err("symbol");
        assert!(err.message().contains("not a Deriv instrument"), "{}", err.message());

        let err = checked_order(&OrderRequest { stake: 0.0, ..base.clone() }).expect_err("stake");
        assert!(err.message().contains("positive"), "{}", err.message());

        let err = checked_order(&OrderRequest {
            stake: MAX_MANUAL_STAKE + 1.0,
            ..base.clone()
        })
        .expect_err("cap");
        assert!(err.message().contains("at most"), "{}", err.message());

        let err = checked_order(&OrderRequest {
            multiplier: Some(75.0),
            ..base.clone()
        })
        .expect_err("ladder");
        assert!(err.message().contains("ladder"), "{}", err.message());

        // The adapter default rides through as 0.0 (env or 50 decides).
        let (_, _, multiplier) = checked_order(&base.clone()).expect("valid");
        assert_eq!(multiplier, 0.0);
        // And a fresh uuid fills the idempotency gap.
        let (order, _, _) = checked_order(&base.clone()).expect("valid");
        assert!(!order.client_order_id.is_empty());
    }
}
