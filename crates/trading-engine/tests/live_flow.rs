//! The live path end to end, against a real database and a flaky venue.
//!
//! ## The one property this file exists to prove
//!
//! **An order is placed at most once, even when the network fails while placing
//! it.** `docs/11` and `docs/15` both require it, and it is the kind of property
//! that a unit test can appear to verify while the assembled system violates
//! it: `execution.rs` proves the gateway's book is right, `db::live` proves the
//! insert is idempotent, and neither proves that a bot which hits a transport
//! failure and then flushes ends up with **one** row in `live_orders` and
//! **one** order at the venue. That is what is asserted here.
//!
//! ## Two shapes of the same failure, and they need opposite responses
//!
//! * The request never arrived. Asking the exchange finds nothing, so retrying
//!   with the *same* client id is correct -- and the id is what makes it safe.
//! * The request arrived and the answer did not. Asking the exchange finds the
//!   order, so retrying would be a second position.
//!
//! Both are driven here against a venue that dedups by client id exactly as
//! Binance does, so "how many orders exist" is counted by the exchange rather
//! than by us.
//!
//! ## It skips rather than fails without a database
//!
//! The suite has to stay runnable on a machine with no `DATABASE_URL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use analytics_core::types::{Candle, Timeframe};
use async_trait::async_trait;
use db::live::LiveOrderRow;
use db::Database;
use strategy_runtime::{RuntimeConfig, StrategyEngine};
use trading_engine::{
    client_order_id, ExchangeAdapter, ExecutionError, LiveBot, LiveConfig, LiveSession, OrderAck,
    OrderRequest, OrderStatus, OrderStatusReport, OrderType, RiskLimits,
};

/// An owner created by this file is named with this prefix so a previous run
/// that panicked before cleaning up can be swept away before it is counted.
const TEST_PREFIX: &str = "live-bot-test-";

const M5: i64 = 5 * 60 * 1_000_000_000;

/// The strategy the live bot runs.
///
/// The same shape the backtester's golden fixture is written against: a trend
/// filter on 1h and a sweep-and-reclaim on 5m. A monotonic ramp would never
/// produce a setup -- there are no sweeps in a line -- and a test that asserted
/// "no duplicate order" over a run that never placed one would pass for the
/// wrong reason.
const DOCUMENT: &str = r#"
name: "Live harness"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 1h
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close > liquidity.swept_level
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.0
"#;

// ---------------------------------------------------------------------------
// The venue
// ---------------------------------------------------------------------------

/// What the venue does when it is asked to place something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The request never arrived: reconcile will not find it.
    NeverArrived,
    /// The request arrived and the answer did not: reconcile finds it.
    AnswerLost,
}

/// A venue that dedups by client id, can lose one request, and can be asked
/// what it holds.
///
/// The dedup is not decoration. It is the exchange-side half of the guarantee,
/// and a mock that accepted a duplicate would make the whole test meaningless:
/// the assertion "exactly one order exists" would be measuring the mock.
struct FlakyVenue {
    /// Orders the exchange has accepted, keyed the way it keys them.
    accepted: Mutex<Vec<OrderRequest>>,
    /// Orders the exchange still holds, as `reconcile` reports them.
    open: Mutex<Vec<OrderStatusReport>>,
    /// What to do on the next `place_order`, consumed once.
    fail_next: Mutex<Option<Failure>>,
    /// Every call to `place_order`, failures included.
    calls: AtomicUsize,
    /// How many times each client id was asked for.
    ///
    /// The assertion that actually names the property: "this order was placed
    /// exactly once" is a statement about *one* id, and a total call count is
    /// not -- the first version of these tests asserted a total and was wrong
    /// because the strategy places three orders per entry (entry, stop, target)
    /// and a take-profit made the arithmetic read as a duplicate that was not
    /// there.
    calls_per_id: Mutex<Vec<(String, usize)>>,
    /// Whether `reconcile` should fail. A venue that is *down*, rather than one
    /// that lost a packet: the distinction is the whole difference between
    /// "ask again" and "stop and call an operator".
    reconcile_down: AtomicUsize,
}

impl FlakyVenue {
    fn new() -> Self {
        Self {
            accepted: Mutex::new(Vec::new()),
            open: Mutex::new(Vec::new()),
            fail_next: Mutex::new(None),
            calls: AtomicUsize::new(0),
            calls_per_id: Mutex::new(Vec::new()),
            reconcile_down: AtomicUsize::new(0),
        }
    }

    fn fail_next(&self, failure: Failure) {
        if let Ok(mut guard) = self.fail_next.lock() {
            *guard = Some(failure);
        }
    }

    /// Make every `reconcile` fail from now on.
    fn fail_reconcile_always(&self) {
        self.reconcile_down.store(usize::MAX, Ordering::SeqCst);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// How many times `place_order` was called for one client id.
    fn calls_for(&self, client_order_id: &str) -> usize {
        self.calls_per_id
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .find(|(id, _)| id == client_order_id)
                    .map_or(0, |(_, count)| *count)
            })
            .unwrap_or(0)
    }

    fn count_call(&self, client_order_id: &str) {
        if let Ok(mut guard) = self.calls_per_id.lock() {
            match guard.iter_mut().find(|(id, _)| id == client_order_id) {
                Some((_, count)) => *count += 1,
                None => guard.push((client_order_id.to_string(), 1)),
            }
        }
    }

    fn accepted(&self) -> Vec<String> {
        self.accepted
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .map(|order| order.client_order_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl ExchangeAdapter for FlakyVenue {
    fn venue(&self) -> &str {
        "binance"
    }

    async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.count_call(&order.client_order_id);

        let failure = self
            .fail_next
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(failure) = failure {
            if failure == Failure::AnswerLost {
                // It arrived. The exchange holds it; we just never heard so.
                self.record_accepted(&order);
                self.record_open(&order);
            }
            return Err(ExecutionError::Transport("connection reset".into()));
        }

        let market = matches!(order.order_type, OrderType::Market);
        self.record_accepted(&order);

        if market {
            // A market order fills immediately, at whatever the venue thinks
            // the price is. The bot tracks the candle close, so this is the
            // same number it sized against.
            return Ok(OrderAck {
                client_order_id: order.client_order_id,
                exchange_order_id: "EXCH-1".into(),
                status: OrderStatus::Filled,
                filled_qty: order.quantity,
                avg_price: Some(100.0),
            });
        }

        self.record_open(&order);
        Ok(OrderAck {
            client_order_id: order.client_order_id,
            exchange_order_id: "EXCH-1".into(),
            status: OrderStatus::New,
            filled_qty: 0.0,
            avg_price: None,
        })
    }

    async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError> {
        if let Ok(mut guard) = self.open.lock() {
            guard.retain(|report| report.client_order_id != client_order_id);
        }
        Ok(())
    }

    async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
        if self.reconcile_down.load(Ordering::SeqCst) > 0 {
            return Err(ExecutionError::Transport(
                "the venue is not answering".into(),
            ));
        }
        Ok(self.open.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |guard| guard.clone(),
        ))
    }
}

impl FlakyVenue {
    /// Record an order, unless the exchange already has that id.
    ///
    /// The dedup is the exchange-side half of the guarantee. A mock that
    /// accepted a duplicate would make "exactly one order exists" an assertion
    /// about the mock.
    fn record_accepted(&self, order: &OrderRequest) {
        if let Ok(mut guard) = self.accepted.lock() {
            if !guard
                .iter()
                .any(|held| held.client_order_id == order.client_order_id)
            {
                guard.push(order.clone());
            }
        }
    }

    /// Add an order to what `reconcile` reports, unless it is already there.
    fn record_open(&self, order: &OrderRequest) {
        if let Ok(mut guard) = self.open.lock() {
            if !guard
                .iter()
                .any(|held| held.client_order_id == order.client_order_id)
            {
                guard.push(OrderStatusReport {
                    client_order_id: order.client_order_id.clone(),
                    exchange_order_id: "EXCH-1".into(),
                    status: OrderStatus::New,
                    filled_qty: 0.0,
                    avg_price: None,
                });
            }
        }
    }

    /// Whether the venue currently holds an order with this client id.
    fn holds(&self, client_order_id: &str) -> bool {
        self.open
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .any(|report| report.client_order_id == client_order_id)
            })
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// The market
// ---------------------------------------------------------------------------

/// The staircase the backtester's golden fixture uses, copied here on purpose.
///
/// A monotonic ramp has no sweeps and no reclaims, so `liquidity.swept_level`
/// never resolves and the strategy never fires. The first version of these
/// tests drove a ramp and asserted that no duplicate order was placed, which was
/// true and proved nothing.
const CYCLE: usize = 120;
const DRIFT: f64 = 24.0;
const KEYFRAMES: [(usize, f64); 6] = [
    (0, 0.0),
    (40, 20.0),
    (55, 8.0),
    (95, 34.0),
    (105, 4.0),
    (119, 24.0),
];

fn offset(bar: usize) -> f64 {
    let position = bar % CYCLE;
    for window in KEYFRAMES.windows(2) {
        let ((from_bar, from), (to_bar, to)) = (window[0], window[1]);
        if position >= from_bar && position <= to_bar {
            let span = (to_bar - from_bar) as f64;
            return from + (to - from) * ((position - from_bar) as f64 / span);
        }
    }
    unreachable!()
}

fn price_at(bar: usize) -> f64 {
    100.0 + DRIFT * (bar / CYCLE) as f64 + offset(bar)
}

fn candle(bar: usize) -> Candle {
    let open = price_at(bar);
    let close = price_at(bar + 1);
    let wiggle = if bar % 2 == 0 { 0.3 } else { 0.6 };
    Candle {
        symbol: "BTCUSDT".into(),
        timeframe: Timeframe::M5,
        open_time: bar as i64 * M5,
        open,
        high: open.max(close) + wiggle,
        low: open.min(close) - wiggle,
        close,
        volume: 1_000.0,
        buy_volume: 700.0,
        sell_volume: 300.0,
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A live bot wired to the flaky venue, plus the row it writes through.
struct Fixture {
    database: Database,
    user_id: uuid::Uuid,
    bot_id: uuid::Uuid,
    session: LiveSession,
    bot: LiveBot<FlakyVenue>,
}

impl Fixture {
    /// Feed the fixture until the bot has opened a position.
    ///
    /// Returns how many decision bars it took, so a caller can assert the run
    /// was real rather than report a silent no-op.
    async fn run_until_entered(&mut self, max_bars: usize) -> usize {
        for bar in 0..max_bars {
            // The 1h candle the trend filter reads, built by resampling what
            // has been seen so far -- the same thing the feed does.
            if bar >= 11 && (bar - 11) % 12 == 0 {
                let seen: Vec<Candle> = (0..=bar).map(candle).collect();
                let hourly = analytics_core::resample(&seen, Timeframe::H1);
                if let Some(last) = hourly.last() {
                    self.bot.on_candle(last).await.expect("1h must not fail");
                }
            }
            self.bot
                .on_candle(&candle(bar))
                .await
                .expect("the run must not fail");
            if self.bot.position().is_some() {
                return bar;
            }
        }
        max_bars
    }

    /// The `live_orders` rows this bot has written.
    async fn live_orders(&self) -> Vec<LiveOrderRow> {
        db::live::list_live_orders(self.database.pool(), self.bot_id, 100)
            .await
            .expect("the order query must succeed")
    }

    /// Clean up, whether or not the assertions passed.
    async fn cleanup(self) {
        // The session may hold unflushed rows; dropping them is fine, the bot
        // row is about to go.
        drop(self.session);
        let _ = db::paper::purge_bot(self.database.pool(), self.bot_id).await;
        let _ = db::paper::purge_owner(self.database.pool(), self.user_id).await;
    }
}

async fn database() -> Option<Database> {
    let _ = dotenvy::dotenv();
    if std::env::var("DATABASE_URL").is_err() {
        return None;
    }
    let database = Database::from_env().await.ok()?;
    database.migrate().await.ok()?;
    let swept = db::paper::purge_owners_with_prefix(database.pool(), TEST_PREFIX, 60)
        .await
        .ok()?;
    if swept > 0 {
        eprintln!("swept {swept} stale test owner(s) from an earlier run");
    }
    Some(database)
}

async fn fixture(failure: Option<Failure>) -> Option<Fixture> {
    let database = database().await?;

    let owner = format!("{TEST_PREFIX}{}@local", uuid::Uuid::new_v4());
    let user_id = db::paper::create_owner(database.pool(), &owner)
        .await
        .expect("the owner must be created");

    let document = serde_json::to_value(strategy_dsl::parse(DOCUMENT).unwrap()).unwrap();
    let strategy_id = db::paper::find_or_create_strategy(
        database.pool(),
        user_id,
        "Live harness",
        "1",
        &document,
        "developer_sdk",
    )
    .await
    .expect("the strategy must be stored");

    let bot_id = db::paper::insert_bot(
        database.pool(),
        user_id,
        strategy_id,
        "live",
        Some("binance"),
    )
    .await
    .expect("the bot row must be created");

    let venue = FlakyVenue::new();
    if let Some(failure) = failure {
        venue.fail_next(failure);
    }

    let validated = strategy_dsl::parse_and_validate(DOCUMENT).expect("the document must validate");
    let engine =
        StrategyEngine::new(&validated, RuntimeConfig::default()).expect("it must be runnable");

    let bot = LiveBot::new(
        engine,
        venue,
        LiveConfig {
            symbol: "BTCUSDT".into(),
            bot_id: bot_id.to_string(),
            limits: RiskLimits::default(),
            rolling: strategy_runtime::RollingConfig::new(500, 200, Default::default()),
            equity: 10_000.0,
            min_quantity: 0.0001,
        },
    );

    let session = LiveSession::attach(
        &database,
        user_id,
        bot_id,
        serde_json::json!({"test": true}),
    )
    .await
    .expect("the session must attach");

    Some(Fixture {
        database,
        user_id,
        bot_id,
        session,
        bot,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The request never arrived: asking the exchange finds nothing, so the order
/// is placed -- once -- with the id it already had.
#[tokio::test]
async fn a_lost_request_is_retried_once_and_the_venue_ends_up_with_one_order() {
    let Some(mut fixture) = fixture(Some(Failure::NeverArrived)).await else {
        eprintln!("DATABASE_URL is not set; skipping the live idempotency test");
        return;
    };

    let bars = fixture.run_until_entered(600).await;
    assert!(
        fixture.bot.position().is_some(),
        "the fixture must open a position or this test proves nothing (ran {bars} bars)"
    );

    let accepted = fixture.bot.adapter().accepted();
    let entries: Vec<&String> = accepted
        .iter()
        .filter(|id| id.ends_with("_entry"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "the venue must hold exactly one entry order, not two: {accepted:?}"
    );
    let entry_id = entries[0].clone();

    // The property, stated about the one order it is about: the entry was asked
    // for exactly twice -- once lost, once retried with the same id. A total
    // call count cannot say this, because the strategy places three orders per
    // entry and the arithmetic would then depend on how many it placed.
    assert_eq!(
        fixture.bot.adapter().calls_for(&entry_id),
        2,
        "the entry must be one lost request and exactly one retry"
    );
    for (id, count) in [("stop", "_stop"), ("target", "_targe")] {
        let placed: Vec<&String> = accepted.iter().filter(|o| o.ends_with(count)).collect();
        assert_eq!(placed.len(), 1, "one {id} per entry: {accepted:?}");
        assert_eq!(
            fixture.bot.adapter().calls_for(placed[0]),
            1,
            "the {id} must be placed once"
        );
    }

    assert!(
        fixture
            .bot
            .orders()
            .iter()
            .any(|(request, _)| request.client_order_id == entry_id),
        "the bot's book must name the same order the venue holds"
    );

    // --- what the database holds ---
    fixture
        .session
        .flush(&mut fixture.bot)
        .await
        .expect("flush");
    let rows = fixture.live_orders().await;

    let entry_rows: Vec<&LiveOrderRow> = rows
        .iter()
        .filter(|row| row.client_order_id == entry_id)
        .collect();
    assert_eq!(
        entry_rows.len(),
        1,
        "one entry order means one `live_orders` row: {:?}",
        rows.iter().map(|r| &r.client_order_id).collect::<Vec<_>>()
    );
    assert_eq!(
        rows.len(),
        accepted.len(),
        "every row is an order the venue accepted and vice versa: {:?} vs {accepted:?}",
        rows.iter().map(|r| &r.client_order_id).collect::<Vec<_>>()
    );

    fixture.cleanup().await;
}

/// The request arrived and the answer did not: the order is **adopted**, not
/// placed again. This is the case that a retry policy gets wrong.
#[tokio::test]
async fn an_order_the_venue_already_has_is_adopted_rather_than_duplicated() {
    let Some(mut fixture) = fixture(Some(Failure::AnswerLost)).await else {
        eprintln!("DATABASE_URL is not set; skipping the live idempotency test");
        return;
    };

    let bars = fixture.run_until_entered(600).await;
    assert!(
        fixture.bot.position().is_some(),
        "the fixture must open a position or this test proves nothing (ran {bars} bars)"
    );

    let calls = fixture.bot.adapter().calls();
    let accepted = fixture.bot.adapter().accepted();

    let entry_id = accepted
        .iter()
        .find(|id| id.ends_with("_entry"))
        .expect("the entry must be among the accepted orders")
        .clone();

    // One call for the entry, and it failed. The recovery found the order at
    // the venue, so no second placement was attempted -- which is exactly the
    // difference between this test and the one above. The stop and the target
    // are placed normally, so the total is three; the *entry* is one.
    assert_eq!(
        calls, 3,
        "one failed entry, then the stop and the target: {accepted:?}"
    );
    assert_eq!(
        fixture.bot.adapter().calls_for(&entry_id),
        1,
        "an order the venue already has must be adopted, never re-placed"
    );

    // The protective stop is resting at the venue, which is the other half of
    // an entry: an adopted position with no stop would be the unprotected case.
    let stop_id = fixture
        .bot
        .position()
        .expect("a position")
        .stop_order_id
        .clone();
    assert!(
        fixture.bot.adapter().holds(&stop_id),
        "the stop must be resting at the venue while the position is open"
    );
    let (_, ack) = fixture
        .bot
        .orders()
        .into_iter()
        .find(|(request, _)| request.client_order_id == entry_id)
        .expect("the adopted order must be in the bot's book");
    assert_eq!(
        ack.exchange_order_id, "EXCH-1",
        "the exchange's id came from their answer, not from ours"
    );

    fixture
        .session
        .flush(&mut fixture.bot)
        .await
        .expect("flush");
    let rows = fixture.live_orders().await;
    assert_eq!(
        rows.len(),
        accepted.len(),
        "one row per order the venue holds, and no more: {:?} vs {accepted:?}",
        rows.iter().map(|r| &r.client_order_id).collect::<Vec<_>>()
    );
    let entry_row = rows
        .iter()
        .find(|row| row.client_order_id == entry_id)
        .expect("the entry must have a row");
    assert_eq!(entry_row.venue, "binance");
    assert_eq!(
        entry_row.side, "BUY",
        "the side comes from the request, not the ack"
    );
    assert_eq!(entry_row.order_type, "MARKET");

    fixture.cleanup().await;
}

/// Flushing twice must not write the order twice.
///
/// The session writes from the gateway's book, and that book holds every order
/// for the life of the bot -- so a flush that re-wrote all of them would be a
/// statement per order per tick. The dedup is what makes the flush cheap, and
/// the property that makes it *safe* is that the row count does not move.
#[tokio::test]
async fn a_second_flush_does_not_write_the_order_again() {
    let Some(mut fixture) = fixture(None).await else {
        eprintln!("DATABASE_URL is not set; skipping the live persistence test");
        return;
    };

    let bars = fixture.run_until_entered(600).await;
    assert!(fixture.bot.position().is_some(), "ran {bars} bars");

    fixture
        .session
        .flush(&mut fixture.bot)
        .await
        .expect("flush");
    let first = fixture.live_orders().await;
    assert!(!first.is_empty(), "the run must have placed something");

    fixture
        .session
        .flush(&mut fixture.bot)
        .await
        .expect("flush");
    let second = fixture.live_orders().await;
    assert_eq!(
        second.len(),
        first.len(),
        "a flush with nothing new must not add rows"
    );

    // And the run is recorded: a decision trail with no `bot.started` cannot
    // tell a clean run from one that died before it began.
    let started =
        db::paper::count_audit_events(fixture.database.pool(), fixture.user_id, "bot.started")
            .await
            .unwrap();
    assert_eq!(started, 1);

    let decisions = db::paper::count_audit_events(
        fixture.database.pool(),
        fixture.user_id,
        "bot.live_decision",
    )
    .await
    .unwrap();
    assert!(decisions > 0, "every decision bar must have a row");

    fixture.cleanup().await;
}

/// A transport failure the venue cannot answer either must stop the bot rather
/// than place blind.
///
/// Not knowing is not permission. The order's fate is unknown, and the only
/// safe response is to stop and let an operator reconcile.
#[tokio::test]
async fn when_the_venue_cannot_be_asked_the_bot_stops() {
    let Some(mut fixture) = fixture(None).await else {
        eprintln!("DATABASE_URL is not set; skipping the live idempotency test");
        return;
    };

    // Both the placement and the question fail, which is a venue that is down
    // rather than one that lost a packet.
    fixture.bot.adapter().fail_next(Failure::NeverArrived);
    fixture.bot.adapter().fail_reconcile_always();

    let error = fixture
        .run_until_error(600)
        .await
        .expect("a bot that cannot ask must stop rather than trade blind");

    assert!(
        matches!(error, ExecutionError::Transport(_)),
        "the error must be the transport failure, got {error:?}"
    );
    assert!(
        fixture.bot.adapter().accepted().is_empty(),
        "nothing may be placed when the venue cannot be asked"
    );

    fixture.cleanup().await;
}

impl Fixture {
    /// Feed the fixture until a decision fails, and return the error.
    async fn run_until_error(&mut self, max_bars: usize) -> Option<ExecutionError> {
        for bar in 0..max_bars {
            if bar >= 11 && (bar - 11) % 12 == 0 {
                let seen: Vec<Candle> = (0..=bar).map(candle).collect();
                let hourly = analytics_core::resample(&seen, Timeframe::H1);
                if let Some(last) = hourly.last() {
                    if let Err(error) = self.bot.on_candle(last).await {
                        return Some(error);
                    }
                }
            }
            if let Err(error) = self.bot.on_candle(&candle(bar)).await {
                return Some(error);
            }
        }
        None
    }
}

/// A client order id is derived from the decision, so a retry regenerates it.
///
/// Asserted here as well as in the unit tests because it is the property the
/// two tests above rest on, and a change that made it non-deterministic would
/// leave them passing while the guarantee was gone.
#[test]
fn the_client_order_id_a_retry_would_regenerate_is_the_same_one() {
    let first = client_order_id("bot-1", "BTCUSDT", 1_700_000_000_000_000_000, "entry");
    let second = client_order_id("bot-1", "BTCUSDT", 1_700_000_000_000_000_000, "entry");
    assert_eq!(first, second);
}
