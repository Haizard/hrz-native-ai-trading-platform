//! Binance spot REST adapter (`docs/11`).
//!
//! ## Signed with HMAC-SHA256, per request
//!
//! Binance signs the *query string*, not a body, so every parameter -- including
//! the timestamp and receive window -- is part of what gets signed and is sent
//! in the URL. A signature over a reordered or re-encoded parameter set is
//! invalid, which is why [`query_string`] is the only place parameters are
//! assembled.
//!
//! ## Scope: trading only, and that is enforced by the operator
//!
//! `docs/15` asks for keys scoped to trading and never withdrawal. The platform
//! cannot verify a key's permissions -- Binance does not expose them -- so it
//! does the next best thing: it never asks for a permission it does not need,
//! and the runbook tells the operator to create the key with trading enabled
//! and withdrawal disabled. A platform that *claims* to have verified something
//! it cannot is worse than one that says so.
//!
//! ## Responses are parsed loosely, on purpose
//!
//! Binance returns order ids and quantities as numbers on some endpoints and as
//! strings on others. A strict `Deserialize` struct that fails on a live
//! response does so *after* the order exists, which is the single worst moment
//! to lose the ability to parse. Fields are therefore read individually and
//! defaulted.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use sha2::Sha256;

use crate::credentials::ExchangeCredentials;
use crate::error::ExecutionError;
use crate::execution::{
    ExchangeAdapter, OrderAck, OrderRequest, OrderStatus, OrderStatusReport, OrderType,
};

/// Production REST base URL.
pub const MAINNET: &str = "https://api.binance.com";

/// Spot testnet REST base URL. Funds there are not real.
pub const TESTNET: &str = "https://testnet.binance.vision";

/// The header carrying the API key.
pub const API_KEY_HEADER: &str = "X-MBX-APIKEY";

/// How long the exchange will accept a signed request after its timestamp.
const RECV_WINDOW_MS: u64 = 5_000;

/// How long to wait for a response before giving up.
///
/// `reqwest::Client::new()` waits **forever**, which is the wrong default here
/// and not an obvious one: a TCP connection that is accepted and then never
/// answered leaves `place_order` pending, the bot's decision loop blocked
/// behind it, and the stop path unable to make progress until
/// [`BotSupervisor`](https://docs.rs/api-gateway)'s grace expires and the task
/// is aborted -- mid-request, with the order's fate unknown.
///
/// Ten seconds is deliberately shorter than that grace: a timeout turns an
/// unanswerable request into a *transport error*, which is the one error the
/// gateway knows how to recover from (ask the exchange, adopt or retry once,
/// otherwise stop). Waiting forever is the only outcome with no rule.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Sign `payload` with `secret`, returning lowercase hex.
///
/// The one use of the secret in the whole codebase.
#[must_use]
pub fn sign(secret: &str, payload: &str) -> String {
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        // An HMAC key may be any length; this does not fail in practice, and
        // the alternative -- panicking while holding a secret -- is worse.
        Err(_) => return String::new(),
    };
    mac.update(payload.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Assemble and percent-encode a query string, in the order given.
#[must_use]
pub fn query_string(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Minimal percent-encoding for query values.
///
/// Everything this adapter sends is alphanumeric, `-`, `_` or `.`, but a symbol
/// with a different shape must not silently become a *different* string after
/// signing and another before sending -- the signature would not match.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => {
                use std::fmt::Write;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Format a quantity or price the way the exchange expects.
///
/// Eight decimals, trailing zeros removed. Precision beyond the symbol's
/// `LOT_SIZE` filter is *not* rounded here: the exchange rejects it with a
/// specific error, and silently rounding to something the operator did not ask
/// for would be worse than a clear refusal.
#[must_use]
pub fn num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let formatted = format!("{value:.8}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// A Binance spot REST client for one symbol.
pub struct BinanceRest {
    base_url: String,
    symbol: String,
    credentials: ExchangeCredentials,
    client: reqwest::Client,
}

impl BinanceRest {
    /// Build a client for one symbol.
    #[must_use]
    pub fn new(base_url: &str, symbol: &str, credentials: ExchangeCredentials) -> Self {
        Self::with_timeout(base_url, symbol, credentials, REQUEST_TIMEOUT)
    }

    /// Build a client with a chosen request timeout.
    ///
    /// Exists so the timeout is *testable*: a test cannot wait ten seconds to
    /// prove that ten seconds is the bound, and a bound nobody has watched
    /// expire is a bound that silently becomes `None` in a refactor.
    #[must_use]
    pub fn with_timeout(
        base_url: &str,
        symbol: &str,
        credentials: ExchangeCredentials,
        timeout: Duration,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            symbol: symbol.to_string(),
            credentials,
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// A client pointed at the spot testnet.
    #[must_use]
    pub fn testnet(symbol: &str, credentials: ExchangeCredentials) -> Self {
        Self::new(TESTNET, symbol, credentials)
    }

    /// The symbol this client trades.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// The base URL in use, so a log can say whether this is testnet.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build from `BINANCE_API_KEY`, `BINANCE_API_SECRET` and an optional
    /// `BINANCE_BASE_URL`.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Credentials`] when the key or secret is
    /// absent; it never echoes either.
    pub fn from_env(symbol: &str) -> Result<Self, ExecutionError> {
        let credentials = ExchangeCredentials::from_env("binance")?;
        let base_url = std::env::var("BINANCE_BASE_URL").unwrap_or_else(|_| MAINNET.to_string());
        Ok(Self::new(&base_url, symbol, credentials))
    }

    /// Append `signature` to a fully assembled query string.
    fn signed(&self, params: &[(&str, String)]) -> String {
        let timestamp = now_ms().to_string();
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("recvWindow", RECV_WINDOW_MS.to_string()));
        all.push(("timestamp", timestamp));
        let query = query_string(&all);
        // Signed last, over everything that precedes it, in that exact order.
        format!(
            "{}&signature={}",
            query,
            sign(self.credentials.secret(), &query)
        )
    }

    fn url(&self, path: &str, params: &[(&str, String)]) -> String {
        format!("{}{path}?{}", self.base_url, self.signed(params))
    }

    /// Turn a non-2xx body into a typed error.
    fn exchange_error(status: StatusCode, body: &serde_json::Value) -> ExecutionError {
        let code = body.get("code").and_then(as_f64).unwrap_or(-1.0) as i64;
        let message = body
            .get("msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown exchange error")
            .to_string();
        if code == -1000 || code == -1001 || code == -1007 {
            // These are "we could not understand/handle it right now" codes;
            // treating them as transport failures is what lets the gateway's
            // recover-then-stop path apply to them too.
            return ExecutionError::Transport(format!("{status}: {message}"));
        }
        ExecutionError::Exchange { code, message }
    }
}

/// Now, in unix milliseconds.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Read a JSON field that may be a number or a string.
fn as_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Read a JSON field as text, whichever of the two shapes it came in.
fn as_string(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

#[async_trait]
impl ExchangeAdapter for BinanceRest {
    fn venue(&self) -> &str {
        "binance"
    }

    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
        let mut params: Vec<(&str, String)> = vec![
            ("symbol", order.symbol.clone()),
            ("side", order.side.as_str().to_string()),
            ("type", order.order_type.as_str().to_string()),
            ("quantity", num(order.quantity)),
            ("newClientOrderId", order.client_order_id.clone()),
        ];
        match order.order_type {
            OrderType::Market => {}
            OrderType::Limit { price } => {
                params.push(("price", num(price)));
                params.push(("timeInForce", "GTC".into()));
            }
            OrderType::StopMarket { stop_price } => {
                params.push(("stopPrice", num(stop_price)));
            }
            OrderType::TakeProfitLimit { stop_price, price } => {
                params.push(("stopPrice", num(stop_price)));
                params.push(("price", num(price)));
                params.push(("timeInForce", "GTC".into()));
            }
        }

        let url = self.url("/api/v3/order", &params);
        let response = self
            .client
            .post(&url)
            .header(API_KEY_HEADER, self.credentials.key())
            .send()
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))?;

        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))?;

        if !status.is_success() {
            return Err(Self::exchange_error(status, &body));
        }

        let filled_qty = body.get("executedQty").and_then(as_f64).unwrap_or(0.0);
        let quote_qty = body
            .get("cummulativeQuoteQty")
            .and_then(as_f64)
            .unwrap_or(0.0);
        let avg_price = (filled_qty > 0.0).then(|| quote_qty / filled_qty);

        Ok(OrderAck {
            client_order_id: order.client_order_id,
            exchange_order_id: as_string(body.get("orderId")),
            status: OrderStatus::parse(
                body.get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("NEW"),
            ),
            filled_qty,
            avg_price,
        })
    }

    async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError> {
        let params = vec![
            ("symbol", self.symbol.clone()),
            ("origClientOrderId", client_order_id.to_string()),
        ];
        let url = self.url("/api/v3/order", &params);
        let response = self
            .client
            .delete(&url)
            .header(API_KEY_HEADER, self.credentials.key())
            .send()
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))?;

        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);

        if !status.is_success() {
            // "Unknown order sent" means it is already gone -- cancelled or
            // filled. That is the outcome we wanted, so it is not an error.
            let code = body.get("code").and_then(as_f64).unwrap_or(0.0) as i64;
            if code == -2011 {
                return Ok(());
            }
            return Err(Self::exchange_error(status, &body));
        }
        Ok(())
    }

    async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
        let params = vec![("symbol", self.symbol.clone())];
        let url = self.url("/api/v3/openOrders", &params);
        let response = self
            .client
            .get(&url)
            .header(API_KEY_HEADER, self.credentials.key())
            .send()
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))?;

        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ExecutionError::Transport(e.to_string()))?;

        if !status.is_success() {
            return Err(Self::exchange_error(status, &body));
        }

        let orders = body.as_array().cloned().unwrap_or_default();
        Ok(orders
            .iter()
            .map(|order| {
                let filled_qty = order.get("executedQty").and_then(as_f64).unwrap_or(0.0);
                let quote_qty = order
                    .get("cummulativeQuoteQty")
                    .and_then(as_f64)
                    .unwrap_or(0.0);
                OrderStatusReport {
                    client_order_id: as_string(order.get("clientOrderId")),
                    exchange_order_id: as_string(order.get("orderId")),
                    status: OrderStatus::parse(
                        order
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("NEW"),
                    ),
                    filled_qty,
                    avg_price: (filled_qty > 0.0).then(|| quote_qty / filled_qty),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::OrderSide;
    use axum::Router;
    use axum::extract::{Request, State};
    use axum::routing::{get, post};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    const SECRET: &str = "exchange-secret";

    /// What the mock exchange saw, so a test can assert on the request rather
    /// than only on our parsing of it.
    #[derive(Clone, Default)]
    struct Seen {
        headers: Arc<Mutex<HashMap<String, String>>>,
        queries: Arc<Mutex<Vec<String>>>,
    }

    async fn mock() -> (String, Seen) {
        let seen = Seen::default();

        async fn place(State(seen): State<Seen>, request: Request) -> axum::response::Response {
            seen.queries
                .lock()
                .unwrap()
                .push(request.uri().query().unwrap_or_default().to_string());
            for (name, value) in request.headers() {
                if let Ok(value) = value.to_str() {
                    // Lowercased, because HTTP header names are
                    // case-insensitive and `HeaderMap` iteration yields them
                    // normalised. Looking up "X-MBX-APIKEY" in a map keyed this
                    // way found nothing, which looked like a missing header
                    // rather than a mismatched lookup.
                    seen.headers
                        .lock()
                        .unwrap()
                        .insert(name.as_str().to_ascii_lowercase(), value.to_string());
                }
            }
            // A signature over the query without its own parameter must match
            // the one we were sent -- that is what "signed correctly" means.
            let query = request.uri().query().unwrap_or_default();
            let (rest, signature) = split_signature(query);
            let expected = sign(SECRET, &rest);
            if signature != expected {
                // A real 4xx, not a 200 with an error body. The first version
                // of this mock answered 200, and the adapter -- which trusts
                // the status, correctly, because that is what Binance does --
                // parsed the error body as a successful order with no id.
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({"code": -1022, "msg": "Signature for this request is not valid."})),
                )
                    .into_response();
            }
            axum::Json(serde_json::json!({
                "symbol": "BTCUSDT",
                "orderId": 28_415_774,
                "clientOrderId": "bot_BTCUSDT_1_entry",
                "status": "NEW",
                "executedQty": "0",
                "cummulativeQuoteQty": "0"
            }))
            .into_response()
        }

        async fn cancel(State(seen): State<Seen>, request: Request) -> axum::response::Response {
            seen.queries
                .lock()
                .unwrap()
                .push(request.uri().query().unwrap_or_default().to_string());
            axum::Json(serde_json::json!({"orderId": 1})).into_response()
        }

        async fn open(State(seen): State<Seen>, request: Request) -> axum::response::Response {
            seen.queries
                .lock()
                .unwrap()
                .push(request.uri().query().unwrap_or_default().to_string());
            axum::Json(serde_json::json!([
                {
                    "symbol": "BTCUSDT",
                    "orderId": 1,
                    "clientOrderId": "bot_BTCUSDT_1_stop",
                    "status": "NEW",
                    "executedQty": "0",
                    "cummulativeQuoteQty": "0"
                },
                {
                    "symbol": "BTCUSDT",
                    "orderId": 2,
                    "clientOrderId": "bot_BTCUSDT_2_entry",
                    "status": "PARTIALLY_FILLED",
                    "executedQty": "0.005",
                    "cummulativeQuoteQty": "500"
                }
            ]))
            .into_response()
        }

        use axum::response::IntoResponse;

        let app = Router::new()
            .route("/api/v3/order", post(place).delete(cancel))
            .route("/api/v3/openOrders", get(open))
            .with_state(seen.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    /// Split `...&signature=X` into the signed part and the signature.
    fn split_signature(query: &str) -> (String, String) {
        match query.rsplit_once("&signature=") {
            Some((rest, signature)) => (rest.to_string(), signature.to_string()),
            None => (query.to_string(), String::new()),
        }
    }

    fn credentials() -> ExchangeCredentials {
        ExchangeCredentials::from_parts("binance", "the-key", SECRET)
    }

    #[test]
    fn the_signature_matches_the_rfc_4231_test_vector() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for
        // nothing?". A signature that is merely *a* hash looks identical in a
        // log, so the only useful check is against a published vector -- and
        // the key has to be the vector's key, not the word "key", which is how
        // this test first failed while the code was correct.
        let digest = sign("Jefe", "what do ya want for nothing?");
        assert_eq!(
            digest,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn quantities_are_formatted_without_trailing_zeros() {
        assert_eq!(num(0.01), "0.01");
        assert_eq!(num(1.0), "1");
        assert_eq!(num(0.123456789), "0.12345679");
        assert_eq!(
            num(f64::NAN),
            "0",
            "a non-finite quantity must not reach the URL"
        );
    }

    #[test]
    fn the_query_string_is_percent_encoded() {
        let query = query_string(&[("symbol", "BTC/USDT".into()), ("q", "1".into())]);
        assert_eq!(query, "symbol=BTC%2FUSDT&q=1");
    }

    #[test]
    fn the_testnet_is_a_different_base_url() {
        let client = BinanceRest::testnet("BTCUSDT", credentials());
        assert_eq!(client.base_url(), TESTNET);
        assert_eq!(client.symbol(), "BTCUSDT");
        let mainnet = BinanceRest::new(MAINNET, "BTCUSDT", credentials());
        assert_eq!(mainnet.base_url(), MAINNET);
    }

    #[tokio::test]
    async fn a_request_that_is_never_answered_becomes_a_transport_error() {
        // The failure this pins: `reqwest::Client::new()` waits forever. A
        // venue that accepts the connection and then says nothing would leave
        // the bot's decision loop blocked inside `place_order` -- with an
        // order whose fate is unknown -- until the supervisor's grace expired
        // and aborted the task mid-request.
        //
        // A `Transport` error is the *useful* outcome: it is the one error the
        // gateway knows how to recover from.
        async fn never() -> axum::response::Response {
            std::future::pending::<()>().await;
            unreachable!()
        }

        let app = Router::new().route("/api/v3/order", post(never));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = BinanceRest::with_timeout(
            &format!("http://{addr}"),
            "BTCUSDT",
            credentials(),
            Duration::from_millis(200),
        );

        let error = client
            .place_order(OrderRequest {
                client_order_id: "bot_BTCUSDT_1_entry".into(),
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                quantity: 0.01,
                order_type: OrderType::Market,
            })
            .await
            .expect_err("a silent venue must not hang the bot");

        assert!(
            matches!(error, ExecutionError::Transport(_)),
            "a timeout must be recoverable, got {error:?}"
        );
    }

    #[tokio::test]
    async fn placing_an_order_sends_the_key_header_and_a_valid_signature() {
        let (base, seen) = mock().await;
        let client = BinanceRest::new(&base, "BTCUSDT", credentials());

        let ack = client
            .place_order(OrderRequest {
                client_order_id: "bot_BTCUSDT_1_entry".into(),
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                quantity: 0.01,
                order_type: OrderType::Market,
            })
            .await
            .expect("the order must be accepted");

        assert_eq!(ack.exchange_order_id, "28415774");
        assert_eq!(ack.status, OrderStatus::New);

        let headers = seen.headers.lock().unwrap();
        assert_eq!(
            headers
                .get(&API_KEY_HEADER.to_ascii_lowercase())
                .map(String::as_str),
            Some("the-key"),
            "the key travels in the header, never in the URL"
        );
        let query = seen.queries.lock().unwrap()[0].clone();
        assert!(query.contains("newClientOrderId=bot_BTCUSDT_1_entry"));
        assert!(query.contains("type=MARKET"));
        assert!(query.contains("quantity=0.01"));
        assert!(
            !query.contains(SECRET),
            "the secret must never be sent: {query}"
        );
    }

    #[tokio::test]
    async fn a_rejection_becomes_a_typed_exchange_error() {
        // Point the client at a base URL whose signature will not verify, which
        // is the cheapest way to make the mock return an error body.
        let (base, _seen) = mock().await;
        let client = BinanceRest::new(
            &base,
            "BTCUSDT",
            ExchangeCredentials::from_parts("binance", "the-key", "the-wrong-secret"),
        );

        let error = client
            .place_order(OrderRequest {
                client_order_id: "bot_BTCUSDT_1_entry".into(),
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                quantity: 0.01,
                order_type: OrderType::Market,
            })
            .await
            .expect_err("must be rejected");

        match error {
            ExecutionError::Exchange { code, message } => {
                assert_eq!(code, -1022);
                // Case-insensitively: the venue says "Signature", and a
                // case-sensitive check here failed on the mock's own wording.
                assert!(message.to_lowercase().contains("signature"), "{message}");
            }
            other => panic!("expected an exchange error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reconciliation_parses_what_the_exchange_holds() {
        let (base, _seen) = mock().await;
        let client = BinanceRest::new(&base, "BTCUSDT", credentials());

        let reports = client.reconcile().await.expect("open orders");
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].client_order_id, "bot_BTCUSDT_1_stop");
        assert_eq!(reports[1].status, OrderStatus::PartiallyFilled);
        assert_eq!(reports[1].filled_qty, 0.005);
        // 500 quote currency over 0.005 base is an average price of 100,000.
        assert_eq!(reports[1].avg_price, Some(100_000.0));
    }

    #[tokio::test]
    async fn cancelling_sends_the_client_id_we_generated() {
        let (base, seen) = mock().await;
        let client = BinanceRest::new(&base, "BTCUSDT", credentials());

        client
            .cancel_order("bot_BTCUSDT_1_stop")
            .await
            .expect("cancel");

        let query = seen.queries.lock().unwrap()[0].clone();
        assert!(
            query.contains("origClientOrderId=bot_BTCUSDT_1_stop"),
            "{query}"
        );
    }
}
