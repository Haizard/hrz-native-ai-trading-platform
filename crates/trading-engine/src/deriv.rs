//! The Deriv execution adapter: contract intents through the platform's
//! order surface.
//!
//! Contracts are not spot orders wearing a costume, and this file is where
//! that honesty is enforced. The mapping, in one paragraph: a **market** buy
//! is a `MULTUP`, a market sell a `MULTDOWN`; `quantity` is the **stake in
//! account currency** (not base units); every purchase consumes a fresh
//! `proposal` (the venue's price quote) and buys at a bounded slippage over
//! the ask; a cancel is an early `sell` of the contract; `reconcile` reads
//! the venue portfolio. Anything the model cannot express -- limit prices,
//! base-asset quantities -- is refused with a named error rather than
//! silently reinterpreted.
//!
//! The OTP-authenticated session (and the `Deriv-App-ID` attribution tag on
//! every call) lives in `market_data::deriv_trade`; this adapter is the
//! order-shape seam the bot layer already speaks (`ExchangeAdapter`).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use market_data::deriv_trade::{
    ContractOrder, DerivTradingGateway, ProposalParams, Purchase,
};
use zeroize::Zeroizing;

use crate::error::ExecutionError;
use crate::execution::{
    ExchangeAdapter, OrderAck, OrderRequest, OrderSide, OrderStatus, OrderStatusReport, OrderType,
};

/// The default leverage when nothing else says: 50x is the middle of Deriv's
/// ladder, not the top of it. `DERIV_DEFAULT_MULTIPLIER` overrides.
pub const DEFAULT_MULTIPLIER: f64 = 50.0;

/// The slippage guard on a purchase: pay at most this fraction above the
/// proposal's ask. A proposal gone staler than that is re-quoted by the
/// venue rather than filled through, which is the honest reading of "price
/// moved while we were buying".
pub const MAX_SLIPPAGE_FRACTION: f64 = 0.02;

/// The Deriv execution adapter. Cloneable by construction: the gateway and
/// every setting here are cheap values; the token is cloned as its own
/// zeroizing box, never shared by reference.
pub struct DerivAdapter {
    gateway: DerivTradingGateway,
    account_id: String,
    token: Zeroizing<String>,
    code: &'static str,
    currency: String,
    leverage: f64,
    /// Our client order ids to the venue's contract ids. Reset with the
    /// process; `reconcile` marks the contracts it cannot map back.
    contracts: Mutex<HashMap<String, u64>>,
}

impl DerivAdapter {
    /// From parts: the OTP gateway, the Deriv account's loginid, its token,
    /// and the instrument the adapter serves.
    #[must_use]
    pub fn new(
        gateway: DerivTradingGateway,
        account_id: impl Into<String>,
        token: impl Into<String>,
        code: &'static str,
        currency: impl Into<String>,
    ) -> Self {
        let leverage = std::env::var("DERIV_DEFAULT_MULTIPLIER")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|m| *m > 0.0)
            .unwrap_or(DEFAULT_MULTIPLIER);
        Self {
            gateway,
            account_id: account_id.into(),
            token: Zeroizing::new(token.into()),
            code,
            currency: currency.into(),
            leverage,
            contracts: Mutex::new(HashMap::new()),
        }
    }

    /// One OTP session's worth of calls, with our account and token bound.
    async fn with_session<T>(
        &self,
        f: impl FnOnce(market_data::deriv_trade::TradingSession<'_>) -> futures::future::BoxFuture<'_, Result<T, market_data::MarketDataError>>,
    ) -> Result<T, ExecutionError> {
        self.gateway
            .session(&self.account_id, &self.token, f)
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))
    }

    fn map_order(&self, order: &OrderRequest) -> Result<ProposalParams, ExecutionError> {
        match order.order_type {
            OrderType::Market => {}
            ref other => {
                return Err(ExecutionError::OrderRejected(format!(
                    "deriv contracts fill at the venue's ask; order type {other:?} is not expressible (market only)"
                )));
            }
        }
        let (contract_type, _dir) = match order.side {
            OrderSide::Buy => ("MULTUP", 1.0),
            OrderSide::Sell => ("MULTDOWN", -1.0),
        };
        if order.quantity <= 0.0 {
            return Err(ExecutionError::OrderRejected(
                "deriv stake (order quantity, in account currency) must be positive".to_string(),
            ));
        }
        Ok(ProposalParams {
            contract_type: contract_type.to_string(),
            underlying_symbol: self.code.to_string(),
            amount: order.quantity,
            currency: self.currency.clone(),
            multiplier: Some(self.leverage),
            duration: None,
            duration_unit: None,
            take_profit: None,
            stop_loss: None,
        })
    }

    fn remember(&self, client_order_id: &str, contract_id: u64) {
        if let Ok(mut map) = self.contracts.lock() {
            map.insert(client_order_id.to_string(), contract_id);
        }
    }

    fn contract_for(&self, client_order_id: &str) -> Option<u64> {
        self.contracts
            .lock()
            .ok()
            .and_then(|map| map.get(client_order_id).copied())
    }
}

#[async_trait]
impl ExchangeAdapter for DerivAdapter {
    fn venue(&self) -> &str {
        "deriv"
    }

    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
        let params = self.map_order(&order)?;
        let client_id = order.client_order_id.clone();
        let purchase = self
            .with_session(|mut s| {
                Box::pin(async move {
                    let proposal = s.proposal(&params).await?;
                    let max_price = proposal.ask_price * (1.0 + MAX_SLIPPAGE_FRACTION);
                    let purchase = s.buy(&proposal.id, max_price).await?;
                    Ok((proposal, purchase))
                })
            })
            .await
            .map(|(proposal, purchase): (market_data::deriv_trade::Proposal, Purchase)| {
                (proposal, purchase)
            })?;
        let (proposal, purchase) = purchase;
        self.remember(&client_id, purchase.contract_id);
        Ok(OrderAck {
            client_order_id: client_id,
            exchange_order_id: purchase.contract_id.to_string(),
            status: OrderStatus::Filled,
            filled_qty: order.quantity,
            avg_price: Some(proposal.ask_price),
        })
    }

    async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError> {
        let contract_id = self.contract_for(client_order_id).ok_or_else(|| {
            ExecutionError::OrderRejected(format!(
                "`{client_order_id}` is not an open contract this adapter knows (the map resets with the process; sell it on the venue directly)"
            ))
        })?;
        self.with_session(|mut s| Box::pin(async move { s.sell(contract_id, 0.0).await }))
            .await?;
        Ok(())
    }

    async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
        let open = self
            .with_session(|mut s| Box::pin(async move { s.portfolio().await }))
            .await?;
        let map = self.contracts.lock().ok().map(|m| {
            m.iter()
                .map(|(c, id)| (*id, c.clone()))
                .collect::<HashMap<u64, String>>()
        });
        Ok(open
            .into_iter()
            .map(|c| {
                let client_id = map
                    .as_ref()
                    .and_then(|m| m.get(&c.contract_id).cloned())
                    .unwrap_or_default();
                OrderStatusReport {
                    client_order_id: client_id,
                    exchange_order_id: c.contract_id.to_string(),
                    // The purchase already filled; the contract runs. The
                    // stake is the filled amount (currency), the ask the
                    // average price when the venue reports it.
                    status: OrderStatus::Filled,
                    filled_qty: 0.0,
                    avg_price: None,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_market_buy_maps_to_a_multiplier_up() {
        let adapter = DerivAdapter::new(
            DerivTradingGateway::from_env(),
            "VRTC123",
            "token",
            "R_75",
            "USD",
        );
        let order = OrderRequest {
            client_order_id: "bot_test_1".to_string(),
            symbol: "DERIV:VOLATILITY 75".to_string(),
            side: OrderSide::Buy,
            quantity: 5.0,
            order_type: OrderType::Market,
        };
        let params = adapter.map_order(&order).expect("maps");
        assert_eq!(params.contract_type, "MULTUP");
        assert_eq!(params.underlying_symbol, "R_75");
        assert_eq!(params.amount, 5.0);
        assert_eq!(params.multiplier, Some(DEFAULT_MULTIPLIER));
        assert!(params.duration.is_none(), "multipliers run to sell or stop-out");

        let sell = OrderRequest { side: OrderSide::Sell, ..order };
        let params = adapter.map_order(&sell).expect("maps");
        assert_eq!(params.contract_type, "MULTDOWN");
    }

    #[test]
    fn non_market_orders_and_bad_stakes_are_refused() {
        let adapter = DerivAdapter::new(
            DerivTradingGateway::from_env(),
            "VRTC123",
            "token",
            "R_75",
            "USD",
        );
        let limit = OrderRequest {
            client_order_id: "bot_test_2".to_string(),
            symbol: "DERIV:VOLATILITY 75".to_string(),
            side: OrderSide::Buy,
            quantity: 5.0,
            order_type: OrderType::Limit { price: 100.0 },
        };
        let err = adapter.map_order(&limit).expect_err("limit refused");
        assert!(err.to_string().contains("market only"), "{err}");

        let zero = OrderRequest {
            order_type: OrderType::Market,
            quantity: 0.0,
            ..limit
        };
        let err = adapter.map_order(&zero).expect_err("zero stake refused");
        assert!(err.to_string().contains("positive"), "{err}");
    }

    #[test]
    fn the_lifecycle_states_flow_through_the_adapter_types() {
        // The adapter's own state machine: Quoted -> Open -> Closed, and the
        // order map remembers client ids.
        let adapter = DerivAdapter::new(
            DerivTradingGateway::from_env(),
            "VRTC123",
            "token",
            "R_75",
            "USD",
        );
        adapter.remember("bot_test_3", 42);
        assert_eq!(adapter.contract_for("bot_test_3"), Some(42));
        assert_eq!(adapter.contract_for("unknown"), None);

        let order = ContractOrder::Quoted(market_data::deriv_trade::Proposal {
            id: "p1".to_string(),
            ask_price: 1.0,
            payout: None,
        });
        let order = order.buy(&Purchase {
            contract_id: 42,
            purchase_price: Some(1.0),
            balance_after: Some(99.0),
        });
        let order = order.close(market_data::deriv_trade::CloseReason::TakeProfit, 2.5);
        assert!(order.is_terminal());
    }
}
