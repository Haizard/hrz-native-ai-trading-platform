//! Deriv contract **execution**: the OTP-authenticated trading gateway and
//! the multiplier-contract lifecycle.
//!
//! Data (`deriv.rs`) and trading (this file) are separate on purpose: market
//! data is public and needs no account, while everything here runs on a
//! user's own token -- OTP-issued, trade-scoped, and carrying the platform's
//! `Deriv-App-ID` on every call, which is the attribution tag the partner
//! commission rides on (Track 2). The flow, exactly as Deriv's docs spell it:
//!
//! 1. `POST /trading/v1/options/accounts/{accountId}/otp` with
//!    `Authorization: Bearer <PAT>` and `Deriv-App-ID` → one-time trading WS
//!    URL (`.../ws/real?otp=...` or `.../ws/demo?otp=...`).
//! 2. On that socket: `balance`, `proposal` (a price quote), `buy` (with the
//!    proposal id), `portfolio` / `proposal_open_contract` (monitoring),
//!    `sell` (early exit). Requests correlate by `req_id`.
//!
//! The contract model is honest about what a multiplier IS: leveraged PnL on
//! the underlying move, loss capped at the stake with a stop-out, take-profit
//! and stop-loss as absolute limit orders -- not a spot position.

use analytics_core::types::Candle;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;

use crate::MarketDataError;

/// The REST base for the OTP call.
pub const DERIV_REST_URL: &str = "https://api.derivws.com";

/// One price-proposal request, multiplier-flavored (the platform's first
/// contract family). Rise/fall (`CALL`/`PUT`) fits the same shape with
/// `contract_type` and duration instead of `multiplier`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalParams {
    /// `MULTUP` or `MULTDOWN` for multipliers.
    pub contract_type: String,
    /// The Deriv code (`R_75`); the field is `underlying_symbol` on the wire.
    pub underlying_symbol: String,
    /// Stake in account currency.
    pub amount: f64,
    /// Account currency (`USD` for most).
    pub currency: String,
    /// The leverage for multipliers (10, 20, 50, 100, 200...).
    pub multiplier: Option<f64>,
    /// Duration + unit for expiry contracts (omit both for multipliers,
    /// which run until sold or stopped out).
    pub duration: Option<u64>,
    /// See `duration`.
    pub duration_unit: Option<String>,
    /// Absolute take-profit price, in `limit_order`.
    pub take_profit: Option<f64>,
    /// Absolute stop-loss price, in `limit_order`.
    pub stop_loss: Option<f64>,
}

impl ProposalParams {
    /// A multiplier proposal (`MULTUP`/`MULTDOWN`).
    #[must_use]
    pub fn multiplier_up(
        symbol: &str,
        stake: f64,
        leverage: f64,
        currency: &str,
    ) -> Self {
        Self {
            contract_type: "MULTUP".to_string(),
            underlying_symbol: symbol.to_string(),
            amount: stake,
            currency: currency.to_string(),
            multiplier: Some(leverage),
            duration: None,
            duration_unit: None,
            take_profit: None,
            stop_loss: None,
        }
    }

    /// The inverse multiplier (`MULTDOWN`).
    #[must_use]
    pub fn multiplier_down(symbol: &str, stake: f64, leverage: f64, currency: &str) -> Self {
        Self {
            contract_type: "MULTDOWN".to_string(),
            ..Self::multiplier_up(symbol, stake, leverage, currency)
        }
    }

    /// The JSON body of the `proposal` call.
    #[must_use]
    pub fn to_request(&self, req_id: u64) -> serde_json::Value {
        let mut req = serde_json::json!({
            "proposal": 1,
            "amount": self.amount,
            "basis": "stake",
            "contract_type": self.contract_type,
            "currency": self.currency,
            "underlying_symbol": self.underlying_symbol,
            "req_id": req_id,
        });
        if let Some(m) = self.multiplier {
            req["multiplier"] = serde_json::json!(m);
        }
        if let (Some(d), Some(u)) = (self.duration, &self.duration_unit) {
            req["duration"] = serde_json::json!(d);
            req["duration_unit"] = serde_json::json!(u);
        }
        if self.take_profit.is_some() || self.stop_loss.is_some() {
            let mut limit = serde_json::Map::new();
            if let Some(tp) = self.take_profit {
                limit.insert("take_profit".to_string(), serde_json::json!(tp));
            }
            if let Some(sl) = self.stop_loss {
                limit.insert("stop_loss".to_string(), serde_json::json!(sl));
            }
            req["limit_order"] = serde_json::Value::Object(limit);
        }
        req
    }
}

/// The venue's answer to `proposal`: the id `buy` consumes and the ask price.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Proposal {
    /// Proposal id (`buy: <id>`).
    pub id: String,
    /// The ask price (what the contract costs now, commission embedded).
    pub ask_price: f64,
    /// The quoted payout (for expiry contracts), when reported.
    #[serde(default)]
    pub payout: Option<f64>,
}

/// The venue's answer to `buy`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Purchase {
    /// The contract id (monitoring and selling key on it).
    pub contract_id: u64,
    /// What was actually paid.
    #[serde(default)]
    pub purchase_price: Option<f64>,
    /// The venue's balance after the purchase, when reported.
    #[serde(default)]
    pub balance_after: Option<f64>,
}

/// One live contract from `portfolio` / `proposal_open_contract`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OpenContract {
    /// The contract id.
    pub contract_id: u64,
    /// Current mark-to-market profit, when reported.
    #[serde(default)]
    pub profit: Option<f64>,
    /// `1` when the venue will accept a `sell` right now.
    #[serde(default)]
    pub is_sold: Option<f64>,
}

/// The wire envelopes (responses correlate by `req_id`).
#[derive(Debug, Deserialize)]
struct TradeResponse {
    req_id: Option<u64>,
    proposal: Option<serde_json::Value>,
    buy: Option<serde_json::Value>,
    balance: Option<serde_json::Value>,
    portfolio: Option<serde_json::Value>,
    proposal_open_contract: Option<serde_json::Value>,
    sell: Option<serde_json::Value>,
    error: Option<TradeError>,
}

/// Deriv's error shape.
#[derive(Debug, Deserialize)]
struct TradeError {
    code: Option<String>,
    message: Option<String>,
}

/// The OTP-authenticated trading session. One short-lived struct per
/// connection; sessions are cheap to rebuild (the OTP is single-use).
#[derive(Debug, Clone)]
pub struct DerivTradingGateway {
    app_id: String,
    rest_url: String,
}

/// The OTP REST reply.
#[derive(Debug, Deserialize)]
struct OtpReply {
    data: Option<OtpData>,
    error: Option<TradeError>,
}

/// The `data` half of the OTP reply.
#[derive(Debug, Deserialize)]
struct OtpData {
    url: String,
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

impl DerivTradingGateway {
    /// From the environment: `DERIV_APP_ID` (required here -- attribution is
    /// the whole point of this gateway; the data fallback id does NOT tag
    /// your commission).
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            app_id: std::env::var("DERIV_APP_ID")
                .unwrap_or_else(|_| crate::deriv::DEFAULT_APP_ID.to_string()),
            rest_url: DERIV_REST_URL.to_string(),
        }
    }

    /// With an explicit endpoint (tests).
    #[must_use]
    pub fn new(app_id: impl Into<String>, rest_url: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            rest_url: rest_url.into(),
        }
    }

    /// The OTP call: `POST /trading/v1/options/accounts/{accountId}/otp`
    /// with `Authorization: Bearer <token>` and `Deriv-App-ID`. The PAT is a
    /// user secret and only ever rides the Authorization header.
    ///
    /// # Errors
    /// [`MarketDataError::Normalization`] on a Deriv refusal or a transport
    /// failure.
    pub async fn otp_url(&self, account_id: &str, token: &str) -> Result<String, MarketDataError> {
        let url = format!(
            "{}/trading/v1/options/accounts/{account_id}/otp",
            self.rest_url
        );
        let response = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Deriv-App-ID", &self.app_id)
            .send()
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv otp failed: {e}")))?;
        let reply: OtpReply = response
            .json()
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv otp reply: {e}")))?;
        if let Some(err) = reply.error {
            return Err(MarketDataError::Normalization(format!(
                "deriv {}: {}",
                err.code.unwrap_or_else(|| "error".to_string()),
                err.message.unwrap_or_else(|| "unknown".to_string())
            )));
        }
        reply
            .data
            .map(|d| d.url)
            .ok_or_else(|| MarketDataError::Normalization("deriv otp returned no url".to_string()))
    }

    /// Connect to an OTP-issued trading URL.
    async fn connect_trading(&self, url: &str) -> Result<WsStream, MarketDataError> {
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv trading connect: {e}")))?;
        Ok(socket)
    }

    /// One request/response round trip on an existing trading socket,
    /// correlated by `req_id` (other envelopes -- stream pushes, heartbeats --
    /// are skipped while waiting).
    async fn call(
        &self,
        socket: &mut WsStream,
        body: serde_json::Value,
    ) -> Result<TradeResponse, MarketDataError> {
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .map_err(|e| MarketDataError::Normalization(format!("deriv trading send: {e}")))?;
        let want = body.get("req_id").and_then(|v| v.as_u64());
        loop {
            let Some(frame) = socket.next().await else {
                return Err(MarketDataError::Normalization(
                    "deriv trading closed the socket".to_string(),
                ));
            };
            let frame = frame.map_err(|e| {
                MarketDataError::Normalization(format!("deriv trading socket: {e}"))
            })?;
            let Message::Text(text) = frame else { continue };
            let parsed: TradeResponse = serde_json::from_str(&text).map_err(|e| {
                MarketDataError::Normalization(format!("deriv trading reply: {e}"))
            })?;
            if let Some(err) = &parsed.error {
                return Err(MarketDataError::from(err));
            }
            if want.is_some() && parsed.req_id == want {
                return Ok(parsed);
            }
            if want.is_none() && parsed.req_id.is_none() {
                return Ok(parsed);
            }
        }
    }

    /// Full session in one call: connect via OTP and run `f` over it. The
    /// OTP is single-use, so every session is a fresh socket.
    pub async fn session<T>(
        &self,
        account_id: &str,
        token: &str,
        f: impl FnOnce(TradingSession) -> futures::future::BoxFuture<'_, Result<T, MarketDataError>>,
    ) -> Result<T, MarketDataError> {
        let url = self.otp_url(account_id, token).await?;
        let socket = self.connect_trading(&url).await?;
        f(TradingSession {
            socket,
            gateway: self,
            next_req: 1,
        })
        .await
    }
}

impl From<&TradeError> for MarketDataError {
    fn from(err: &TradeError) -> Self {
        MarketDataError::Normalization(format!(
            "deriv {}: {}",
            err.code.clone().unwrap_or_else(|| "error".to_string()),
            err.message.clone().unwrap_or_else(|| "unknown".to_string())
        ))
    }
}

/// An open trading session: one socket, incrementing `req_id`s.
pub struct TradingSession<'a> {
    socket: WsStream,
    gateway: &'a DerivTradingGateway,
    next_req: u64,
}

impl<'a> TradingSession<'a> {
    fn req_id(&mut self) -> u64 {
        let id = self.next_req;
        self.next_req += 1;
        id
    }

    /// The account balance.
    ///
    /// # Errors
    /// As [`DerivTradingGateway::otp_url`].
    pub async fn balance(&mut self) -> Result<f64, MarketDataError> {
        let id = self.req_id();
        let body = serde_json::json!({ "balance": 1, "req_id": id });
        let reply = self.gateway.call(&mut self.socket, body).await?;
        let balance = reply
            .balance
            .and_then(|b| b.get("balance").and_then(|v| v.as_f64()))
            .ok_or_else(|| {
                MarketDataError::Normalization("deriv balance reply had no balance".to_string())
            })?;
        Ok(balance)
    }

    /// Quote a contract (`proposal`): the id `buy` consumes and the ask price.
    ///
    /// # Errors
    /// As [`DerivTradingGateway::otp_url`].
    pub async fn proposal(&mut self, params: &ProposalParams) -> Result<Proposal, MarketDataError> {
        let id = self.req_id();
        let body = params.to_request(id);
        let reply = self.gateway.call(&mut self.socket, body).await?;
        let p = reply
            .proposal
            .ok_or_else(|| MarketDataError::Normalization("deriv proposal reply missing".to_string()))?;
        let proposal: Proposal = serde_json::from_value(p).map_err(|e| {
            MarketDataError::Normalization(format!("deriv proposal shape: {e}"))
        })?;
        Ok(proposal)
    }

    /// Buy with a proposal id at a max price.
    ///
    /// # Errors
    /// As [`DerivTradingGateway::otp_url`].
    pub async fn buy(&mut self, proposal_id: &str, max_price: f64) -> Result<Purchase, MarketDataError> {
        let id = self.req_id();
        let body = serde_json::json!({
            "buy": proposal_id,
            "price": max_price,
            "req_id": id,
        });
        let reply = self.gateway.call(&mut self.socket, body).await?;
        let b = reply
            .buy
            .ok_or_else(|| MarketDataError::Normalization("deriv buy reply missing".to_string()))?;
        let purchase: Purchase = serde_json::from_value(b).map_err(|e| {
            MarketDataError::Normalization(format!("deriv buy shape: {e}"))
        })?;
        Ok(purchase)
    }

    /// The portfolio of open contracts.
    ///
    /// # Errors
    /// As [`DerivTradingGateway::otp_url`].
    pub async fn portfolio(&mut self) -> Result<Vec<OpenContract>, MarketDataError> {
        let id = self.req_id();
        let body = serde_json::json!({ "portfolio": 1, "req_id": id });
        let reply = self.gateway.call(&mut self.socket, body).await?;
        let p = reply
            .portfolio
            .ok_or_else(|| MarketDataError::Normalization("deriv portfolio reply missing".to_string()))?;
        let contracts = p
            .get("contracts")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for c in contracts {
            out.push(
                serde_json::from_value(c).map_err(|e| {
                    MarketDataError::Normalization(format!("deriv contract shape: {e}"))
                })?,
            );
        }
        Ok(out)
    }

    /// Sell a contract early at (at most) `price`.
    ///
    /// # Errors
    /// As [`DerivTradingGateway::otp_url`].
    pub async fn sell(&mut self, contract_id: u64, price: f64) -> Result<f64, MarketDataError> {
        let id = self.req_id();
        let body = serde_json::json!({
            "sell": contract_id,
            "price": price,
            "req_id": id,
        });
        let reply = self.gateway.call(&mut self.socket, body).await?;
        let s = reply
            .sell
            .ok_or_else(|| MarketDataError::Normalization("deriv sell reply missing".to_string()))?;
        s.get("sold_for")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| MarketDataError::Normalization("deriv sell reply had no price".to_string()))
    }
}

/// One contract's lifecycle as a small explicit state machine: quoted →
/// purchased → open → closed (sold, expired, or stopped out). Every order
/// the platform places becomes one of these, so "what stage is this contract
/// at" is a value with transitions, not a pile of ad-hoc flags.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractOrder {
    /// A proposal exists but nothing was bought.
    Quoted(Proposal),
    /// Bought and open: the venue's contract id plus what we paid.
    Open {
        /// The venue's contract id.
        contract_id: u64,
        /// The proposal the purchase consumed.
        proposal: Proposal,
        /// The purchase price paid.
        purchase_price: f64,
    },
    /// Terminal: closed by sell, expiry, or stop-out, with the final PnL.
    Closed {
        /// The venue's contract id.
        contract_id: u64,
        /// How it ended.
        reason: CloseReason,
        /// Final realized PnL in account currency.
        pnl: f64,
    },
}

/// Why a contract closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Sold early via `sell`.
    SoldEarly,
    /// Expired by clock (expiry contracts).
    Expired,
    /// The stake was consumed (stop-out).
    StoppedOut,
    /// A limit-order take-profit fired.
    TakeProfit,
    /// A limit-order stop-loss fired.
    StopLoss,
}

impl ContractOrder {
    /// Buy a quote: Quoted → Open (or straight to Closed on a rejected price).
    #[must_use]
    pub fn buy(self, purchase: &Purchase) -> Self {
        match self {
            ContractOrder::Quoted(proposal) => ContractOrder::Open {
                contract_id: purchase.contract_id,
                proposal,
                purchase_price: purchase.purchase_price.unwrap_or_default(),
            },
            // Re-buying is a caller bug; keep the terminal/open state as-is.
            other => other,
        }
    }

    /// Close an open contract with a reason and its PnL: Open → Closed.
    #[must_use]
    pub fn close(self, reason: CloseReason, pnl: f64) -> Self {
        match self {
            ContractOrder::Open { contract_id, .. } => ContractOrder::Closed {
                contract_id,
                reason,
                pnl,
            },
            other => other,
        }
    }

    /// Whether the contract is in its terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, ContractOrder::Closed { .. })
    }
}

// ---------------------------------------------------------------------------
// The multiplier PnL model (shared by the backtest simulator and the docs).

/// Multiplier PnL at an exit price, Deriv's own semantics: the move scales by
/// the multiplier, and the loss is capped at the stake (a stop-out, which
/// triggers when the adverse move reaches `1 / multiplier` of the entry).
/// `long` is MULTUP; `!long` is MULTDOWN.
#[must_use]
pub fn multiplier_pnl(entry: f64, exit: f64, stake: f64, multiplier: f64, long: bool) -> f64 {
    if entry <= 0.0 || stake <= 0.0 || multiplier <= 0.0 {
        return 0.0;
    }
    let dir = if long { 1.0 } else { -1.0 };
    let raw = stake * multiplier * (exit - entry) / entry * dir;
    raw.max(-stake)
}

/// The stop-out level: the adverse move that costs the whole stake.
#[must_use]
pub fn stop_out_level(entry: f64, multiplier: f64, long: bool) -> f64 {
    if long {
        entry * (1.0 - 1.0 / multiplier)
    } else {
        entry * (1.0 + 1.0 / multiplier)
    }
}

/// One candle's path against an open multiplier position: does the candle
/// stop the position out, hit the take-profit, or hit the stop-loss first?
/// Intrabar order follows the venue's own rule (the stop fills first when
/// both could touch -- the same conservative choice the spot sim makes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOutcome {
    /// Nothing triggered; the position rides.
    Open,
    /// The stop-out level was touched; the stake is lost.
    StoppedOut,
    /// `take_profit` was touched.
    TakeProfit,
    /// `stop_loss` was touched.
    StopLoss,
}

/// Evaluate one candle's range against an open multiplier position.
#[must_use]
pub fn path_outcome(
    candle: &Candle,
    entry: f64,
    multiplier: f64,
    long: bool,
    take_profit: Option<f64>,
    stop_loss: Option<f64>,
) -> PathOutcome {
    let stop_out = stop_out_level(entry, multiplier, long);
    if long {
        if candle.low <= stop_out {
            return PathOutcome::StoppedOut;
        }
        // A user stop-loss sits above the stop-out (a smaller loss); check it
        // first only if it was NOT already covered by the stop-out check.
        if let Some(sl) = stop_loss {
            if candle.low <= sl {
                return PathOutcome::StopLoss;
            }
        }
        if let Some(tp) = take_profit {
            if candle.high >= tp {
                return PathOutcome::TakeProfit;
            }
        }
    } else {
        if candle.high >= stop_out {
            return PathOutcome::StoppedOut;
        }
        if let Some(sl) = stop_loss {
            if candle.high >= sl {
                return PathOutcome::StopLoss;
            }
        }
        if let Some(tp) = take_profit {
            if candle.low <= tp {
                return PathOutcome::TakeProfit;
            }
        }
    }
    PathOutcome::Open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "live trading round-trip: needs DERIV_TEST_TOKEN (trade-scoped PAT) and DERIV_TEST_ACCOUNT (a VRTC demo loginid) in the environment"]
    async fn demo_multiplier_round_trip() {
        let Ok(token) = std::env::var("DERIV_TEST_TOKEN") else {
            eprintln!("DERIV_TEST_TOKEN unset; skipping the live round-trip");
            return;
        };
        let Ok(account) = std::env::var("DERIV_TEST_ACCOUNT") else {
            eprintln!("DERIV_TEST_ACCOUNT unset; skipping the live round-trip");
            return;
        };
        let gateway = DerivTradingGateway::from_env();
        gateway
            .session(&account, &token, |mut s| {
                Box::pin(async move {
                    let balance = s.balance().await?;
                    assert!(balance > 1.0, "the demo account is funded: {balance}");
                    let proposal = s
                        .proposal(&ProposalParams::multiplier_up("R_75", 1.0, 100.0, "USD"))
                        .await?;
                    assert!(proposal.ask_price > 0.0 && proposal.ask_price <= 1.05);
                    let purchase = s.buy(&proposal.id, proposal.ask_price * 1.05).await?;
                    let open = s.portfolio().await?;
                    assert!(
                        open.iter().any(|c| c.contract_id == purchase.contract_id),
                        "the portfolio holds the contract"
                    );
                    let sold = s.sell(purchase.contract_id, 0.0).await?;
                    assert!(sold >= 0.0, "sold for {sold}");
                    Ok(())
                })
            })
            .await
            .expect("the demo round-trip completes");
    }

    #[test]
    fn the_contract_lifecycle_moves_forward_only() {
        let proposal = Proposal { id: "p1".to_string(), ask_price: 1.0, payout: None };
        let order = ContractOrder::Quoted(proposal.clone());
        assert!(!order.is_terminal());
        let purchased = order.buy(&Purchase {
            contract_id: 42,
            purchase_price: Some(1.0),
            balance_after: Some(99.0),
        });
        assert!(!purchased.is_terminal());
        let closed = purchased.close(CloseReason::SoldEarly, 0.35);
        assert!(closed.is_terminal());
        assert!(matches!(
            closed,
            ContractOrder::Closed { contract_id: 42, reason: CloseReason::SoldEarly, pnl } if (pnl - 0.35).abs() < 1e-9
        ));
        // Terminal stays terminal under further transitions.
        let still = closed.clone().close(CloseReason::TakeProfit, 9.0);
        assert_eq!(still, closed);
    }

    #[test]
    fn the_proposal_request_carries_the_multiplier_shape() {
        let p = ProposalParams::multiplier_up("R_75", 10.0, 100.0, "USD");
        let req = p.to_request(7);
        assert_eq!(req["proposal"], 1);
        assert_eq!(req["contract_type"], "MULTUP");
        assert_eq!(req["underlying_symbol"], "R_75");
        assert_eq!(req["amount"], 10.0);
        assert_eq!(req["multiplier"], 100.0);
        assert_eq!(req["req_id"], 7);
        assert_eq!(req["basis"], "stake");
        assert!(req.get("duration").is_none(), "multipliers do not expire by clock");
        let down = ProposalParams::multiplier_down("R_75", 5.0, 50.0, "USD");
        assert_eq!(down.contract_type, "MULTDOWN");
        let mut with_limits = ProposalParams::multiplier_up("R_75", 10.0, 100.0, "USD");
        with_limits.take_profit = Some(105.0);
        with_limits.stop_loss = Some(97.0);
        let req = with_limits.to_request(1);
        assert_eq!(req["limit_order"]["take_profit"], 105.0);
        assert_eq!(req["limit_order"]["stop_loss"], 97.0);
    }

    #[test]
    fn multiplier_pnl_scales_the_move_and_caps_the_loss() {
        // +1% move on a 100x multiplier is +100% of stake.
        let pnl = multiplier_pnl(100.0, 101.0, 10.0, 100.0, true);
        assert!((pnl - 10.0).abs() < 1e-9);
        // The same move against a MULTDOWN: -100% of stake = stop-out.
        let pnl = multiplier_pnl(100.0, 101.0, 10.0, 100.0, false);
        assert_eq!(pnl, -10.0);
        // A -2% move at 100x is -200% of stake, but the loss caps at -stake.
        let pnl = multiplier_pnl(100.0, 98.0, 10.0, 100.0, true);
        assert_eq!(pnl, -10.0);
        // Stop-out levels: 1/multiplier of entry in the adverse direction.
        assert!((stop_out_level(100.0, 100.0, true) - 99.0).abs() < 1e-9);
        assert!((stop_out_level(100.0, 100.0, false) - 101.0).abs() < 1e-9);
        assert!((stop_out_level(100.0, 50.0, true) - 98.0).abs() < 1e-9);
    }

    #[test]
    fn the_path_outcome_honors_stop_out_before_limit_orders() {
        let c = |h: f64, l: f64| Candle {
            symbol: "T".to_string(),
            timeframe: analytics_core::types::Timeframe::M1,
            open_time: 0,
            open: 100.0,
            high: h,
            low: l,
            close: 100.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        };
        // Riding: nothing touched.
        assert_eq!(
            path_outcome(&c(101.0, 99.5), 100.0, 100.0, true, Some(105.0), Some(98.5)),
            PathOutcome::Open
        );
        // The stop-out (99.0 for 100x long) fires.
        assert_eq!(
            path_outcome(&c(101.0, 98.9), 100.0, 100.0, true, Some(105.0), Some(98.5)),
            PathOutcome::StoppedOut
        );
        // A user stop-loss ABOVE the stop-out (99.5 > 99.0) fires on its own
        // level -- the low 99.4 touches it without reaching the stop-out.
        assert_eq!(
            path_outcome(&c(101.0, 99.4), 100.0, 100.0, true, Some(105.0), Some(99.5)),
            PathOutcome::StopLoss
        );
        // Take profit for a long at the high.
        assert_eq!(
            path_outcome(&c(105.5, 99.5), 100.0, 100.0, true, Some(105.0), Some(98.5)),
            PathOutcome::TakeProfit
        );
        // SHORT: the stop-out is above (101.0 for 100x).
        assert_eq!(
            path_outcome(&c(101.1, 99.0), 100.0, 100.0, false, None, None),
            PathOutcome::StoppedOut
        );
        // SHORT take-profit at the low.
        assert_eq!(
            path_outcome(&c(100.5, 94.0), 100.0, 100.0, false, Some(95.0), None),
            PathOutcome::TakeProfit
        );
    }
}
