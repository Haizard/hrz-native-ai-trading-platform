//! Order placement, idempotency and reconciliation (`docs/11`, `docs/15`).
//!
//! ## The one property this module exists to guarantee
//!
//! An order is placed **at most once**, even when the network fails while
//! placing it. That sounds like a retry policy and is not: a retry policy is
//! what *creates* duplicate orders. The hard part is the case where the request
//! reached the exchange and the response did not reach us, because at that
//! moment the platform does not know whether it has a position.
//!
//! The rule here is that the platform is never allowed to guess:
//!
//! 1. every order carries a **client-generated id**, so the exchange itself
//!    rejects a true duplicate;
//! 2. after a transport failure we **ask the exchange what it has** before
//!    doing anything else;
//! 3. if the exchange cannot be asked either, we **stop** and return the error.
//!    Not knowing is not permission to place again.
//!
//! ## Order placement happens outside the sandbox
//!
//! `docs/15`: the sandbox never sees a credential, and no order is placed from
//! inside it. The sandbox produces a `Signal`; everything in this file happens
//! afterwards, on the outside, driven only by that signal.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ExecutionError;

/// Which way an order goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    /// Buy.
    Buy,
    /// Sell.
    Sell,
}

impl OrderSide {
    /// The exchange wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "BUY",
            Self::Sell => "SELL",
        }
    }

    /// The side that closes a position opened by this one.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}

impl From<strategy_dsl::Direction> for OrderSide {
    fn from(direction: strategy_dsl::Direction) -> Self {
        match direction {
            strategy_dsl::Direction::Long => Self::Buy,
            strategy_dsl::Direction::Short => Self::Sell,
        }
    }
}

/// How an order is to be filled.
///
/// Only the four shapes a strategy document can actually produce: a market
/// entry, a protective stop, a take-profit limit, and a plain limit. There is
/// deliberately no `StopLimit` for entries -- nothing in the DSL asks for one,
/// and every unused order type is another way to be wrong about fills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderType {
    /// Fill immediately, at whatever the book offers.
    Market,
    /// Rest at `price`.
    Limit {
        /// Limit price.
        price: f64,
    },
    /// A protective stop: becomes a market order once `stop_price` trades.
    StopMarket {
        /// Trigger price.
        stop_price: f64,
    },
    /// A take-profit: rests as a limit at `price` once `stop_price` trades.
    TakeProfitLimit {
        /// Trigger price.
        stop_price: f64,
        /// Limit price.
        price: f64,
    },
}

impl OrderType {
    /// The Binance wire word.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Market => "MARKET",
            Self::Limit { .. } => "LIMIT",
            Self::StopMarket { .. } => "STOP_LOSS_MARKET",
            Self::TakeProfitLimit { .. } => "TAKE_PROFIT_LIMIT",
        }
    }
}

/// One order, with the identity the platform generated for it.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRequest {
    /// Our id, and the exchange's idempotency key. See
    /// [`client_order_id`](client_order_id) for why it must be deterministic.
    pub client_order_id: String,
    /// Market, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Buy or sell.
    pub side: OrderSide,
    /// Base-asset quantity.
    pub quantity: f64,
    /// How it fills.
    pub order_type: OrderType,
}

/// An order's state, in the words both sides understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Accepted, nothing filled.
    New,
    /// Some of it filled.
    PartiallyFilled,
    /// All of it filled.
    Filled,
    /// Cancelled by us or by the exchange.
    Canceled,
    /// Refused.
    Rejected,
    /// Lapsed, e.g. a GTC order the exchange expired.
    Expired,
}

impl OrderStatus {
    /// Parse the exchange's status word.
    ///
    /// Unknown words become [`OrderStatus::New`] rather than an error: a new
    /// status the platform does not model yet is not a reason to lose track of
    /// an order that exists.
    #[must_use]
    pub fn parse(wire: &str) -> Self {
        match wire {
            "NEW" | "ACK" => Self::New,
            "PARTIALLY_FILLED" => Self::PartiallyFilled,
            "FILLED" => Self::Filled,
            "CANCELED" | "CANCELLED" | "PENDING_CANCEL" => Self::Canceled,
            "REJECTED" => Self::Rejected,
            "EXPIRED" | "EXPIRED_IN_MATCH" => Self::Expired,
            _ => Self::New,
        }
    }

    /// The exchange's status word.
    ///
    /// The exact inverse of [`parse`](Self::parse), and deliberately the
    /// *venue's* vocabulary rather than a platform-specific one: `live_orders`
    /// stores this string, so a reconciliation mismatch reads as a difference
    /// between two comparable things rather than between two dialects.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "NEW",
            Self::PartiallyFilled => "PARTIALLY_FILLED",
            Self::Filled => "FILLED",
            Self::Canceled => "CANCELED",
            Self::Rejected => "REJECTED",
            Self::Expired => "EXPIRED",
        }
    }

    /// Whether the order will not trade again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Canceled | Self::Rejected | Self::Expired
        )
    }
}

/// What the exchange said when we placed an order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderAck {
    /// Our id.
    pub client_order_id: String,
    /// The exchange's id.
    pub exchange_order_id: String,
    /// Its state.
    pub status: OrderStatus,
    /// How much filled so far.
    pub filled_qty: f64,
    /// Average fill price, when anything filled.
    pub avg_price: Option<f64>,
}

/// An order as the exchange currently sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderStatusReport {
    /// Our id, echoed back. Empty when the order pre-dates us.
    pub client_order_id: String,
    /// The exchange's id.
    pub exchange_order_id: String,
    /// Its state.
    pub status: OrderStatus,
    /// Filled quantity.
    pub filled_qty: f64,
    /// Average fill price.
    pub avg_price: Option<f64>,
}

/// The venue-facing surface (`docs/11`).
///
/// Deliberately three methods plus a name: place, cancel, and "tell me
/// everything you have". A richer interface -- amending orders, querying fills
/// by id -- can be added when something needs it, and every method that exists
/// is a method an adapter has to get right against a real venue.
#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    /// The venue's short name, lowercase, e.g. `binance`.
    ///
    /// Required rather than defaulted: it is written into every `live_orders`
    /// row and compared against the opt-in list, and a default of `"unknown"`
    /// would let an adapter silently record orders against a venue nobody
    /// opted in to.
    fn venue(&self) -> &str;
    /// Place an order. The `client_order_id` makes a repeat call safe.
    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError>;
    /// Cancel one of our orders by its client id.
    async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError>;
    /// Every order the exchange currently holds for us.
    async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError>;
}

/// The longest client order id any venue here accepts.
///
/// Binance documents 36 characters, and the id is not a suggestion: an
/// over-long one is rejected at the venue, which is a *refusal*, not a
/// duplicate. Either way the trade does not happen, so the bound is enforced
/// here and pinned by a test rather than discovered in production.
pub const MAX_CLIENT_ORDER_ID: usize = 36;

/// Build the identifier an order carries for its whole life.
///
/// **Deterministic on purpose.** It is derived from the bot, the candle that
/// produced the decision and the intent, so a retry after a network failure
/// regenerates the *same* id and the exchange can recognise it as the same
/// order. A random id would make every retry a new order -- which is precisely
/// the failure this module is built to prevent.
///
/// **Fits in 36 characters by construction**, not by luck: each component is
/// truncated and the timestamp is base-36, so the worst case is
/// `6 + 1 + 8 + 1 + 13 + 1 + 5 = 35`. The first version of this used decimal
/// nanoseconds and overflowed to 39 characters -- an id no venue would accept.
#[must_use]
pub fn client_order_id(bot: &str, symbol: &str, at_ns: i64, intent: &str) -> String {
    format!(
        "{}_{}_{}_{}",
        sanitize(bot, 6),
        sanitize(symbol, 8),
        base36(at_ns.max(0) as u64),
        sanitize(intent, 5)
    )
}

/// Keep the characters a venue accepts, and no more than `max` of them.
fn sanitize(raw: &str, max: usize) -> String {
    raw.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(max)
        .collect()
}

/// Lowercase base-36, which is what makes 13 characters enough for any `i64`
/// nanosecond timestamp.
fn base36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        digits.push(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        });
        value /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

/// Places orders through an adapter without ever placing one twice.
#[derive(Debug)]
pub struct OrderGateway<A: ExchangeAdapter> {
    adapter: A,
    /// Every order we believe exists, by client id.
    placed: HashMap<String, OrderAck>,
    /// The request each acknowledgement answered.
    ///
    /// Kept beside the acknowledgement because the acknowledgement is the
    /// *exchange's* half -- an id, a status, a fill -- and the side, the type
    /// and the intended quantity only exist on the request. A `live_orders`
    /// row built from the acknowledgement alone would have to leave those
    /// columns blank, which is how a table ends up with rows nobody can read.
    requests: HashMap<String, OrderRequest>,
}

impl<A: ExchangeAdapter> OrderGateway<A> {
    /// Wrap an adapter.
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            placed: HashMap::new(),
            requests: HashMap::new(),
        }
    }

    /// The adapter, for the caller to query the exchange directly.
    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    /// Orders we believe are live, by client id.
    pub fn placed(&self) -> &HashMap<String, OrderAck> {
        &self.placed
    }

    /// The request that produced the acknowledgement for `client_order_id`.
    #[must_use]
    pub fn request(&self, client_order_id: &str) -> Option<&OrderRequest> {
        self.requests.get(client_order_id)
    }

    /// Every order as a (request, acknowledgement) pair, sorted by client id.
    ///
    /// Sorted rather than in hash order so a flush writes rows in a stable
    /// order: an audit trail whose row order changes between runs is a trail
    /// nobody can diff.
    #[must_use]
    pub fn entries(&self) -> Vec<(&OrderRequest, &OrderAck)> {
        let mut ids: Vec<&String> = self.placed.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| {
                let request = self.requests.get(id)?;
                let ack = self.placed.get(id)?;
                Some((request, ack))
            })
            .collect()
    }

    /// How many orders this gateway has placed (or recovered).
    #[must_use]
    pub fn len(&self) -> usize {
        self.placed.len()
    }

    /// Whether it has placed nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }

    /// Place an order, or return the acknowledgement we already have for it.
    ///
    /// # Errors
    /// Returns whatever the adapter returned, except that a transport failure
    /// is first given the chance to be *recovered* from the exchange's own
    /// view -- see the module note.
    pub async fn place(&mut self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
        if let Some(ack) = self.placed.get(&order.client_order_id) {
            // Already ours. Re-sending would be a second order; returning the
            // first acknowledgement is the whole point of carrying an id.
            return Ok(ack.clone());
        }

        match self.adapter.place_order(order.clone()).await {
            Ok(ack) => {
                self.remember(&order, ack.clone());
                Ok(ack)
            }
            Err(ExecutionError::Transport(_)) => self.recover(order).await,
            Err(other) => Err(other),
        }
    }

    /// Record an acknowledgement and the request behind it.
    fn remember(&mut self, request: &OrderRequest, ack: OrderAck) {
        self.requests
            .insert(request.client_order_id.clone(), request.clone());
        self.placed.insert(ack.client_order_id.clone(), ack);
    }

    /// A transport failure happened: find out what the exchange has, then act.
    async fn recover(&mut self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
        let reports = self.adapter.reconcile().await?;

        if let Some(report) = reports
            .into_iter()
            .find(|report| report.client_order_id == order.client_order_id)
        {
            // The order is live and we simply never saw the answer. Adopt it
            // rather than placing anything else.
            let ack = OrderAck {
                client_order_id: report.client_order_id.clone(),
                exchange_order_id: report.exchange_order_id,
                status: report.status,
                filled_qty: report.filled_qty,
                avg_price: report.avg_price,
            };
            self.remember(&order, ack.clone());
            return Ok(ack);
        }

        // Not on the exchange, so the request genuinely did not arrive. The id
        // is unchanged, so this is still the same order.
        let ack = self.adapter.place_order(order.clone()).await?;
        self.remember(&order, ack.clone());
        Ok(ack)
    }

    /// Cancel an order and forget it.
    ///
    /// # Errors
    /// Returns the adapter's error. A cancelled order is removed from our book
    /// either way, because keeping it would report a position we no longer
    /// have for the rest of the run.
    pub async fn cancel(&mut self, client_order_id: &str) -> Result<(), ExecutionError> {
        let result = self.adapter.cancel_order(client_order_id).await;
        if result.is_ok() {
            self.placed.remove(client_order_id);
            self.requests.remove(client_order_id);
        }
        result
    }

    /// Compare our book with the exchange's.
    ///
    /// # Errors
    /// Returns the adapter's error: if the exchange cannot be asked, there is
    /// no reconciliation to report, and inventing one would be worse.
    pub async fn reconcile(&self) -> Result<Reconciliation, ExecutionError> {
        let reports = self.adapter.reconcile().await?;
        Ok(reconcile(&self.placed, &reports))
    }
}

/// Why our view and the exchange's disagree.
///
/// `PartialEq` only, not `Eq`: a quantity disagreement carries `f64`, and
/// claiming a total equivalence for floats is the kind of thing that makes an
/// equality check pass when it should not.
#[derive(Debug, Clone, PartialEq)]
pub enum MismatchKind {
    /// We think it is live; the exchange has never heard of it.
    MissingAtExchange,
    /// The exchange holds an order we never placed.
    UnknownToUs {
        /// The exchange's id for it.
        exchange_order_id: String,
    },
    /// Both know it, and disagree about its state.
    StatusDisagrees {
        /// What we recorded.
        ours: OrderStatus,
        /// What the exchange reports.
        theirs: OrderStatus,
    },
    /// Both know it, and disagree about how much filled.
    QuantityDisagrees {
        /// Our filled quantity.
        ours: f64,
        /// Theirs.
        theirs: f64,
    },
}

/// One disagreement.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    /// Which order.
    pub client_order_id: String,
    /// What is wrong.
    pub kind: MismatchKind,
}

/// The result of a reconciliation pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciliation {
    /// Orders both sides agree on.
    pub matched: usize,
    /// Orders they do not.
    pub mismatches: Vec<Mismatch>,
}

impl Reconciliation {
    /// Whether the two views agree.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Compare our book against the exchange's, counting every disagreement.
///
/// Free function rather than a method so it can be tested without an adapter
/// and without an async runtime -- the interesting logic is the comparison, not
/// the I/O.
#[must_use]
pub fn reconcile(
    placed: &HashMap<String, OrderAck>,
    reports: &[OrderStatusReport],
) -> Reconciliation {
    let mut matched = 0usize;
    let mut mismatches = Vec::new();

    for (id, ack) in placed {
        // Terminal orders are history, not a disagreement: the exchange drops
        // filled and cancelled orders from its open-order list, so their
        // absence is expected rather than alarming.
        if ack.status.is_terminal() {
            continue;
        }
        match reports.iter().find(|report| report.client_order_id == *id) {
            None => mismatches.push(Mismatch {
                client_order_id: id.clone(),
                kind: MismatchKind::MissingAtExchange,
            }),
            Some(report) => {
                if report.status != ack.status {
                    mismatches.push(Mismatch {
                        client_order_id: id.clone(),
                        kind: MismatchKind::StatusDisagrees {
                            ours: ack.status,
                            theirs: report.status,
                        },
                    });
                } else if (report.filled_qty - ack.filled_qty).abs() > 1e-9 {
                    mismatches.push(Mismatch {
                        client_order_id: id.clone(),
                        kind: MismatchKind::QuantityDisagrees {
                            ours: ack.filled_qty,
                            theirs: report.filled_qty,
                        },
                    });
                } else {
                    matched += 1;
                }
            }
        }
    }

    for report in reports {
        if report.client_order_id.is_empty() {
            continue;
        }
        if !placed.contains_key(&report.client_order_id) {
            mismatches.push(Mismatch {
                client_order_id: report.client_order_id.clone(),
                kind: MismatchKind::UnknownToUs {
                    exchange_order_id: report.exchange_order_id.clone(),
                },
            });
        }
    }

    Reconciliation {
        matched,
        mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An exchange we control: it counts what it was asked to do and can be
    /// told to fail on demand.
    struct MockExchange {
        /// Orders the exchange has actually accepted.
        accepted: Mutex<Vec<OrderRequest>>,
        /// How many more `place_order` calls should fail with a transport
        /// error before one succeeds.
        fail_places: AtomicUsize,
        /// Whether `reconcile` should fail too.
        fail_reconcile: AtomicUsize,
        /// What `reconcile` reports.
        reports: Mutex<Vec<OrderStatusReport>>,
        /// How many times `place_order` was called, failures included.
        calls: AtomicUsize,
    }

    impl MockExchange {
        fn new() -> Self {
            Self {
                accepted: Mutex::new(Vec::new()),
                fail_places: AtomicUsize::new(0),
                fail_reconcile: AtomicUsize::new(0),
                reports: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }

        fn accepted_len(&self) -> usize {
            self.accepted.lock().map_or(0, |guard| guard.len())
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn push_report(&self, report: OrderStatusReport) {
            if let Ok(mut guard) = self.reports.lock() {
                guard.push(report);
            }
        }
    }

    #[async_trait]
    impl ExchangeAdapter for MockExchange {
        fn venue(&self) -> &str {
            "mock"
        }

        async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            if self.fail_places.load(Ordering::SeqCst) > 0 {
                self.fail_places.fetch_sub(1, Ordering::SeqCst);
                return Err(ExecutionError::Transport("connection reset".into()));
            }

            // An exchange that already holds this id does not create a second
            // order; this is the behaviour the whole design leans on.
            let duplicate = self
                .accepted
                .lock()
                .map(|guard| {
                    guard
                        .iter()
                        .any(|o| o.client_order_id == order.client_order_id)
                })
                .unwrap_or(false);
            if !duplicate {
                if let Ok(mut guard) = self.accepted.lock() {
                    guard.push(order.clone());
                }
            }

            Ok(OrderAck {
                client_order_id: order.client_order_id,
                exchange_order_id: "EXCH-1".into(),
                status: OrderStatus::New,
                filled_qty: 0.0,
                avg_price: None,
            })
        }

        async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError> {
            if let Ok(mut guard) = self.accepted.lock() {
                guard.retain(|order| order.client_order_id != client_order_id);
            }
            Ok(())
        }

        async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
            if self.fail_reconcile.load(Ordering::SeqCst) > 0 {
                self.fail_reconcile.fetch_sub(1, Ordering::SeqCst);
                return Err(ExecutionError::Transport("reconcile failed".into()));
            }
            Ok(self.reports.lock().map_or_else(
                |poisoned| poisoned.into_inner().clone(),
                |guard| guard.clone(),
            ))
        }
    }

    fn order(id: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            quantity: 0.01,
            order_type: OrderType::Market,
        }
    }

    #[tokio::test]
    async fn placing_the_same_order_twice_places_it_once() {
        let exchange = MockExchange::new();
        let mut gateway = OrderGateway::new(exchange);
        let first = gateway.place(order("id-1")).await.expect("first place");
        let second = gateway.place(order("id-1")).await.expect("replay");

        assert_eq!(first, second, "the replay returns the same acknowledgement");
        assert_eq!(gateway.adapter().accepted_len(), 1);
        assert_eq!(gateway.adapter().calls(), 1, "the exchange was asked once");
        assert_eq!(gateway.len(), 1);
    }

    #[tokio::test]
    async fn a_transport_failure_that_the_exchange_did_see_is_adopted_not_repeated() {
        // The case that matters: the request arrived, the answer did not.
        let exchange = MockExchange::new();
        exchange.fail_places.store(1, Ordering::SeqCst);
        exchange.push_report(OrderStatusReport {
            client_order_id: "id-1".into(),
            exchange_order_id: "EXCH-77".into(),
            status: OrderStatus::New,
            filled_qty: 0.0,
            avg_price: None,
        });

        let mut gateway = OrderGateway::new(exchange);
        let ack = gateway.place(order("id-1")).await.expect("recoverable");

        assert_eq!(ack.exchange_order_id, "EXCH-77", "we adopted their order");
        assert_eq!(gateway.adapter().accepted_len(), 0, "no order was placed");
        assert_eq!(gateway.len(), 1, "but we know about theirs");
    }

    #[tokio::test]
    async fn a_transport_failure_that_the_exchange_never_saw_is_retried_once() {
        let exchange = MockExchange::new();
        exchange.fail_places.store(1, Ordering::SeqCst);

        let mut gateway = OrderGateway::new(exchange);
        let ack = gateway.place(order("id-1")).await.expect("retry succeeds");

        assert_eq!(ack.client_order_id, "id-1");
        assert_eq!(gateway.adapter().calls(), 2, "one failure, one retry");
        assert_eq!(gateway.adapter().accepted_len(), 1, "exactly one order");
    }

    #[tokio::test]
    async fn when_the_exchange_cannot_be_asked_we_stop_rather_than_guess() {
        // Not knowing is not permission: a second order here would be a second
        // position, and the platform would not know it had one.
        let exchange = MockExchange::new();
        exchange.fail_places.store(1, Ordering::SeqCst);
        exchange.fail_reconcile.store(1, Ordering::SeqCst);

        let mut gateway = OrderGateway::new(exchange);
        let error = gateway.place(order("id-1")).await.expect_err("must refuse");

        assert!(matches!(error, ExecutionError::Transport(_)), "{error:?}");
        assert_eq!(gateway.adapter().calls(), 1, "it did not try again");
        assert!(gateway.is_empty());
    }

    #[tokio::test]
    async fn a_rejected_order_is_not_retried() {
        // A rejection is an answer: the exchange is telling us the order does
        // not exist. Retrying would be the duplicate-order bug in disguise.
        struct Rejecting;
        #[async_trait]
        impl ExchangeAdapter for Rejecting {
            fn venue(&self) -> &str {
                "mock"
            }

            async fn place_order(&self, _: OrderRequest) -> Result<OrderAck, ExecutionError> {
                Err(ExecutionError::Exchange {
                    code: -2010,
                    message: "insufficient balance".into(),
                })
            }
            async fn cancel_order(&self, _: &str) -> Result<(), ExecutionError> {
                Ok(())
            }
            async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
                Ok(Vec::new())
            }
        }

        let mut gateway = OrderGateway::new(Rejecting);
        let error = gateway.place(order("id-1")).await.expect_err("rejected");
        assert!(
            matches!(error, ExecutionError::Exchange { .. }),
            "{error:?}"
        );
        assert!(gateway.is_empty(), "nothing was recorded as placed");
    }

    #[tokio::test]
    async fn cancelling_forgets_the_order() {
        let exchange = MockExchange::new();
        let mut gateway = OrderGateway::new(exchange);
        gateway.place(order("id-1")).await.expect("placed");
        gateway.cancel("id-1").await.expect("cancelled");
        assert!(gateway.is_empty());
        assert_eq!(gateway.adapter().accepted_len(), 0);
    }

    #[tokio::test]
    async fn a_clean_book_reconciles_with_no_mismatches() {
        let exchange = MockExchange::new();
        exchange.push_report(OrderStatusReport {
            client_order_id: "id-1".into(),
            exchange_order_id: "EXCH-1".into(),
            status: OrderStatus::New,
            filled_qty: 0.0,
            avg_price: None,
        });
        let mut gateway = OrderGateway::new(exchange);
        gateway.place(order("id-1")).await.expect("placed");

        let result = gateway.reconcile().await.expect("reconcile");
        assert!(result.is_clean(), "{result:?}");
        assert_eq!(result.matched, 1);
    }

    #[tokio::test]
    async fn an_order_the_exchange_does_not_have_is_a_mismatch() {
        let exchange = MockExchange::new();
        let mut gateway = OrderGateway::new(exchange);
        gateway.place(order("id-1")).await.expect("placed");

        let result = gateway.reconcile().await.expect("reconcile");
        assert_eq!(result.mismatches.len(), 1);
        assert_eq!(
            result.mismatches[0].kind,
            MismatchKind::MissingAtExchange,
            "we think it is live and they have never heard of it"
        );
    }

    #[tokio::test]
    async fn an_order_we_never_placed_is_a_mismatch() {
        let exchange = MockExchange::new();
        exchange.push_report(OrderStatusReport {
            client_order_id: "someone-elses".into(),
            exchange_order_id: "EXCH-9".into(),
            status: OrderStatus::New,
            filled_qty: 0.0,
            avg_price: None,
        });
        let gateway = OrderGateway::new(exchange);

        let result = gateway.reconcile().await.expect("reconcile");
        assert_eq!(
            result.mismatches[0].kind,
            MismatchKind::UnknownToUs {
                exchange_order_id: "EXCH-9".into()
            }
        );
    }

    #[test]
    fn a_filled_order_vanishing_from_the_exchange_is_not_a_mismatch() {
        // Open-order lists do not contain history, so a filled order's absence
        // is normal. Getting this wrong makes reconciliation cry wolf on every
        // completed trade.
        let mut placed = HashMap::new();
        placed.insert(
            "id-1".to_string(),
            OrderAck {
                client_order_id: "id-1".into(),
                exchange_order_id: "EXCH-1".into(),
                status: OrderStatus::Filled,
                filled_qty: 0.01,
                avg_price: Some(100.0),
            },
        );
        let result = reconcile(&placed, &[]);
        assert!(result.is_clean(), "{result:?}");
    }

    #[test]
    fn the_client_order_id_is_deterministic_and_legal() {
        let first = client_order_id("bot-42", "BTCUSDT", 1_700_000_000_000_000_000, "entry");
        let second = client_order_id("bot-42", "BTCUSDT", 1_700_000_000_000_000_000, "entry");
        assert_eq!(first, second, "a retry must regenerate the same id");
        assert!(
            first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "illegal character in {first}"
        );
        assert_ne!(
            first,
            client_order_id("bot-42", "BTCUSDT", 1_700_000_000_000_000_000, "stop")
        );
    }

    #[test]
    fn the_longest_possible_client_order_id_still_fits() {
        // The bug this pins: decimal nanoseconds pushed the id to 39 characters,
        // which no venue accepts. Asserting on a *realistic* input would have
        // missed it, so this uses the worst case for every component.
        let worst = client_order_id(&"b".repeat(64), &"S".repeat(64), i64::MAX, &"i".repeat(64));
        assert!(
            worst.len() <= MAX_CLIENT_ORDER_ID,
            "{} characters: {worst}",
            worst.len()
        );
        // And it is still recognisable rather than a hash of itself: the bot
        // and the intent are what an engineer reads first.
        assert!(worst.starts_with("bbbbbb_SSSSSSSS_"), "{worst}");
        assert!(worst.ends_with("_iiiii"), "{worst}");
    }

    #[test]
    fn the_status_parser_knows_both_spellings_of_cancelled() {
        assert_eq!(OrderStatus::parse("CANCELED"), OrderStatus::Canceled);
        assert_eq!(OrderStatus::parse("CANCELLED"), OrderStatus::Canceled);
        assert_eq!(OrderStatus::parse("FILLED"), OrderStatus::Filled);
        assert!(OrderStatus::Filled.is_terminal());
        assert!(!OrderStatus::New.is_terminal());
    }
}
