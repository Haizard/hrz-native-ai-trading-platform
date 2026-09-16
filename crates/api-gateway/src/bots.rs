//! Running paper and live bots (`docs/11-BOT-TRADING-ENGINE.md`,
//! `docs/12-API-GATEWAY.md`).
//!
//! ## What this owns
//!
//! `POST /bots` has to *start* something, and something that runs needs an
//! owner: a task, a lifetime, and a way to stop it. That is this module. The
//! `bots` row is the durable record; a running task is the in-process instance,
//! and they are deliberately separate -- a deployment restarts, and when it
//! does the rows survive and the tasks do not.
//!
//! ## One feed per symbol, not one per bot
//!
//! Ten bots on BTCUSDT need one websocket, not ten. The supervisor owns a
//! [`MarketBusRegistry`] and starts a collector for a symbol when the first bot
//! needs it, stopping it when the last one goes. That is also why the bus is
//! the interface between the feed and the bots: a bot subscribes to a symbol,
//! and neither knows how many of the other there are.
//!
//! That rule now covers live bots too, and it is a *correctness* property
//! rather than an economy: a paper bot and a live bot on the same symbol must
//! be shown the same candles, or the two runs disagree about what the market
//! did and every backtest-versus-live comparison is measuring the feed rather
//! than the strategy.
//!
//! ## The seam that makes this testable
//!
//! [`BotSupervisor::feed_candle`] publishes a closed candle into the bus. The
//! live collector calls it; a test calls it directly. Everything downstream --
//! which bots receive it, what they decide, what reaches the database -- is the
//! same code either way, so the test exercises the real path without a socket.
//!
//! ## Pausing drains rather than unsubscribes
//!
//! A paused bot keeps receiving candles and throws them away. Unsubscribing
//! would be tidier but wrong: a `tokio::sync::broadcast` receiver that stops
//! reading makes the *sender* lag, and one paused bot would start dropping
//! candles for every other bot on the symbol.
//!
//! ## A live decision blocks the loop, and that is deliberate
//!
//! A live decision places orders, which is network I/O. Doing it inline in the
//! select loop means the flush tick and the next candle wait behind it. The
//! alternative -- spawning the decision and continuing to read -- would let the
//! bot take a second entry while the first is still in flight, which is the
//! concurrency limit defeated by scheduling. Waiting is the conservative
//! failure.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use analytics_core::types::{Candle, OrderBookSnapshot};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use trading_engine::{BinanceRest, BotSession, LiveBot, LiveSession, PaperBot};

use crate::now_ns;

/// The audit event type a bot writes when it stopped because it could not
/// safely continue.
///
/// Distinct from `bot.stopped`, which records that a run ended. This one
/// records that it ended *unexpectedly*, which is the difference between a bot
/// an operator may restart and one they must reconcile first.
pub const FATAL_EVENT: &str = "bot.fatal";

/// How often a running bot flushes its decisions and trades.
///
/// Short enough that a crash loses seconds of audit trail, long enough that a
/// busy symbol is not one INSERT per candle.
///
/// Configurable rather than fixed because it is the difference between a test
/// that takes a third of a second and one that takes thirty -- and because a
/// deployment that wants a tighter audit trail should not need a rebuild.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// How many bot events to buffer for a slow watcher.
///
/// A watcher that falls this far behind is told it lagged rather than blocking
/// the bots, which is the same backpressure rule the market channels use.
const EVENT_BUFFER: usize = 256;

/// Whether the supervisor opens a market feed of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedMode {
    /// No feed. Bots receive whatever is published into the bus by something
    /// else -- which, in a deployment that runs nothing else, is nothing.
    Off,
    /// Open a Binance feed per symbol on demand.
    Binance,
}

impl FeedMode {
    /// Read the mode from `MARKET_FEED`.
    ///
    /// Defaults to `Off`, and says so when a bot is started, because a bot that
    /// is `running` and receiving nothing looks exactly like a bot that is
    /// running and finding no setups. Only one of those is a problem, and the
    /// operator should not have to guess which.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("MARKET_FEED").as_deref() {
            Ok("binance") => Self::Binance,
            Ok("off") | Err(_) => Self::Off,
            Ok(other) => {
                warn!("MARKET_FEED=`{other}` is not a known mode; treating it as `off`");
                Self::Off
            }
        }
    }
}

/// Something a running bot did, for a client watching it.
///
/// Broadcast rather than polled: `/ws/bots/{id}` has to show a decision as it
/// happens, and asking the database every second would turn one bot's activity
/// into a query per second per watcher.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BotEvent {
    /// The task attached and started consuming.
    Started {
        /// The bot.
        bot_id: Uuid,
        /// What it trades.
        symbol: String,
    },
    /// One decision candle, including the ones that did nothing.
    Decision {
        /// The bot.
        bot_id: Uuid,
        /// What it decided.
        record: trading_engine::DecisionRecord,
    },
    /// One live decision, including the ones that placed nothing.
    ///
    /// A separate variant rather than a reuse of `Decision`: a live decision
    /// carries an *outcome* (entered, denied by risk, refused by the venue,
    /// protection lost) where a paper one carries a signal, and a watcher that
    /// could not tell them apart could not tell "the strategy wanted in" from
    /// "we bought".
    LiveDecision {
        /// The bot.
        bot_id: Uuid,
        /// What it decided, and what the venue said.
        record: trading_engine::LiveRecord,
    },
    /// The task finished, cleanly or otherwise.
    Stopped {
        /// The bot.
        bot_id: Uuid,
        /// How many trades it completed.
        trades: usize,
        /// Why the risk engine stopped it, when it did.
        halt_reason: Option<String>,
    },
}

impl BotEvent {
    /// Which bot this event is about.
    ///
    /// A method rather than a match at each call site, because the call site
    /// that matters is the websocket's filter and a filter that forgets a
    /// variant *silently drops events*: adding `LiveDecision` broke the
    /// exhaustive match there, which is how it was noticed, but the next
    /// variant added with a `_ =>` fallback would not be.
    #[must_use]
    pub const fn bot_id(&self) -> Uuid {
        match self {
            Self::Started { bot_id, .. }
            | Self::Decision { bot_id, .. }
            | Self::LiveDecision { bot_id, .. }
            | Self::Stopped { bot_id, .. } => *bot_id,
        }
    }
}

/// Which kind of bot a task is running.
///
/// Both variants are boxed, and the reason is a correction rather than
/// belt-and-braces: the first version boxed only `Live`, on the assumption that
/// a live bot carrying an exchange adapter must be the heavy one. It is not.
/// `PaperBot` is ~1.4kB inline (it owns a `RollingLadder`, a `StrategyEngine`
/// and a `RiskEngine`) while `LiveBot<BinanceRest>` is mostly its own boxed
/// adapter, so the enum was 1.4kB wide and every `BotKind` — including a
/// `Live` one — was moved in 1.4kB chunks. Clippy's `large_enum_variant` caught
/// the measurement; the assumption was the bug.
enum BotKind {
    /// A simulator, no venue.
    Paper(Box<PaperBot>),
    /// A real venue, real orders.
    Live(Box<LiveBot<BinanceRest>>),
}

impl BotKind {
    /// The symbol traded.
    fn symbol(&self) -> &str {
        match self {
            Self::Paper(bot) => bot.symbol(),
            Self::Live(bot) => bot.symbol(),
        }
    }

    /// Whether the risk engine has stopped it.
    fn is_halted(&self) -> bool {
        match self {
            Self::Paper(bot) => bot.is_halted(),
            Self::Live(bot) => bot.is_halted(),
        }
    }

    /// Why, when it has.
    fn halt_reason(&self) -> Option<&str> {
        match self {
            Self::Paper(bot) => bot.halt_reason(),
            Self::Live(bot) => bot.halt_reason(),
        }
    }

    /// How many trades it completed.
    fn trade_count(&self) -> usize {
        match self {
            Self::Paper(bot) => bot.trades().len(),
            Self::Live(bot) => bot.trades().len(),
        }
    }

    /// Whether this bot trades a real venue.
    fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }

    /// The venue, for a live bot.
    fn venue(&self) -> Option<&str> {
        match self {
            Self::Paper(_) => None,
            Self::Live(bot) => Some(bot.venue()),
        }
    }

    /// What to record in `bot.started`.
    fn started_context(&self) -> serde_json::Value {
        match self {
            Self::Paper(_) => serde_json::json!({ "attached": true, "mode": "paper" }),
            Self::Live(bot) => serde_json::json!({
                "attached": true,
                "mode": "live",
                "venue": bot.venue(),
            }),
        }
    }

    /// One decision candle, as the event a watcher should see.
    ///
    /// # Errors
    /// Only a live bot can fail here, and only when it could not safely
    /// continue -- an order whose fate is unknown. That is fatal by design: see
    /// [`trading_engine::execution`].
    async fn on_candle(
        &mut self,
        bot_id: Uuid,
        candle: &Candle,
    ) -> Result<Option<BotEvent>, trading_engine::ExecutionError> {
        match self {
            Self::Paper(bot) => Ok(bot
                .on_candle(candle)
                .map(|record| BotEvent::Decision { bot_id, record })),
            Self::Live(bot) => bot
                .on_candle(candle)
                .await
                .map(|record| record.map(|record| BotEvent::LiveDecision { bot_id, record })),
        }
    }

    /// Trip the kill switch and act on it now.
    ///
    /// The switch is read on the next decision bar, which for a five-minute
    /// strategy is up to five minutes away. A button labelled "stop" that takes
    /// five minutes is not a stop, so a live bot liquidates immediately and a
    /// paper bot simply stops being asked.
    ///
    /// # Errors
    /// Returns the execution error when the emergency close itself failed. The
    /// switch is tripped either way, so the bot will not trade again.
    async fn kill(&mut self, reason: &str, now: i64) -> Result<(), trading_engine::ExecutionError> {
        match self {
            Self::Paper(bot) => {
                bot.risk_mut().kill(reason);
                Ok(())
            }
            Self::Live(bot) => {
                bot.kill(reason, now).await?;
                Ok(())
            }
        }
    }
}

/// What a running task writes through.
///
/// One variant per bot kind because the two sessions drain different shapes:
/// [`LiveSession`] also has `live_orders` to maintain, which paper mode has
/// never heard of.
enum Session {
    /// Paper.
    Paper(BotSession),
    /// Live.
    Live(Box<LiveSession>),
}

impl Session {
    /// Attach to an existing bot row.
    async fn attach(
        database: &db::Database,
        user_id: Uuid,
        bot_id: Uuid,
        kind: &BotKind,
    ) -> Result<Self, trading_engine::ExecutionError> {
        let context = kind.started_context();
        match kind {
            BotKind::Paper(_) => Ok(Self::Paper(
                BotSession::attach(database, user_id, bot_id, context).await?,
            )),
            BotKind::Live(_) => Ok(Self::Live(Box::new(
                LiveSession::attach(database, user_id, bot_id, context).await?,
            ))),
        }
    }

    /// Write everything produced since the last flush.
    async fn flush(&mut self, kind: &mut BotKind) -> Result<(), trading_engine::ExecutionError> {
        match (self, kind) {
            (Self::Paper(session), BotKind::Paper(bot)) => session.flush(bot).await,
            (Self::Live(session), BotKind::Live(bot)) => session.flush(bot).await,
            // Cannot happen: the session is built from the kind and both live
            // in the same task. Returning rather than panicking, because a
            // mismatched pair that panicked would take the process down.
            _ => Ok(()),
        }
    }

    /// Finalise: flush, set the terminal status, write `bot.stopped`.
    async fn finish(&mut self, kind: &mut BotKind) -> Result<(), trading_engine::ExecutionError> {
        match (self, kind) {
            (Self::Paper(session), BotKind::Paper(bot)) => session.finish(bot).await,
            (Self::Live(session), BotKind::Live(bot)) => session.finish(bot).await,
            _ => Ok(()),
        }
    }
}

/// A bot that is running in this process.
struct RunningBot {
    /// The task feeding it candles.
    handle: JoinHandle<()>,
    /// Set to stop it; the task checks this between candles.
    stop: Arc<AtomicBool>,
    /// Wakes the task when `stop` is set.
    ///
    /// The flag alone is not enough: it is only read after the task's `select!`
    /// returns, and the task's own wakeups are the flush tick and incoming
    /// candles -- thirty seconds and one closed candle, respectively. A stop
    /// should not have to wait for either.
    wake: Arc<Notify>,
    /// Set while paused. The task keeps reading and discards.
    paused: Arc<AtomicBool>,
    /// Set to liquidate now rather than on the next decision bar.
    ///
    /// Separate from `stop` because the two want different things done: a stop
    /// ends the run, a kill ends the run *and* closes the position. A live bot
    /// with an open position that is merely stopped leaves that position on the
    /// exchange with nothing watching its stop.
    kill: Arc<AtomicBool>,
    /// The venue this bot trades, for a live one.
    ///
    /// Kept here rather than asked of the task because the point of knowing it
    /// is to reach the bots that *are* running -- revoking a venue has to stop
    /// them, and a question that could not be answered without the answer would
    /// not be a question.
    venue: Option<String>,
}

/// How long a graceful stop may take before the task is aborted.
///
/// Deliberately not derived from `flush_interval`, which bounds how soon the
/// task *notices* the flag and nothing else. The work a stop has to wait for is
/// the flush already in flight plus [`BotSession::finish`] -- a flush, a status
/// update and the `bot.stopped` event, three statements against the managed
/// database, where one statement measures around a second.
///
/// Measured, not guessed: a stop that finalises cleanly takes ~1200ms against
/// the managed database. The old bound was `flush_interval + 1s`, which the
/// tests set to 1100ms -- a margin of 100ms, so a clean stop was aborted
/// whenever the machine was busy. An aborted task never writes `bot.stopped`,
/// and that event is the only thing that tells a clean stop from a crash. So
/// the flake was not a slow test: it was the one assertion that distinguishes
/// the two failure modes reporting a crash for a stop that worked.
///
/// Thirty seconds is a valve against a genuinely wedged task, not a budget for
/// a slow one.
const STOP_GRACE: Duration = Duration::from_secs(30);

/// Owns every running bot and the feed they share.
#[derive(Clone)]
pub struct BotSupervisor {
    inner: Arc<Inner>,
}

struct Inner {
    /// The pub/sub the bots subscribe to and the feed publishes into.
    bus: Arc<market_data::MarketBusRegistry>,
    running: Mutex<HashMap<Uuid, RunningBot>>,
    feed: FeedMode,
    /// Symbols a collector has been started for, so a second bot on the same
    /// symbol does not open a second websocket.
    feeds: Mutex<HashMap<String, JoinHandle<()>>>,
    /// Wall-clock time (unix nanos) of the most recent candle published for a
    /// symbol.
    ///
    /// This exists so the process that *evaluates* the stale-feed rule is the
    /// process that knows the age. `docs/18` puts a 5-minute limit on a silent
    /// feed and `docs/20` has a runbook for it, but the age of a feed is not
    /// observable from inside the bus: a channel with nothing to say and a
    /// channel whose source died look identical. Only a clock tells them apart,
    /// and the clock has to be read where the rule runs.
    last_candle_ns: Mutex<HashMap<String, i64>>,
    /// How often a running bot flushes.
    flush_interval: Duration,
    /// Everything the running bots have done, for `/ws/bots/{id}`.
    events: broadcast::Sender<BotEvent>,
}

impl std::fmt::Debug for BotSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotSupervisor")
            .field("running", &self.running_count())
            .field("feed", &self.inner.feed)
            .finish()
    }
}

impl BotSupervisor {
    /// Build a supervisor.
    #[must_use]
    pub fn new(feed: FeedMode) -> Self {
        Self::with_flush_interval(feed, DEFAULT_FLUSH_INTERVAL)
    }

    /// Build a supervisor that flushes on a chosen interval.
    ///
    /// Tests use a short one: at the default, a test would either wait thirty
    /// seconds or assert against decisions that have not been written yet.
    #[must_use]
    pub fn with_flush_interval(feed: FeedMode, flush_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus: Arc::new(market_data::MarketBusRegistry::new()),
                running: Mutex::new(HashMap::new()),
                feed,
                feeds: Mutex::new(HashMap::new()),
                last_candle_ns: Mutex::new(HashMap::new()),
                flush_interval,
                events: broadcast::channel(EVENT_BUFFER).0,
            }),
        }
    }

    /// How the feed is configured, for the startup log.
    #[must_use]
    pub fn feed_mode(&self) -> FeedMode {
        self.inner.feed
    }

    /// How many bots are running.
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.inner.running.lock().map_or(0, |running| running.len())
    }

    /// Whether a bot is running here.
    #[must_use]
    pub fn is_running(&self, bot_id: Uuid) -> bool {
        self.inner
            .running
            .lock()
            .is_ok_and(|running| running.contains_key(&bot_id))
    }

    /// Subscribe to closed candles for a symbol.
    ///
    /// The same bus the supervisor's feed publishes into, so a chart watching
    /// `/ws/market/{symbol}/{timeframe}` and a bot trading it see the same
    /// candles -- they cannot disagree about what the market did.
    #[must_use]
    pub fn subscribe_candles(
        &self,
        symbol: &str,
    ) -> broadcast::Receiver<analytics_core::types::Candle> {
        self.inner.bus.bus(symbol).subscribe_candles()
    }

    /// Ensure a feed exists for a symbol, for a caller that only wants to watch.
    ///
    /// A chart opening `/ws/market` should start the feed just as a bot does:
    /// otherwise the channel is silent until somebody happens to launch a bot,
    /// and "the chart shows nothing" has two possible causes again.
    pub fn ensure_feed_for(&self, symbol: &str) {
        self.ensure_feed(symbol);
    }

    /// Subscribe to what the running bots are doing.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<BotEvent> {
        self.inner.events.subscribe()
    }

    /// Publish a closed candle into the bus.
    ///
    /// The seam described in the module docs: the live collector calls this, and
    /// so does a test.
    pub fn feed_candle(&self, candle: &Candle) {
        // Stamped with the *arrival* time, not the candle's own open time.
        // "Stale" means "nothing has arrived recently", and a replayed or
        // backfilled candle arrives now even though it is about last Tuesday --
        // so arrival time is the one that does not report a replay as an outage.
        if let Ok(mut ages) = self.inner.last_candle_ns.lock() {
            ages.insert(candle.symbol.clone(), now_ns());
        }
        self.inner
            .bus
            .bus(&candle.symbol)
            .publish_candle(candle.clone());
    }

    /// How long ago each symbol's feed last produced a candle, in seconds.
    ///
    /// A symbol that has never produced one is absent rather than infinite:
    /// "no feed has started yet" and "the feed died" are different incidents
    /// and only the second one should page anybody.
    #[must_use]
    pub fn feed_ages(&self, now: i64) -> Vec<(String, f64)> {
        let Ok(ages) = self.inner.last_candle_ns.lock() else {
            return Vec::new();
        };
        let mut out: Vec<(String, f64)> = ages
            .iter()
            .map(|(symbol, at)| {
                let seconds = now.saturating_sub(*at) as f64 / 1_000_000_000.0;
                (symbol.clone(), seconds.max(0.0))
            })
            .collect();
        // Sorted so a scrape is stable and a test can assert on order.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Subscribe to live order books for `symbol`.
    ///
    /// The book is maintained in `market-data`: a REST snapshot, then diff
    /// events bridged onto it, published only once the sequence is contiguous.
    /// This is just the read end.
    #[must_use]
    pub fn subscribe_orderbook(&self, symbol: &str) -> broadcast::Receiver<OrderBookSnapshot> {
        self.inner.bus.bus(symbol).subscribe_orderbook()
    }

    /// Publish an order-book snapshot into the bus.
    ///
    /// The same seam as [`feed_candle`], for the same reason: the collector
    /// publishes through the bus, and a test publishes through this.
    pub fn feed_orderbook(&self, snapshot: &OrderBookSnapshot) {
        self.inner
            .bus
            .bus(&snapshot.symbol)
            .publish_orderbook(snapshot.clone());
    }

    /// Start running a paper bot.
    ///
    /// Returns immediately: the bot runs in its own task. Starting a bot that is
    /// already running is a no-op rather than a second task, because two tasks
    /// on one bot row would interleave their decisions into the same audit
    /// trail and neither would be the truth.
    pub fn start(&self, bot_id: Uuid, user_id: Uuid, database: db::Database, bot: PaperBot) {
        self.spawn(bot_id, user_id, database, BotKind::Paper(Box::new(bot)));
    }

    /// Start running a live bot against a real venue.
    ///
    /// The caller is responsible for having passed
    /// [`trading_engine::LiveGate`] -- this does not re-check it, because the
    /// check is a request-time decision about a *strategy* and by the time a
    /// task exists that decision has been made.
    pub fn start_live(
        &self,
        bot_id: Uuid,
        user_id: Uuid,
        database: db::Database,
        bot: LiveBot<BinanceRest>,
    ) {
        self.spawn(bot_id, user_id, database, BotKind::Live(Box::new(bot)));
    }

    /// The one task loop, shared by both bot kinds.
    fn spawn(&self, bot_id: Uuid, user_id: Uuid, database: db::Database, mut kind: BotKind) {
        if self.is_running(bot_id) {
            warn!(%bot_id, "bot is already running here; not starting a second task");
            return;
        }

        let symbol = kind.symbol().to_string();
        let venue = kind.venue().map(str::to_string);
        self.ensure_feed(&symbol);

        let flush_interval = self.inner.flush_interval;
        let events = self.inner.events.clone();
        let mut candles = self.inner.bus.bus(&symbol).subscribe_candles();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let kill = Arc::new(AtomicBool::new(false));
        let (task_stop, task_paused, task_wake, task_kill) = (
            Arc::clone(&stop),
            Arc::clone(&paused),
            Arc::clone(&wake),
            Arc::clone(&kill),
        );

        let handle = tokio::spawn(async move {
            let mut session = match Session::attach(&database, user_id, bot_id, &kind).await {
                Ok(session) => session,
                Err(e) => {
                    warn!(%bot_id, "could not attach the bot session: {e}");
                    return;
                }
            };

            let mut flush = tokio::time::interval(flush_interval);
            // The reason this run ended, when it ended for a reason other than
            // being asked to. Set by a fatal live error; reported in
            // `bot.stopped` so a trail says *why* rather than only that it did.
            let mut fatal: Option<String> = None;

            info!(%bot_id, %symbol, live = kind.is_live(), "bot started");
            events
                .send(BotEvent::Started {
                    bot_id,
                    symbol: symbol.clone(),
                })
                .ok();

            loop {
                tokio::select! {
                    received = candles.recv() => match received {
                        Ok(candle) => {
                            // Skipped rather than `continue`d: the stop check
                            // below is the only thing that ends this loop, and a
                            // paused bot on a busy symbol takes this branch every
                            // time -- so `continue` here would let it ignore a
                            // stop for as long as candles keep arriving.
                            if !task_paused.load(Ordering::Relaxed) {
                                match kind.on_candle(bot_id, &candle).await {
                                    Ok(Some(event)) => { events.send(event).ok(); }
                                    Ok(None) => {}
                                    Err(error) => {
                                        // A live bot only fails here when it
                                        // could not safely continue: an order
                                        // whose fate is unknown. Continuing
                                        // would be trading blind, so this ends
                                        // the run.
                                        warn!(%bot_id, "the bot could not continue: {error}");
                                        fatal = Some(error.to_string());
                                    }
                                }
                            }
                        }
                        Err(RecvError::Lagged(skipped)) => {
                            // Losing candles means the bot's view of the market
                            // has a hole in it, so this is loud.
                            warn!(%bot_id, "bot feed lagged by {skipped} candles");
                        }
                        Err(RecvError::Closed) => break,
                    },
                    _ = flush.tick() => {
                        if let Err(e) = session.flush(&mut kind).await {
                            warn!(%bot_id, "could not flush the audit trail: {e}");
                        }
                    },
                    // Falls through to the checks below rather than breaking
                    // here, so the atomic flags stay the single source of truth.
                    // The notification only decides *when* they are read; a
                    // notification that arrives while the task is inside a flush
                    // is still honoured, because the permit is held until the
                    // next `notified()`.
                    _ = task_wake.notified() => {}
                }

                // Checked before the stop, and in this order: an operator who
                // pressed "kill" wants the position closed, and a stop that
                // skipped the liquidation would leave it open with nothing
                // watching it.
                if task_kill.swap(false, Ordering::Relaxed) {
                    match kind.kill("the kill-switch was engaged", now_ns()).await {
                        Ok(()) => info!(%bot_id, "the kill-switch closed the position"),
                        Err(error) => {
                            // Deliberately fatal: the switch is engaged either
                            // way, so the bot will not trade again, but a
                            // position that could not be closed is an operator's
                            // problem and the trail must say so.
                            warn!(%bot_id, "the kill-switch could not close the position: {error}");
                            fatal = Some(format!(
                                "the kill-switch could not close the position: {error}"
                            ));
                        }
                    }
                }

                if task_stop.load(Ordering::Relaxed) || kind.is_halted() {
                    if kind.is_halted() {
                        info!(%bot_id, reason = ?kind.halt_reason(), "bot stopped by the risk engine");
                    }
                    break;
                }
            }

            // A fatal error is a stop the operator did not ask for, so it is
            // written down as one rather than left to be inferred from a gap in
            // the trail.
            if let Some(reason) = &fatal {
                if let Err(e) = write_fatal(&database, user_id, bot_id, reason).await {
                    warn!(%bot_id, "could not record why the bot stopped: {e}");
                }
            }

            if let Err(e) = session.finish(&mut kind).await {
                warn!(%bot_id, "could not finalise the bot: {e}");
            }
            events
                .send(BotEvent::Stopped {
                    bot_id,
                    trades: kind.trade_count(),
                    halt_reason: kind.halt_reason().map(str::to_string),
                })
                .ok();
            info!(%bot_id, trades = kind.trade_count(), "bot finished");
        });

        if let Ok(mut running) = self.inner.running.lock() {
            running.insert(
                bot_id,
                RunningBot {
                    handle,
                    stop,
                    wake,
                    paused,
                    kill,
                    venue,
                },
            );
        }
    }

    /// Pause a running bot. `false` if it is not running here.
    pub fn pause(&self, bot_id: Uuid) -> bool {
        let Ok(running) = self.inner.running.lock() else {
            return false;
        };
        let Some(bot) = running.get(&bot_id) else {
            return false;
        };
        bot.paused.store(true, Ordering::Relaxed);
        true
    }

    /// Resume a paused bot. `false` if it is not running here.
    pub fn resume(&self, bot_id: Uuid) -> bool {
        let Ok(running) = self.inner.running.lock() else {
            return false;
        };
        let Some(bot) = running.get(&bot_id) else {
            return false;
        };
        bot.paused.store(false, Ordering::Relaxed);
        true
    }

    /// Trip a running bot's kill switch.
    ///
    /// Returns `false` when it is not running here, for the same reason
    /// [`pause`](Self::pause) does: a switch that is thrown on a bot whose task
    /// lives elsewhere (or nowhere) records an intention nothing will act on,
    /// and the caller needs to know that so it can say so rather than report a
    /// liquidation that never happened.
    ///
    /// Sets the stop flag as well as the kill flag, because a kill is terminal:
    /// `docs/12` lists `killed` alongside `stopped`, and a bot that liquidated
    /// and then carried on looking for entries would be a bot the operator
    /// believes is off.
    ///
    /// The work happens in the task, on its next wakeup, which this triggers.
    /// The call therefore returns before the position is closed -- a route
    /// cannot block on a network round trip to a venue. A caller that needs to
    /// know the liquidation finished should follow this with
    /// [`stop`](Self::stop), which waits for the task.
    pub fn kill(&self, bot_id: Uuid) -> bool {
        let Ok(running) = self.inner.running.lock() else {
            return false;
        };
        let Some(bot) = running.get(&bot_id) else {
            return false;
        };
        bot.kill.store(true, Ordering::Relaxed);
        bot.stop.store(true, Ordering::Relaxed);
        // Woken so the liquidation happens now rather than at the next flush
        // tick -- which is thirty seconds in production.
        bot.wake.notify_one();
        true
    }

    /// Throw the kill switch on every running live bot trading `venue`.
    ///
    /// This is what makes a revoke real. `docs/15` asks for a revoke that takes
    /// effect without a redeploy, and a revoke that only changes what *future*
    /// bots may do would leave every bot already running on that venue trading
    /// an account the operator has just withdrawn consent for.
    ///
    /// Returns the ids it threw the switch on, so the response can say what it
    /// did rather than only what it recorded. The liquidations happen in the
    /// tasks; this does not wait for them.
    pub fn kill_venue(&self, venue: &str) -> Vec<Uuid> {
        let venue = venue.to_ascii_lowercase();
        let Ok(running) = self.inner.running.lock() else {
            return Vec::new();
        };
        let mut killed = Vec::new();
        for (bot_id, bot) in running.iter() {
            if bot.venue.as_deref() == Some(venue.as_str()) {
                bot.kill.store(true, Ordering::Relaxed);
                bot.stop.store(true, Ordering::Relaxed);
                bot.wake.notify_one();
                killed.push(*bot_id);
            }
        }
        killed.sort();
        killed
    }

    /// Stop a bot and wait for its task to finish.
    ///
    /// Waits rather than aborting: the task's last act is to flush and write
    /// `bot.stopped`, and killing it mid-flight is how a bot ends up looking
    /// like it crashed.
    ///
    /// Returning `true` means the task is **gone**, not merely signalled. The
    /// wait is bounded, and if the bound is reached the task is aborted -- it
    /// was removed from `running` on the way in, so a task that outlived this
    /// call could never be waited for again, and would go on writing audit rows
    /// for a bot the caller believed it had finished with. That is exactly how
    /// a `DELETE /bots/{id}` ends up leaving an `audit_log` row behind and
    /// failing the account's own cleanup.
    pub async fn stop(&self, bot_id: Uuid) -> bool {
        let Some(bot) = self
            .inner
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.remove(&bot_id))
        else {
            return false;
        };
        bot.stop.store(true, Ordering::Relaxed);
        // Woken rather than left to be noticed. Without this the task finds out
        // on its next flush tick -- which is thirty seconds in production, and
        // that is how long `DELETE /bots/{id}` would then take.
        bot.wake.notify_one();

        let started = tokio::time::Instant::now();
        let mut handle = bot.handle;
        if tokio::time::timeout(STOP_GRACE, &mut handle).await.is_err() {
            // Not a slow stop -- [`STOP_GRACE`] is sized for one. A task still
            // alive after it is wedged, and aborting is the lesser evil: the
            // alternative is a task nobody can reach, still writing audit rows
            // for a bot the caller believes it has finished with.
            warn!(
                %bot_id,
                elapsed_ms = started.elapsed().as_millis(),
                "bot task did not stop within its grace; aborting it"
            );
            handle.abort();
        } else {
            // Worth a line: the difference between 40ms and 4s here is the
            // difference between a local database and the managed one, and that
            // is not otherwise visible from a request.
            debug!(%bot_id, elapsed_ms = started.elapsed().as_millis(), "bot stopped");
        }
        true
    }

    /// Stop every bot, for a graceful shutdown.
    ///
    /// Concurrently, because the stops are independent and the shutdown budget
    /// is shared. Run one after another, N bots that each take a few seconds to
    /// finalise would add up past the platform's kill timeout -- and being
    /// killed mid-flush is the failure a graceful shutdown exists to avoid.
    pub async fn stop_all(&self) {
        let ids: Vec<Uuid> = self
            .inner
            .running
            .lock()
            .map_or_else(|_| Vec::new(), |running| running.keys().copied().collect());
        futures::future::join_all(ids.into_iter().map(|id| self.stop(id))).await;
    }

    /// Start a collector for a symbol if the feed is enabled and it has none.
    fn ensure_feed(&self, symbol: &str) {
        if self.inner.feed != FeedMode::Binance {
            warn!(
                symbol,
                "no market feed is configured (MARKET_FEED is not `binance`): this bot is running \
                 and will receive no candles until something publishes into the bus"
            );
            return;
        }

        let Ok(mut feeds) = self.inner.feeds.lock() else {
            return;
        };
        if feeds.contains_key(symbol) {
            return;
        }

        let bus = Arc::clone(&self.inner.bus);
        let symbol = symbol.to_string();
        let for_task = symbol.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = run_binance_feed(bus, &for_task).await {
                warn!(symbol = %for_task, "the market feed stopped: {e}");
            }
        });
        feeds.insert(symbol, handle);
    }
}

/// Write down why a bot stopped when nobody asked it to.
///
/// `bot.stopped` already exists and says a run ended; this says *why*, and the
/// distinction matters most in exactly the case that produces it -- an order
/// whose fate is unknown, where the operator's next move is to reconcile
/// against the venue rather than to restart the bot.
async fn write_fatal(
    database: &db::Database,
    user_id: Uuid,
    bot_id: Uuid,
    reason: &str,
) -> Result<(), db::DbError> {
    db::paper::insert_audit_events(
        database.pool(),
        &[db::paper::AuditEvent {
            user_id: Some(user_id),
            event_type: FATAL_EVENT.into(),
            payload: serde_json::json!({
                "bot_id": bot_id,
                "reason": reason,
                "action": "reconcile against the venue before restarting",
            }),
            ts: now_ns(),
        }],
    )
    .await
}

/// Connect to Binance and publish closed candles for one symbol.
async fn run_binance_feed(
    bus: Arc<market_data::MarketBusRegistry>,
    symbol: &str,
) -> Result<(), String> {
    use market_data::{BinanceCollector, ExchangeCollector};

    let mut collector = BinanceCollector::with_defaults(Arc::clone(&bus));
    collector.connect().await.map_err(|e| e.to_string())?;

    let mut trades = collector
        .trade_stream(symbol)
        .ok_or_else(|| format!("no trade stream for {symbol}"))?;
    collector
        .subscribe_trades(symbol)
        .await
        .map_err(|e| e.to_string())?;

    // Depth rides the *same* connection: the collector uses Binance's combined
    // `/stream` endpoint, which is the whole reason one socket can carry both.
    // Subscribing it here means the DOM has a book whenever anything at all is
    // watching this symbol, rather than only once a bot happens to run.
    //
    // A failure here is not fatal. Candles are what bots trade on; a DOM
    // without depth is a degraded panel, not a dead chart. The depth channel
    // tells the client when there is no book rather than showing an empty one.
    if let Err(e) = collector.subscribe_order_book(symbol).await {
        warn!(symbol, "the depth feed did not start: {e}");
    }

    // The timeframes the feed builds are the ones the bots declared, but the
    // feed starts before it knows them; `standard` builds the common set and a
    // bot whose document declares something outside it simply never warms up.
    let mut builder = market_data::MultiTimeframeCandleBuilder::standard(symbol.to_string());
    info!(symbol, "market feed connected");

    loop {
        match trades.recv().await {
            Ok(trade) => {
                for candle in builder.on_trade(&trade) {
                    bus.bus(&candle.symbol).publish_candle(candle);
                }
            }
            Err(RecvError::Lagged(skipped)) => {
                warn!(symbol, "trade feed lagged by {skipped} messages");
            }
            Err(RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feed_defaults_to_off() {
        // A bot that is `running` with no feed looks exactly like a bot that is
        // running and finding no setups, so the default has to be the one that
        // does not open sockets and says so.
        assert_eq!(FeedMode::from_env(), FeedMode::Off);
    }

    #[tokio::test]
    async fn stopping_a_bot_that_is_not_running_is_false_not_a_panic() {
        let supervisor = BotSupervisor::new(FeedMode::Off);
        assert!(!supervisor.is_running(Uuid::new_v4()));
        assert!(!supervisor.pause(Uuid::new_v4()));
        assert!(!supervisor.resume(Uuid::new_v4()));
        assert!(!supervisor.stop(Uuid::new_v4()).await);
        // The kill switch answers the same way, and it has to: a switch thrown
        // on a bot with no task records an intention nothing will act on, and
        // the caller must be able to say so rather than report a liquidation
        // that never happened.
        assert!(!supervisor.kill(Uuid::new_v4()));
    }

    #[test]
    fn killing_a_venue_with_nothing_running_reports_nothing_killed() {
        // An empty answer is the *normal* one -- it means nothing was trading
        // there -- and it must not be confused with a failure.
        let supervisor = BotSupervisor::new(FeedMode::Off);
        assert!(supervisor.kill_venue("binance").is_empty());
    }

    /// A candle with a distinct open time per `bar`.
    fn test_candle(symbol: &str, bar: usize) -> Candle {
        Candle {
            symbol: symbol.to_string(),
            timeframe: analytics_core::types::Timeframe::M5,
            open_time: bar as i64 * 5 * 60 * 1_000_000_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 1.0,
            buy_volume: 0.6,
            sell_volume: 0.4,
        }
    }

    #[test]
    fn the_age_of_a_feed_is_measured_from_when_the_candle_arrived() {
        // This is the writer the `StaleFeed` alert reads. Before it existed the
        // rule read a gauge nothing set, so it could never fire and the runbook
        // in `docs/20` described an alert that did not exist in practice.
        let supervisor = BotSupervisor::new(FeedMode::Off);

        // A symbol whose feed has never produced anything is *absent*, not
        // infinitely stale: no feed has been started, which is a different
        // incident from a feed that died.
        assert!(
            supervisor.feed_ages(now_ns()).is_empty(),
            "a symbol with no feed must not be reported as stale"
        );

        supervisor.feed_candle(&test_candle("BTCUSDT", 0));

        // The clock is read *after* the feed, not before.
        //
        // The first version of this captured `now` first and asserted the age at
        // `now + 600s` was `>= 600.0`. It is not: `feed_candle` stamps arrival a
        // few hundred nanoseconds later than the captured `now`, so the age came
        // out at 599.9999982 and the test failed roughly whenever it ran. Reading
        // the clock after the stamp makes the bound true by construction instead
        // of by luck -- and a timing assertion that depends on scheduling is the
        // coin-flip `docs/16` says not to write.
        let fed_at = now_ns();
        let ages = supervisor.feed_ages(fed_at);
        assert_eq!(ages.len(), 1);
        assert_eq!(ages[0].0, "BTCUSDT");
        assert!(
            ages[0].1 < 1.0,
            "a just-arrived candle is not old: {ages:?}"
        );

        // Ten minutes with nothing new. The age is measured from arrival, not
        // from the candle's own open time, so a backfilled candle about last
        // Tuesday does not report as a ten-day outage.
        let later = fed_at + 600 * 1_000_000_000;
        let ages = supervisor.feed_ages(later);
        assert!(
            ages[0].1 >= 600.0 && ages[0].1 < 601.0,
            "ten minutes later the age must be ten minutes: {ages:?}"
        );

        // A second candle resets it, which is what makes the alert a level
        // rather than a latch.
        supervisor.feed_candle(&test_candle("BTCUSDT", 1));
        assert!(supervisor.feed_ages(now_ns())[0].1 < 1.0);
    }

    #[test]
    fn every_symbol_is_aged_separately() {
        // The gauge is labelled per symbol rather than global, because one quiet
        // market must not hide behind another that is healthy -- a single
        // `market_data_feed_age_seconds` would report whichever symbol was
        // written last and the rule would see one number where there are two.
        //
        // What can be asserted without an injectable clock is that each symbol
        // gets its own entry, in a stable order, so the rule sees two samples
        // and reports the worst.
        let supervisor = BotSupervisor::new(FeedMode::Off);
        supervisor.feed_candle(&test_candle("ETHUSDT", 0));
        supervisor.feed_candle(&test_candle("BTCUSDT", 0));

        let ages = supervisor.feed_ages(now_ns());
        let symbols: Vec<&str> = ages.iter().map(|(symbol, _)| symbol.as_str()).collect();
        assert_eq!(
            symbols,
            vec!["BTCUSDT", "ETHUSDT"],
            "one entry per symbol, sorted so a scrape is stable"
        );
    }

    #[test]
    fn an_event_always_names_its_bot() {
        // The websocket filters on this. A variant that forgot to report its
        // bot would not be a compile error at the call site if the filter used
        // a `_` arm, and would silently drop every event of that kind.
        let bot_id = Uuid::new_v4();
        let events = [
            BotEvent::Started {
                bot_id,
                symbol: "BTCUSDT".into(),
            },
            BotEvent::Stopped {
                bot_id,
                trades: 3,
                halt_reason: None,
            },
        ];
        for event in &events {
            assert_eq!(event.bot_id(), bot_id);
        }
    }

    #[test]
    fn a_fresh_supervisor_runs_nothing() {
        let supervisor = BotSupervisor::new(FeedMode::Off);
        assert_eq!(supervisor.running_count(), 0);
        assert_eq!(supervisor.feed_mode(), FeedMode::Off);
    }
}
