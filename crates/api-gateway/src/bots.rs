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
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{debug, info, warn};
use uuid::Uuid;

use trading_engine::{BinanceRest, BotSession, DecisionPath, LiveBot, LiveSession, PaperBot};

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

/// How many market feeds may be open at once.
///
/// ## Why there is a ceiling at all
///
/// Every feed is a websocket plus a candle builder per resolution plus a slot
/// in the RAM history and a tape. A platform whose whole point is "any symbol,
/// not just BTCUSDT" will be asked for a lot of symbols, and `ensure_feed_for`
/// is called by *routes* -- opening a chart, asking a question -- so the number
/// of sockets was previously bounded only by how many distinct symbol strings
/// a client cared to send. One tab with a symbol list is enough to open
/// hundreds, and Binance's own limit is 300 connections per 5 minutes per IP,
/// after which it stops answering *everything*, including the symbol the
/// operator actually trades.
///
/// ## Why 32
///
/// Chosen against the memory budget rather than the venue's connection limit.
/// The interesting cost is not the socket, it is `HistoryRegistry`: a series
/// holds up to 1500 bars per resolution, and a feed fills seven of them. At
/// roughly a hundred bytes a bar that is ~1 MB a symbol for candles, plus a
/// tape that is capped separately. Thirty-two symbols lands around 40-50 MB of
/// buffer, which is affordable on a small instance and comfortably above what
/// one person watches at once.
///
/// ## What happens at the ceiling
///
/// The oldest *unused* feed is closed to make room, never a symbol somebody is
/// looking at. See [`BotSupervisor::evict_for`]. Refusing outright was the
/// first design and it is wrong here: the request that arrives when the table
/// is full is usually the one the user just clicked, and answering it with
/// "too many" would make a full table feel like a broken platform. Eviction is
/// cheap because the buffer going cold costs one venue round trip, not data.
pub const MAX_ACTIVE_FEEDS: usize = 32;

/// How long a feed lives after its last use.
///
/// Shorter than the 5-minute staleness rule in `docs/18`, deliberately: that
/// rule is about a feed that was *supposed* to be producing and stopped, while
/// this is about a feed nobody is looking at any more. A feed that goes idle
/// and is reclaimed is a normal event; a feed that goes stale is an incident,
/// and they must not be reported by the same signal.
///
/// Long enough to survive a user stepping away from the tab and coming back --
/// a browser that loses focus still polls, but a chart left open overnight
/// should not hold a socket for eight hours.
pub const DEFAULT_FEED_IDLE: Duration = Duration::from_secs(180);

/// What asked for a feed, which decides how long it may stay idle.
///
/// The distinction is not cosmetic. A feed opened because a **bot** is trading
/// that symbol must never be reclaimed for idleness: the bot is deciding on
/// those candles, and a feed that vanished mid-run would make the bot stop
/// seeing bars with nothing reported anywhere except a gap in the audit trail.
/// A feed opened because a **chart** asked is reclaimable, because the chart
/// will ask again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedReason {
    /// A running bot trades this symbol. Not reclaimable while it runs.
    Bot,
    /// A route asked -- a chart, a question, an order book.
    Route,
}

/// A feed the supervisor owns.
struct Feed {
    /// How to stop the collector.
    ///
    /// An [`AbortHandle`] rather than the [`JoinHandle`] itself, and not merely
    /// for tidiness: the supervisor only ever *aborts* a feed, and `JoinHandle`
    /// owns the task's output and its `Result`. Keeping the full handle in the
    /// table means the table is a place a panic could be collected from, which
    /// is exactly the kind of thing that quietly is not done. An `AbortHandle`
    /// is `Send + Sync`, cloneable, and constructible without spawning -- so
    /// the eviction rule can be tested without a runtime and without a socket.
    handle: AbortHandle,
    /// What asked for it, which bounds whether it may be reclaimed.
    reason: FeedReason,
    /// When something last used it, unix nanos.
    ///
    /// Shared with the feed's own task so a candle arriving counts as a use.
    /// Without that, a symbol nobody is *asking* about but that is actively
    /// publishing would look idle and be closed -- and reopening it would cost
    /// a REST round trip to rebuild a buffer that was one bar from complete.
    last_used_ns: Arc<std::sync::atomic::AtomicI64>,
}

/// Whether the supervisor opens a market feed of its own, and from where.
///
/// ## It names the venue, not just "yes"
///
/// This was `{ Off, Binance }`, and the venue name reached the live path in
/// exactly one place -- the `!= FeedMode::Binance` guard below. That was fine
/// while one venue existed and a trap the moment a second did: the guard and the
/// constructor would have had to agree, in two places, on a decision that is one
/// decision. It now carries the name and `FeedMode::is_on` answers "any venue",
/// so the guard cannot drift from the constructor.
///
/// Still `Copy`: it is read inside `BotSupervisor`, which is cloned per bot, and
/// turning it into a `String` would make every one of those clones allocate.
/// A `&'static str` keeps it `Copy` and keeps the venue list closed at the two
/// places a venue can actually be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedMode {
    /// No feed. Bots receive whatever is published into the bus by something
    /// else -- which, in a deployment that runs nothing else, is nothing.
    Off,
    /// Open a Binance feed per symbol on demand.
    Binance,
    /// Open a Bybit feed per symbol on demand.
    Bybit,
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
            Ok("bybit") => Self::Bybit,
            Ok("off") | Err(_) => Self::Off,
            Ok(other) => {
                warn!("MARKET_FEED=`{other}` is not a known mode; treating it as `off`");
                Self::Off
            }
        }
    }

    /// Whether any venue is selected.
    ///
    /// The one predicate the supervisor should branch on: a caller that has to
    /// name the venue to decide *whether* to open a feed would have to be edited
    /// for every venue added, which is the coupling this type exists to remove.
    #[must_use]
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
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

    /// Which path this bot decides on.
    fn decision_path(&self) -> DecisionPath {
        match self {
            Self::Paper(bot) => bot.decision_path(),
            Self::Live(bot) => bot.decision_path(),
        }
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
    /// Who owns the bot.
    ///
    /// Kept beside `broker_account_id` so that stopping the bots on an account
    /// filters on *both*. An account id reaches the supervisor from a URL, and
    /// two ids that must match is the difference between a guess stopping this
    /// user's bot and stopping everybody's.
    user_id: Uuid,
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
    /// The broker account this bot trades, for a live one.
    ///
    /// Same reasoning as `venue`, one level finer: revoking a venue stops
    /// everything on it, while disconnecting one account must stop only the
    /// bots spending *that* account's money. A user with two Binance accounts
    /// who disconnects one has not asked for the other to stop.
    broker_account_id: Option<Uuid>,
    /// Which path this bot decides on, recorded so a test can assert principle
    /// #6 without reaching into the task.
    decision_path: DecisionPath,
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

/// How often the feed recorder snapshots forming bars onto the chart lane.
///
/// One second, which is the cadence a chart can use: faster only multiplies
/// WebSocket frames for a repaint nobody perceives, slower and a `1d` bar
/// looks frozen between prints. Closed candles are never delayed by this --
/// they publish the moment the collector closes them -- and the tick only
/// throttles the *forming* snapshots.
const FORMING_PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// Owns every running bot and the feed they share.
#[derive(Clone)]
pub struct BotSupervisor {
    inner: Arc<Inner>,
}

struct Inner {
    /// The pub/sub the bots subscribe to and the feed publishes into.
    bus: Arc<market_data::MarketBusRegistry>,
    running: Mutex<HashMap<Uuid, RunningBot>>,
    /// The alert engine, shared with the task that evaluates it.
    ///
    /// The feed lifecycle writes here (`exclude_symbol` when a feed closes on
    /// purpose, `watch_symbol` when one opens) so the stale rules can tell a
    /// dead feed from an unwatched symbol. A `Mutex` because the writes come
    /// from request tasks and the reads from the alert task; nothing holds it
    /// across an await.
    alerter: Mutex<observability::Alerter>,
    feed: FeedMode,
    /// Symbols a collector has been started for, so a second bot on the same
    /// symbol does not open a second websocket.
    ///
    /// Bounded by [`MAX_ACTIVE_FEEDS`], with the least-recently-used reclaimable
    /// entry evicted at the ceiling. See [`BotSupervisor::ensure_feed`].
    feeds: Mutex<HashMap<String, Feed>>,
    /// The chart's recent history, held in RAM.
    ///
    /// Held here rather than in the collector because a series has to survive
    /// the thing that produced it: the collector is per-symbol and comes and
    /// goes with the feed, while the buffer is what `GET /candles` answers
    /// from, and it must answer even between feeds.
    ///
    /// Nothing in here is ever written to the database. That is the point --
    /// see [`market_data::history`] for the arithmetic that decided it.
    history: Arc<market_data::HistoryRegistry>,
    /// Recent trades and the newest book, in RAM.
    ///
    /// The tape is the only place trades live -- they are the expensive half of
    /// market data and the database cannot hold them. See
    /// [`market_data::tape`] for what that costs and what it limits.
    live: Arc<market_data::LiveRegistry>,
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
    /// How long a route-opened feed may sit unused before it is reclaimed.
    feed_idle: Duration,
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
        Self::build(feed, DEFAULT_FLUSH_INTERVAL)
    }

    /// Build a supervisor that flushes on a chosen interval.
    ///
    /// Tests use a short one: at the default, a test would either wait thirty
    /// seconds or assert against decisions that have not been written yet.
    #[must_use]
    pub fn with_flush_interval(feed: FeedMode, flush_interval: Duration) -> Self {
        Self::build(feed, flush_interval)
    }

    fn build(feed: FeedMode, flush_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus: Arc::new(market_data::MarketBusRegistry::new()),
                running: Mutex::new(HashMap::new()),
                feed,
                feeds: Mutex::new(HashMap::new()),
                history: Arc::new(market_data::HistoryRegistry::new()),
                live: Arc::new(market_data::LiveRegistry::new()),
                last_candle_ns: Mutex::new(HashMap::new()),
                alerter: Mutex::new(observability::Alerter::new(observability::default_rules())),
                flush_interval,
                feed_idle: DEFAULT_FEED_IDLE,
                events: broadcast::channel(EVENT_BUFFER).0,
            }),
        }
    }

    /// The shared alert engine, for the task that evaluates the rules.
    ///
    /// Handed out rather than constructed inside the alert task so the
    /// evaluate loop and the lifecycle writers see the same instance -- that
    /// identity is the whole mechanism: an exclusion the reaper writes is what
    /// the next rule evaluation reads.
    #[must_use]
    pub fn alerter(&self) -> &Mutex<observability::Alerter> {
        &self.inner.alerter
    }

    /// The chart's recent history -- what `GET /candles` answers from.
    ///
    /// Exposed rather than reached into so the route can say how much of a
    /// window RAM can cover and fetch only the rest from the venue.
    #[must_use]
    pub fn history(&self) -> Arc<market_data::HistoryRegistry> {
        Arc::clone(&self.inner.history)
    }

    /// The live tape and book -- what `/footprint` and `/orderbook` answer from.
    #[must_use]
    pub fn live(&self) -> Arc<market_data::LiveRegistry> {
        Arc::clone(&self.inner.live)
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

    /// Which path the bot decides on, when it is running here.
    #[must_use]
    pub fn bot_decision_path(&self, bot_id: Uuid) -> Option<DecisionPath> {
        self.inner
            .running
            .lock()
            .ok()
            .and_then(|running| running.get(&bot_id).map(|b| b.decision_path))
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

    /// Subscribe to the **chart lane**: closed candles and forming ones,
    /// interleaved in publication order.
    ///
    /// This is what a chart wants and a bot must never get: the forming frame
    /// moves the newest bar once a second, so a `1d` chart ticks live instead
    /// of looking frozen until midnight, while strategies keep consuming the
    /// closed-candle lane and can never fire on a bar that does not exist yet.
    /// See [`market_data::MarketEventBus::publish_chart_candle`] for the
    /// ordering contract the single publisher gives this lane.
    pub fn subscribe_chart_candles(
        &self,
        symbol: &str,
    ) -> broadcast::Receiver<analytics_core::types::Candle> {
        self.inner.bus.bus(symbol).subscribe_chart_candles()
    }

    /// Ensure a feed exists for a symbol, for a caller that only wants to watch.
    ///
    /// A chart opening `/ws/market` should start the feed just as a bot does:
    /// otherwise the channel is silent until somebody happens to launch a bot,
    /// and "the chart shows nothing" has two possible causes again.
    ///
    /// The feed is opened as [`FeedReason::Route`], so it can be reclaimed when
    /// nobody is looking at it. A bot's feed is opened as [`FeedReason::Bot`]
    /// and is not.
    pub fn ensure_feed_for(&self, symbol: &str) {
        self.ensure_feed(symbol, FeedReason::Route);
    }

    /// Note that a symbol's feed was just used, so it is not reclaimed.
    ///
    /// Called by the routes that answer from the buffer -- `/candles`,
    /// `/orderbook`, `/footprint`, an agent question -- because "used" is the
    /// input to the idle rule and a symbol being read is a symbol that must
    /// not be closed underneath the reader.
    pub fn touch_feed(&self, symbol: &str) {
        if let Ok(feeds) = self.inner.feeds.lock() {
            if let Some(feed) = feeds.get(&symbol.to_uppercase()) {
                feed.last_used_ns.store(now_ns(), Ordering::Relaxed);
            }
        }
    }

    /// How many feeds are open, and how many of them a bot depends on.
    ///
    /// Reported rather than inferred so a ceiling that is being hit is visible
    /// before it becomes "the chart is slow": the difference between 4 feeds and
    /// 32 is the difference between a platform used by hand and one being
    /// scraped, and only one of those is a reason to raise the limit.
    #[must_use]
    pub fn feed_counts(&self) -> (usize, usize) {
        let Ok(feeds) = self.inner.feeds.lock() else {
            return (0, 0);
        };
        let bots = feeds
            .values()
            .filter(|feed| feed.reason == FeedReason::Bot)
            .count();
        (feeds.len(), bots)
    }

    /// The symbols with an open feed, sorted.
    #[must_use]
    pub fn feed_symbols(&self) -> Vec<String> {
        let Ok(feeds) = self.inner.feeds.lock() else {
            return Vec::new();
        };
        let mut out: Vec<String> = feeds.keys().cloned().collect();
        out.sort();
        out
    }

    /// Close every feed nobody has used within [`Inner::feed_idle`].
    ///
    /// Returns the symbols closed, so the caller can log a number rather than
    /// leave an operator guessing whether the ceiling is being reached.
    ///
    /// A bot's feed is never touched, however idle: the bot holds a subscription
    /// to that symbol's bus, and closing the feed would leave it reading a
    /// channel nothing publishes into -- a running bot that silently stops
    /// deciding, which is the failure mode this whole module is arranged to
    /// avoid. A bot that ends calls [`Self::release_feed`], which is what makes
    /// its symbol reclaimable again.
    pub fn reclaim_idle_feeds(&self, now: i64) -> Vec<String> {
        let Ok(mut feeds) = self.inner.feeds.lock() else {
            return Vec::new();
        };
        let idle_ns = i64::try_from(self.inner.feed_idle.as_nanos()).unwrap_or(i64::MAX);

        let stale: Vec<String> = feeds
            .iter()
            .filter(|(_, feed)| feed.reason == FeedReason::Route)
            .filter(|(_, feed)| {
                now.saturating_sub(feed.last_used_ns.load(Ordering::Relaxed)) > idle_ns
            })
            .map(|(symbol, _)| symbol.clone())
            .collect();

        for symbol in &stale {
            if let Some(feed) = feeds.remove(symbol) {
                feed.handle.abort();
                info!(symbol, "closed an idle market feed");
                // The symbol is unwatched by choice now. Its age gauges freeze
                // and grow, so without this the stale rules would page on a
                // symbol nobody asked for about a minute after the reaper ran.
                if let Ok(mut alerter) = self.inner.alerter.lock() {
                    alerter.exclude_symbol(symbol);
                }
                self.forget_feed_clocks(symbol);
            }
        }
        stale
    }

    /// Drop every freshness clock a closed feed left behind.
    ///
    /// The exclusion mask stops the rules reading a reclaimed symbol, but the
    /// gauges themselves would keep growing forever -- and any future rule that
    /// scans all symbols would inherit the same bug. Removing the entries is
    /// the honest state: a closed feed has no age, not an infinite one.
    fn forget_feed_clocks(&self, symbol: &str) {
        if let Ok(mut ages) = self.inner.last_candle_ns.lock() {
            ages.remove(&symbol.to_uppercase());
        }
        self.inner.live.forget_book(symbol);
    }

    /// Drop a bot's claim on its symbol's feed.
    ///
    /// Called when a bot's task ends. The feed itself is left open -- its
    /// buffer is what makes the symbol's chart instant, and closing it would
    /// throw away the bars for a symbol the user is probably still watching.
    /// What changes is that the feed becomes *reclaimable*, so it will be
    /// closed by [`Self::reclaim_idle_feeds`] once nobody reads it.
    pub fn release_feed(&self, symbol: &str) {
        let Ok(mut feeds) = self.inner.feeds.lock() else {
            return;
        };
        let symbol = symbol.to_uppercase();
        if let Some(feed) = feeds.get_mut(&symbol) {
            // Downgraded rather than removed. A bot that stops and starts again
            // a second later should not pay a reconnect for it.
            feed.reason = FeedReason::Route;
            feed.last_used_ns.store(now_ns(), Ordering::Relaxed);
        }
    }

    /// Start a collector for a symbol if the feed is enabled and it has none.
    ///
    /// ## The ceiling
    ///
    /// At [`MAX_ACTIVE_FEEDS`] the least-recently-used **reclaimable** feed is
    /// closed to make room. Refusing instead was the first design, and it is
    /// wrong: the request that arrives at a full table is usually the one the
    /// user just clicked, so "too many symbols" would be the error a platform
    /// advertised as "any symbol" gives to the person using it normally.
    ///
    /// If every entry is a bot's, nothing is evicted and this symbol gets no
    /// feed -- the bots are the reason the table is full, and cancelling a
    /// trading bot's market data to serve a chart is the wrong way round. That
    /// case is logged at `warn`, and it is the only case where a chart can ask
    /// for a symbol and not get a feed.
    fn ensure_feed(&self, symbol: &str, reason: FeedReason) {
        if !self.inner.feed.is_on() {
            warn!(
                symbol,
                "no market feed is configured (MARKET_FEED is not a venue): this bot is running \
                 and will receive no candles until something publishes into the bus"
            );
            return;
        }

        let symbol = symbol.to_uppercase();
        // Watched again. Whatever the reaper or the ceiling said about this
        // symbol is superseded: a feed is opening, so staleness is a real
        // condition from here.
        if let Ok(mut alerter) = self.inner.alerter.lock() {
            alerter.watch_symbol(&symbol);
        }
        let Ok(mut feeds) = self.inner.feeds.lock() else {
            return;
        };
        if let Some(feed) = feeds.get_mut(&symbol) {
            // Already open. Record the use, and promote it if a bot now needs
            // it: a route-opened feed that a bot starts trading must stop being
            // reclaimable, or the bot loses its market data at the next sweep.
            feed.last_used_ns.store(now_ns(), Ordering::Relaxed);
            if reason == FeedReason::Bot {
                feed.reason = FeedReason::Bot;
            }
            return;
        }

        self.evict_for(&mut feeds, &symbol);

        let bus = Arc::clone(&self.inner.bus);
        let supervisor = self.clone();
        let mode = self.inner.feed;
        let last_used_ns = Arc::new(std::sync::atomic::AtomicI64::new(now_ns()));
        let for_task = symbol.clone();
        let task_used = Arc::clone(&last_used_ns);
        let handle = tokio::spawn(async move {
            if let Err(e) = run_market_feed(mode, bus, &for_task, supervisor, task_used).await {
                warn!(symbol = %for_task, "the market feed stopped: {e}");
            }
        });
        feeds.insert(
            symbol,
            Feed {
                handle: handle.abort_handle(),
                reason,
                last_used_ns,
            },
        );
    }

    /// Make room at the ceiling by closing the least-recently-used route feed.
    ///
    /// Split out so the choice is testable without opening a socket: the
    /// decision -- *which* entry goes -- is the part that can be wrong, and it
    /// is the part a test can drive through `feeds` directly.
    fn evict_for(&self, feeds: &mut HashMap<String, Feed>, incoming: &str) {
        if feeds.len() < MAX_ACTIVE_FEEDS {
            return;
        }

        let victim = feeds
            .iter()
            .filter(|(_, feed)| feed.reason == FeedReason::Route)
            .min_by_key(|(_, feed)| feed.last_used_ns.load(Ordering::Relaxed))
            .map(|(symbol, _)| symbol.clone());

        match victim {
            Some(symbol) => {
                if let Some(feed) = feeds.remove(&symbol) {
                    feed.handle.abort();
                    // Same reason as `reclaim_idle_feeds`: a deliberately
                    // closed feed must not read as a dead one to the alerter.
                    if let Ok(mut alerter) = self.inner.alerter.lock() {
                        alerter.exclude_symbol(&symbol);
                    }
                    self.forget_feed_clocks(&symbol);
                    info!(
                        evicted = %symbol,
                        incoming,
                        open = feeds.len(),
                        "at the market-feed ceiling; closed the least recently used feed"
                    );
                }
            }
            None => {
                // Every open feed is a bot's. Nothing is closed -- see the
                // rationale on `ensure_feed`.
                warn!(
                    incoming,
                    open = feeds.len(),
                    MAX_ACTIVE_FEEDS,
                    "every market feed is in use by a running bot; not opening one for this \
                     symbol. The bots keep their data and this symbol waits."
                );
            }
        }
    }

    /// Subscribe to what the running bots are doing.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<BotEvent> {
        self.inner.events.subscribe()
    }

    /// Publish a closed candle into the bus, and age the feed with it.
    ///
    /// The seam a test uses. The *live* collector does **not** go through it:
    /// `market-data` owns the candle builder and publishes straight into the bus.
    /// So this is only half of what a feed needs, and the other half -- recording
    /// that a candle arrived, publishing nothing -- is [`Self::note_candle`].
    /// Keeping them apart is what lets the collector's path age the feed without
    /// every bar being published twice.
    pub fn feed_candle(&self, candle: &Candle) {
        self.note_candle(candle);
        let bus = self.inner.bus.bus(&candle.symbol);
        bus.publish_candle(candle.clone());
        // The chart lane mirrors every closed candle: its subscribers render
        // frames in arrival order, so a close must be visible there too or a
        // chart could keep showing the forming bar of a bucket that ended.
        bus.publish_chart_candle(candle.clone());
    }

    /// Publish a **forming** candle onto the chart lane only.
    ///
    /// A forming bar is not a candle yet: it has no close that any strategy
    /// document could mean, so it never touches the closed lane the bots read
    /// and never counts as feed activity (the trade that produced it already
    /// did). Charts subscribe through [`Self::subscribe_chart_candles`].
    pub fn feed_forming_candle(&self, candle: &Candle) {
        self.inner
            .bus
            .bus(&candle.symbol)
            .publish_chart_candle(candle.clone());
    }

    /// Record that a candle arrived for its symbol, without publishing it.
    ///
    /// Stamped with the *arrival* time, not the candle's own open time.
    /// "Stale" means "nothing has arrived recently", and a replayed or
    /// backfilled candle arrives now even though it is about last Tuesday --
    /// so arrival time is the one that does not report a replay as an outage.
    ///
    /// The only writer of the input to `MD_FEED_AGE`, and it has to be reached by
    /// the path the live feed actually takes. The collector publishes its own
    /// candles, so a supervisor that stamped only inside [`Self::feed_candle`]
    /// served no `market_data_feed_age_seconds` at all -- and `stale_market_data`
    /// could not fire, however dead the feed was.
    pub fn note_candle(&self, candle: &Candle) {
        if let Ok(mut ages) = self.inner.last_candle_ns.lock() {
            ages.insert(candle.symbol.clone(), now_ns());
        }
    }

    /// Stamp that `symbol`'s feed just produced a **trade**.
    ///
    /// The feed-age clock used to be stamped only by closed candles, and that
    /// read as an outage that was not one: a thin market can go minutes without
    /// a 1m candle closing while its trades keep streaming every few seconds --
    /// `0GTRY` did exactly that, and `stale_market_data` fired at 137s with the
    /// feed perfectly healthy. A trade arriving is proof the venue connection
    /// is alive, which is the only thing "stale" is meant to measure, so it
    /// stamps the same clock.
    pub fn note_trade(&self, symbol: &str) {
        if let Ok(mut ages) = self.inner.last_candle_ns.lock() {
            ages.insert(symbol.to_uppercase(), now_ns());
        }
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
        self.spawn(
            bot_id,
            user_id,
            database,
            BotKind::Paper(Box::new(bot)),
            None,
        );
    }

    /// Start running a live bot against a real venue.
    ///
    /// The caller is responsible for having passed
    /// [`trading_engine::LiveGate`] -- this does not re-check it, because the
    /// check is a request-time decision about a *strategy* and by the time a
    /// task exists that decision has been made.
    /// `broker_account_id` is passed rather than read from the bot row: the
    /// supervisor must be able to answer "which bots trade this account" without
    /// a database round trip, because the question is asked *while* taking the
    /// lock that a disconnection is waiting on.
    pub fn start_live(
        &self,
        bot_id: Uuid,
        user_id: Uuid,
        database: db::Database,
        bot: LiveBot<BinanceRest>,
        broker_account_id: Option<Uuid>,
    ) {
        self.spawn(
            bot_id,
            user_id,
            database,
            BotKind::Live(Box::new(bot)),
            broker_account_id,
        );
    }

    /// The one task loop, shared by both bot kinds.
    fn spawn(
        &self,
        bot_id: Uuid,
        user_id: Uuid,
        database: db::Database,
        mut kind: BotKind,
        broker_account_id: Option<Uuid>,
    ) {
        if self.is_running(bot_id) {
            warn!(%bot_id, "bot is already running here; not starting a second task");
            return;
        }

        let symbol = kind.symbol().to_string();
        let venue = kind.venue().map(str::to_string);
        // A bot's feed is not reclaimable while the bot runs: the bot holds a
        // subscription to this symbol's bus, so closing the feed would leave it
        // reading a channel nothing publishes into -- a running bot that
        // silently stops deciding.
        self.ensure_feed(&symbol, FeedReason::Bot);

        let flush_interval = self.inner.flush_interval;
        let events = self.inner.events.clone();
        // Moved into the task so it can hand the feed back when it ends.
        let release = self.clone();
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
        let decision_path = kind.decision_path();

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
            // Hand the feed back to the reclaimable pool. A bot that stopped is
            // no longer a reason to hold a websocket open forever, but its bars
            // stay in the buffer for whoever is looking at the chart.
            release.release_feed(&symbol);
            info!(%bot_id, trades = kind.trade_count(), "bot finished");
        });

        if let Ok(mut running) = self.inner.running.lock() {
            running.insert(
                bot_id,
                RunningBot {
                    user_id,
                    handle,
                    stop,
                    wake,
                    paused,
                    kill,
                    venue,
                    broker_account_id,
                    decision_path,
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
        self.kill_matching(|bot| {
            bot.venue
                .as_deref()
                .is_some_and(|name| name == venue.as_str())
        })
    }

    /// Throw the kill switch on every running live bot trading `account_id`.
    ///
    /// What makes a disconnect real, and the finer-grained sibling of
    /// [`kill_venue`](Self::kill_venue): revoking a venue stops everything on
    /// it, while disconnecting one account must stop only the bots spending
    /// that account's money. A user with two accounts on one venue who
    /// disconnects one has not asked for the other to stop.
    ///
    /// `user_id` is part of the filter rather than only the account id because
    /// an account id reaches this from a URL. Two ids that must both match means
    /// a guess cannot stop somebody else's bot even if the account lookup above
    /// this were wrong.
    ///
    /// # Panics
    /// Never in practice: see [`kill_matching`](Self::kill_matching).
    pub fn kill_broker_account(&self, user_id: Uuid, account_id: Uuid) -> Vec<Uuid> {
        self.kill_matching(|bot| {
            bot.broker_account_id == Some(account_id) && bot.user_id == user_id
        })
    }

    /// Throw the kill switch on every running bot a predicate matches.
    ///
    /// One implementation for the two questions above, because the body was
    /// three lines of subtlety that both callers need to get right: set `kill`
    /// *and* `stop`, and notify, or the liquidation waits up to a flush tick.
    /// Written twice, one of the copies would eventually miss the `wake`.
    fn kill_matching(&self, matches: impl Fn(&RunningBot) -> bool) -> Vec<Uuid> {
        let Ok(running) = self.inner.running.lock() else {
            return Vec::new();
        };
        let mut killed = Vec::new();
        for (bot_id, bot) in running.iter() {
            if matches(bot) {
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

    /// Sweep idle feeds until the process ends.
    ///
    /// Spawned once by the gateway rather than run per request, because the
    /// rule it enforces is about *absence* of use: nothing calls a function
    /// when a user closes a tab, so a sweep driven by requests would only ever
    /// run while the thing it is meant to reclaim is still being used.
    ///
    /// The interval is deliberately a fraction of the idle window. Sweeping at
    /// the window itself would let a feed live up to twice its configured age,
    /// so a deployment that lowers `MARKET_FEED_IDLE` would not see the change
    /// take effect for that long.
    pub fn spawn_reaper(self: &Arc<Self>) {
        let supervisor = Arc::clone(self);
        let idle = supervisor.inner.feed_idle;
        let every = (idle / 4).max(Duration::from_secs(5));

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            // The first tick fires immediately; skip it so a fresh process does
            // not run a sweep before any feed exists.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let closed = supervisor.reclaim_idle_feeds(now_ns());
                if !closed.is_empty() {
                    info!(count = closed.len(), "reclaimed idle market feeds");
                }
            }
        });
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

/// Connect to the configured venue and keep the feed for one symbol running.
///
/// ## It does not build candles, and that is the point
///
/// The collector owns a `MultiTimeframeCandleBuilder` per symbol and publishes
/// every closed candle into the bus itself
/// (`market-data/src/exchanges/collector.rs`). This task used to build a
/// *second* one from the same trades, so every bar was published twice --
/// identical values, because both builders were fed the same trades -- and every
/// chart appended each bar twice. One builder, one publisher.
///
/// The one thing it *does* build here is the bar that is still forming. The
/// collector only publishes closed candles, so without this a chart that
/// opened cold would draw its last `1d` bar up to a day late. The forming bar
/// goes into the RAM history only -- never onto the bus, so nothing can
/// receive it twice.
///
/// What it does own is the collector: `Drop for Collector` aborts the pump, so a
/// task that returned early would stop the market with nothing anywhere saying
/// so. The `await` at the end is what keeps it alive.
///
/// ## Why the venue is a parameter and not a second function
///
/// This was `run_binance_feed`, and the only venue-specific thing in it is which
/// codec to build. A copy for a second venue would duplicate the subscribe
/// ordering, the `expect_book` clock, the recorder task and the deliberate
/// trailing `await` -- all four of which are load-bearing and were each a defect
/// once. The body is identical; only the constructor differs.
async fn run_market_feed(
    mode: FeedMode,
    bus: Arc<market_data::MarketBusRegistry>,
    symbol: &str,
    supervisor: BotSupervisor,
    last_used_ns: Arc<std::sync::atomic::AtomicI64>,
) -> Result<(), String> {
    use market_data::{BookBootstrap, Collector, CollectorConfig, ExchangeCollector};

    // Subscribed **before** the collector connects, for the same reason as the
    // candle watch below: a candle that closes in the gap between connecting and
    // subscribing is gone, and there is no second copy of it anywhere.
    let symbol_bus = bus.bus(symbol);
    let mut candles_rx = symbol_bus.subscribe_candles();
    let mut trades_rx = symbol_bus.subscribe_trades();

    // One collector, whichever venue `MARKET_FEED` names. The two arms differ in
    // the codec and in whether the book is bootstrapped over REST -- and nothing
    // else, which is the point of the codec seam.
    let mut collector: Box<dyn ExchangeCollector> = match mode {
        FeedMode::Binance => {
            let codec = Arc::new(market_data::BinanceCodec::new());
            let config = CollectorConfig::default();
            let rest_url = config.rest_url.clone();
            let fetcher: market_data::SnapshotFetcher = Arc::new(move |_, symbol, limit| {
                let rest_url = rest_url.clone();
                Box::pin(async move { fetch_binance_depth(&rest_url, &symbol, limit).await })
            });
            Box::new(Collector::new(codec, config, Arc::clone(&bus)).with_snapshot_fetcher(fetcher))
        }
        FeedMode::Bybit => {
            let codec = Arc::new(market_data::BybitCodec::spot());
            let config = CollectorConfig {
                book_bootstrap: BookBootstrap::InBand,
                rest_url: market_data::BYBIT_REST.to_string(),
                ..CollectorConfig::default()
            };
            Box::new(Collector::new(codec, config, Arc::clone(&bus)))
        }
        FeedMode::Off => {
            // Unreachable in practice: `ensure_feed` is what spawns this task and
            // it returns early when the mode is `Off`. Refusing rather than
            // opening a Binance feed for an operator who asked for none, which is
            // what an `unreachable!()` or a `_` arm would have done.
            return Err("no venue configured".into());
        }
    };

    collector.connect().await.map_err(|e| e.to_string())?;

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
    match collector.subscribe_order_book(symbol).await {
        Ok(()) => {
            // From this instant a book is expected, and that is the whole
            // reason the clock is started here rather than when the first book
            // arrives. `docs/19` row 24: a stream that never bridges onto its
            // snapshot publishes nothing at all, so if the age were measured
            // only from the newest book there would be no number to alert on
            // and the dead book would stay silent for the entire run.
            //
            // Not started when the subscription failed: that is a feed the
            // platform chose not to open, and it already says so in the log
            // above. The rule is for a book that was asked for and stopped.
            supervisor.live().expect_book(symbol, now_ns());
        }
        Err(e) => warn!(symbol, "the depth feed did not start: {e}"),
    }

    info!(symbol, "market feed connected");

    // Subscribed before anything is awaited, so a candle that closes between the
    // subscribe above and the watch below is not missed.
    let candles = supervisor.subscribe_candles(symbol);
    let history = supervisor.history();
    let live = supervisor.live();
    // A second handle for the recorder: it stamps the feed-age clock on every
    // trade (`note_trade`), and the watcher below takes the original.
    let stamping = supervisor.clone();
    let watcher = tokio::spawn(watch_feed_candles(
        candles,
        supervisor,
        Arc::clone(&last_used_ns),
    ));

    // ## Recording what the feed collects -- in RAM, never to disk
    //
    // `docs/19` row 21 asked why the live feed never reached storage. The answer
    // turned out to be that it *shouldn't*: one symbol's trades are ~110 MB/day
    // and the database has 6 GB in total for every symbol of every market this
    // platform will carry. So the row is closed the other way round -- the feed
    // records into a bounded in-memory buffer, and history older than the
    // buffer is fetched from the venue on demand.
    //
    // Closed bars come off the bus, so there is exactly one builder producing
    // them. Trades drive a second builder here purely for the *forming* bar,
    // which the collector never publishes.
    //
    // The same trades go onto the tape, and the book straight into the cache --
    // `/footprint` and `/orderbook` read both, and the tape is the only place
    // trades exist.
    let owned = symbol.to_string();
    let mut books_rx = symbol_bus.subscribe_orderbook();
    // The chart lane's only live publisher is this recorder, which is what
    // gives the lane its ordering contract (see
    // `MarketEventBus::publish_chart_candle`): closed candles land on it in the
    // order they closed, interleaved with at most one forming snapshot per
    // resolution per [`FORMING_PUBLISH_INTERVAL`].
    let chart_bus = Arc::clone(&symbol_bus);
    let recorder = tokio::spawn(async move {
        let mut builder = market_data::MultiTimeframeCandleBuilder::standard(&owned);
        // Forming bars are published on a clock rather than per trade: a busy
        // symbol prints hundreds of trades a second, and a chart that repaints
        // per trade costs sockets for a visual difference nobody can see. One
        // snapshot per second per resolution is what makes a `1d` bar visibly
        // alive while costing a handful of frames.
        let mut form_tick = tokio::time::interval(FORMING_PUBLISH_INTERVAL);
        loop {
            tokio::select! {
                Ok(candle) = candles_rx.recv() => {
                    history.record_closed(&candle);
                    chart_bus.publish_chart_candle(candle);
                }
                Ok(trade) = trades_rx.recv() => {
                    live.record_trade(&trade);
                    // A trade is proof the venue connection is alive. The age
                    // clock used to be stamped only by *closed* candles, so a
                    // thin market that legitimately went minutes between 1m
                    // closes (0GTRY) read as a dead feed while its trades kept
                    // flowing. The gauge's meaning stays "seconds since the
                    // feed last said anything".
                    stamping.note_trade(&trade.symbol);
                    let _closed = builder.on_trade(&trade);
                    for forming in builder.forming() {
                        history.record_forming(forming);
                    }
                }
                Ok(book) = books_rx.recv() => live.record_book(&book),
                _ = form_tick.tick() => {
                    // Closed candles queued ahead of this tick publish first.
                    // The select picks between ready branches arbitrarily, so
                    // without this drain a chart could receive the *next*
                    // bucket's forming frame before the previous bucket's
                    // close -- and a replace-or-append chart would then draw a
                    // stale bar after the one it is on.
                    while let Ok(candle) = candles_rx.try_recv() {
                        history.record_closed(&candle);
                        chart_bus.publish_chart_candle(candle);
                    }
                    for forming in builder.forming() {
                        chart_bus.publish_chart_candle(forming.clone());
                    }
                    // The 1m forming bar, whether or not a trade has arrived in
                    // this bucket yet. `forming()` only yields buckets a trade
                    // has already opened, so a 1m chart watching this feed had
                    // a newest bar that froze until the minute's first trade --
                    // and with it the live price level and the bar itself only
                    // moved when the minute closed. Every frame above already
                    // duplicates one the builder produced; this one guarantees
                    // the lane carries the shortest series too, which is the
                    // one a live chart is most likely watching.
                    if let Some(current) = builder.current_forming(analytics_core::Timeframe::M1) {
                        chart_bus.publish_chart_candle(current.clone());
                    }
                }
                else => break,
            }
        }
    });

    // Hold the collector for as long as the watch runs. Everything this feed
    // does is done by the pump inside `collector`; this task's only other job is
    // not to drop it.
    let _ = watcher.await;

    // The buffer outlives the feed -- that is the whole reason it is not the
    // collector's. But a recorder left running would block on `recv` forever
    // rather than noticing the socket is gone. Only reached when the feed has
    // already failed.
    recorder.abort();
    Ok(())
}

/// `GET /api/v3/depth`, for the Binance collector's book bootstrap.
///
/// ## Why the fetch is a closure and not a method
///
/// A collector is generic over its codec and knows nothing about URLs: it is
/// handed an `Fn` that turns a symbol into a snapshot, so a venue that carries
/// its book in-band never gets an HTTP path at all. Binance is the venue that
/// needs one -- it has no in-band reset boundary -- so its HTTP lives here, at
/// the one call site that builds a Binance collector.
///
/// The `rest_url` is taken from the config the collector was given rather than
/// hard-coded, so a test harness pointing at `127.0.0.1:1` keeps guaranteeing no
/// network access, exactly as `BackfillClient::new(url)` does for REST history.
async fn fetch_binance_depth(
    rest_url: &str,
    symbol: &str,
    limit: u16,
) -> Result<market_data::Incoming, market_data::MarketDataError> {
    use market_data::MarketDataError;

    let response = reqwest::Client::new()
        .get(format!("{rest_url}/api/v3/depth"))
        .query(&[("symbol", symbol), ("limit", &limit.to_string())])
        .send()
        .await
        .map_err(|e| MarketDataError::Transport(format!("depth snapshot request failed: {e}")))?
        .error_for_status()
        .map_err(|e| MarketDataError::Transport(format!("depth snapshot HTTP error: {e}")))?
        .json::<market_data::wire::DepthSnapshotResponse>()
        .await
        .map_err(|e| MarketDataError::Normalization(format!("depth snapshot decode: {e}")))?;

    market_data::exchanges::binance_codec::BinanceCodec::snapshot_from_rest(&response, symbol)
}

/// Age a feed from the candles its collector publishes.
///
/// Split out of [`run_market_feed`] because the failure it guards is silent:
/// `MD_FEED_AGE` is read by `stale_market_data` and written by nothing else, so
/// a supervisor that is never told a candle arrived serves a metric that does
/// not exist and a rule that cannot fire. [`BotSupervisor::feed_candle`] cannot
/// be used for this -- it publishes as well as stamps, and the collector has
/// already published.
async fn watch_feed_candles(
    mut candles: broadcast::Receiver<Candle>,
    supervisor: BotSupervisor,
    last_used_ns: Arc<std::sync::atomic::AtomicI64>,
) {
    loop {
        match candles.recv().await {
            Ok(candle) => {
                supervisor.note_candle(&candle);
                // A bar arriving is a use of this feed, even when nobody is
                // asking for the symbol right now. Without this a symbol that
                // is publishing steadily but is not the one on screen would be
                // closed for idleness -- and reopening it costs a REST round
                // trip to rebuild a buffer that was one bar from complete.
                last_used_ns.store(now_ns(), Ordering::Relaxed);
            }
            Err(RecvError::Lagged(skipped)) => {
                // Losing messages here loses *age* information, not data: the
                // chart holds its own subscription. Say so rather than let the
                // age jump by an unexplained amount.
                warn!(skipped, "the feed's own candle watch lagged");
            }
            Err(RecvError::Closed) => return,
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
        //
        // `from_env` reads the process environment, so this test is only about
        // the default when the variable is *absent*. `.env` ships
        // `MARKET_FEED=binance`, and any shell that has exported it -- e.g.
        // `set -a && . ./.env && cargo test`, which is how the gateway is run
        // against the real venue -- would otherwise fail here for a reason that
        // has nothing to do with the default and everything to do with the
        // caller's environment. Skip rather than lie in either direction.
        if let Ok(mode) = std::env::var("MARKET_FEED") {
            eprintln!("MARKET_FEED=`{mode}` is set; the default is not observable here");
            return;
        }
        assert_eq!(FeedMode::from_env(), FeedMode::Off);
    }

    #[test]
    fn a_nonsense_feed_mode_is_off_and_says_so() {
        // The real function with a value nobody recognises. `from_env` reads the
        // process environment, and this is the one case where writing it is
        // safe: the mode it sets is one that opens no sockets, and the test
        // restores whatever was there before. `unsafe` because setting an env
        // var in a multi-threaded test process races any concurrent reader --
        // which is why the assertion below does not depend on a *concurrent*
        // read, only on the value this thread wrote.
        let previous = std::env::var("MARKET_FEED").ok();
        // SAFETY: the value written is a string literal that outlives the call,
        // and the restore happens in the same test before it returns.
        unsafe { std::env::set_var("MARKET_FEED", "definitely-not-a-mode") };

        let resolved = FeedMode::from_env();

        match previous {
            Some(value) => unsafe { std::env::set_var("MARKET_FEED", value) },
            None => unsafe { std::env::remove_var("MARKET_FEED") },
        }

        // An unrecognised mode must not become a feed that opens sockets: a typo
        // in a deployment variable would otherwise start streaming from a venue
        // nobody asked for.
        assert_eq!(resolved, FeedMode::Off);
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

    #[tokio::test]
    async fn a_candle_the_collector_published_ages_the_feed() {
        // The live path never calls `feed_candle`: `market-data` owns the candle
        // builder and publishes straight into the bus. So the supervisor has to
        // be told separately, or `MD_FEED_AGE` is written by nothing and
        // `stale_market_data` cannot fire however dead the feed is. This drives
        // the same function the feed task runs, through the same bus.
        //
        // Removing the `note_candle` call inside `watch_feed_candles` fails this
        // test, which is the point of it.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        let candles = supervisor.subscribe_candles("BTCUSDT");
        // The same handle `ensure_feed` hands the real task, so this exercises
        // the production call shape rather than a simplified one.
        let last_used = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let watcher = tokio::spawn(watch_feed_candles(
            candles,
            supervisor.clone(),
            Arc::clone(&last_used),
        ));

        // Exactly what `handle_trade` in `market-data` does -- and the reason the
        // supervisor must not do it a second time: one publish, one bar.
        supervisor
            .inner
            .bus
            .bus("BTCUSDT")
            .publish_candle(test_candle("BTCUSDT", 0));

        // The watch is a task, so the stamp lands a scheduling hop later.
        let mut ages = Vec::new();
        for _ in 0..200 {
            ages = supervisor.feed_ages(now_ns());
            if !ages.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        watcher.abort();

        assert_eq!(
            ages.len(),
            1,
            "a candle the collector published must age the feed: MD_FEED_AGE has no \
             other writer, and stale_market_data cannot fire without it"
        );
        assert_eq!(ages[0].0, "BTCUSDT");
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
    fn a_trade_keeps_a_thin_market_off_the_stale_list() {
        // The 0GTRY incident, reduced: a feed whose last 1m candle closed over
        // two minutes ago but whose trades are still streaming seconds ago is
        // *alive*. Only a closed candle used to stamp the age clock, so the
        // rule read a healthy quiet market as an outage and fired
        // `stale_market_data` at 137s with the limit at 120s.
        let supervisor = BotSupervisor::new(FeedMode::Off);
        supervisor.feed_candle(&test_candle("0GTRY", 0));

        // The candle stamp is immediately superseded by a trade arriving now.
        supervisor.note_trade("0GTRY");
        let age = supervisor.feed_ages(now_ns());
        assert_eq!(age.len(), 1);
        assert!(
            age[0].1 < 1.0,
            "a trade that just arrived must read as a fresh feed, got {:.0}s",
            age[0].1
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

    /// A chart's history starts empty and is filled by the feed, so the registry
    /// has to exist before anything has been collected for it.
    #[test]
    fn a_fresh_supervisor_has_an_empty_history() {
        use analytics_core::Timeframe;

        let supervisor = BotSupervisor::new(FeedMode::Off);
        assert_eq!(supervisor.history().series_count(), 0);
        assert!(supervisor.history().symbols().is_empty());
        assert_eq!(
            supervisor.history().newest("BTCUSDT", Timeframe::M1),
            None,
            "no bars means no newest bar, not a newest bar at zero"
        );
    }

    /// What the feed records for one symbol is what `GET /candles` serves, and
    /// the forming bar has to be included or a chart's right-hand edge is up to
    /// one whole resolution stale.
    #[test]
    fn recording_a_bar_makes_it_available_to_a_chart() {
        use analytics_core::{Candle, Timeframe};

        let supervisor = BotSupervisor::new(FeedMode::Off);
        let history = supervisor.history();
        let bar = Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 60_000_000_000,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        };

        history.record_closed(&bar);
        assert_eq!(
            history.newest("BTCUSDT", Timeframe::M1),
            Some(60_000_000_000)
        );
        assert_eq!(history.symbols(), vec!["BTCUSDT"]);
        assert_eq!(history.depth().len(), 1);
    }

    // -----------------------------------------------------------------------
    // The feed ceiling and the idle sweep
    // -----------------------------------------------------------------------

    /// Insert a feed entry without opening a socket.
    ///
    /// The decision under test is *which* entry the ceiling picks, and that is
    /// a question about the map. Opening a real websocket to ask it would make
    /// the test depend on the venue and prove nothing extra.
    ///
    /// Spawns a task that parks until dropped, because an `AbortHandle` can only
    /// come from a spawned task -- so these tests are async. The task does
    /// nothing and touches nothing; it exists so `abort()` has something real
    /// to act on, which is the path being exercised.
    fn seed_feed(supervisor: &BotSupervisor, symbol: &str, reason: FeedReason, used_at: i64) {
        let handle = tokio::spawn(std::future::pending::<()>()).abort_handle();
        let previous = supervisor.inner.feeds.lock().expect("feeds").insert(
            symbol.to_string(),
            Feed {
                handle,
                reason,
                last_used_ns: Arc::new(std::sync::atomic::AtomicI64::new(used_at)),
            },
        );
        assert!(
            previous.is_none(),
            "{symbol} was seeded twice; the second insert would silently win"
        );
    }

    #[tokio::test]
    async fn a_full_table_evicts_the_least_recently_used_route_feed() {
        // The failure this guards: an unbounded table. `ensure_feed_for` is
        // reached by routes, so without a ceiling the number of sockets this
        // process holds is decided by how many symbol strings a client sends --
        // and Binance stops answering *everything* past 300 connections, the
        // traded symbol included.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        for i in 0..MAX_ACTIVE_FEEDS {
            seed_feed(
                &supervisor,
                &format!("SYM{i}USDT"),
                FeedReason::Route,
                i as i64,
            );
        }
        assert_eq!(supervisor.feed_counts().0, MAX_ACTIVE_FEEDS);

        let mut feeds = supervisor.inner.feeds.lock().expect("feeds");
        supervisor.evict_for(&mut feeds, "NEWUSDT");
        assert_eq!(
            feeds.len(),
            MAX_ACTIVE_FEEDS - 1,
            "exactly one entry must go, not more"
        );
        assert!(
            !feeds.contains_key("SYM0USDT"),
            "the least recently used entry is the one to close"
        );
        assert!(
            feeds.contains_key("SYM1USDT"),
            "and nothing else may be touched"
        );
    }

    #[tokio::test]
    async fn a_running_bots_feed_is_never_evicted() {
        // The bug this prevents: a chart asking for a symbol closes the feed a
        // trading bot is deciding on. The bot would then hold a subscription to
        // a bus nothing publishes into -- a running bot that silently stops
        // seeing bars, with the gap visible only in the audit trail.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        // The bot's feed is the *oldest*, so a naive LRU would pick it first.
        seed_feed(&supervisor, "BTCUSDT", FeedReason::Bot, 0);
        for i in 1..MAX_ACTIVE_FEEDS {
            seed_feed(
                &supervisor,
                &format!("SYM{i}USDT"),
                FeedReason::Route,
                i as i64,
            );
        }

        let mut feeds = supervisor.inner.feeds.lock().expect("feeds");
        supervisor.evict_for(&mut feeds, "NEWUSDT");
        assert!(
            feeds.contains_key("BTCUSDT"),
            "a bot's feed must survive the sweep however idle it looks"
        );
        assert!(
            !feeds.contains_key("SYM1USDT"),
            "the next-oldest went instead"
        );
    }

    #[tokio::test]
    async fn when_every_feed_belongs_to_a_bot_nothing_is_evicted() {
        // The other half of the rule: a chart asking for a new symbol must not
        // take market data away from a trading bot to get it. The request is
        // declined instead, and this asserts that the table is left *intact* --
        // a version that evicted anyway would still be "bounded", so a length
        // assertion alone would not catch it.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        for i in 0..MAX_ACTIVE_FEEDS {
            seed_feed(
                &supervisor,
                &format!("SYM{i}USDT"),
                FeedReason::Bot,
                i as i64,
            );
        }

        let mut feeds = supervisor.inner.feeds.lock().expect("feeds");
        supervisor.evict_for(&mut feeds, "NEWUSDT");
        assert_eq!(
            feeds.len(),
            MAX_ACTIVE_FEEDS,
            "nothing may be closed when every feed has a bot on it"
        );
    }

    #[tokio::test]
    async fn the_idle_sweep_closes_route_feeds_and_spares_bots() {
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        let now = 1_000_000_000_000_i64;
        let idle = i64::try_from(DEFAULT_FEED_IDLE.as_nanos()).expect("fits");

        // Route feeds: one long idle, one fresh.
        seed_feed(&supervisor, "OLDUSDT", FeedReason::Route, now - idle * 2);
        seed_feed(&supervisor, "NEWUSDT", FeedReason::Route, now - idle / 2);
        // A bot's feed, idle for far longer than any route feed.
        seed_feed(&supervisor, "BOTUSDT", FeedReason::Bot, now - idle * 100);

        let closed = supervisor.reclaim_idle_feeds(now);
        assert_eq!(closed, vec!["OLDUSDT".to_string()]);
        assert_eq!(
            supervisor.feed_symbols(),
            vec!["BOTUSDT".to_string(), "NEWUSDT".to_string()],
            "the fresh route feed stays and the bot's is untouchable"
        );
    }

    #[tokio::test]
    async fn reclaiming_a_feed_masks_and_forgets_its_symbol() {
        // The 0GTRY / MATICJPY pair from the logs: the reaper closed an idle
        // feed and a minute later the platform paged about a symbol nobody was
        // watching -- the alert clocks outlived the feed. Reclaiming must both
        // mask the symbol at the alerter and drop its freshness clocks.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        let now = 1_000_000_000_000_i64;
        let idle = i64::try_from(DEFAULT_FEED_IDLE.as_nanos()).expect("fits");
        seed_feed(&supervisor, "0GTRY", FeedReason::Route, now - idle * 2);

        // Leftover clocks from the feed that ran: a candle age and a book
        // expectation, both of which would otherwise age forever.
        supervisor.note_candle(&Candle {
            symbol: "0GTRY".into(),
            timeframe: analytics_core::Timeframe::M1,
            open_time: 0,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        });
        supervisor.live().expect_book("0GTRY", now);

        let closed = supervisor.reclaim_idle_feeds(now);
        assert_eq!(closed, vec!["0GTRY".to_string()]);
        assert!(
            supervisor
                .alerter()
                .lock()
                .expect("alerter")
                .is_excluded("0GTRY"),
            "a reclaimed symbol must be masked at the alerter"
        );
        assert!(
            supervisor.feed_ages(now).iter().all(|(s, _)| s != "0GTRY"),
            "the candle-age clock must be dropped, not left growing"
        );
        assert!(
            supervisor
                .live()
                .book_ages(now)
                .iter()
                .all(|(s, _)| s != "0GTRY"),
            "the book-age clock must be dropped, not left growing"
        );

        // And the mask is not a one-way door: asking for the symbol again
        // starts a feed and clears the exclusion.
        supervisor.ensure_feed_for("0GTRY");
        assert!(
            !supervisor
                .alerter()
                .lock()
                .expect("alerter")
                .is_excluded("0GTRY"),
            "a re-watched symbol must leave the exclusion mask"
        );
    }

    #[tokio::test]
    async fn releasing_a_bot_feed_makes_it_reclaimable() {
        // The handover that stops a stopped bot holding a socket forever, and
        // the reason `release_feed` downgrades rather than removes: a bot that
        // restarts a second later should not pay a reconnect.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        seed_feed(&supervisor, "BTCUSDT", FeedReason::Bot, 0);

        supervisor.release_feed("BTCUSDT");
        assert!(supervisor.feed_symbols().contains(&"BTCUSDT".to_string()));

        // Not idle yet. Measured against the real clock, because `release_feed`
        // stamps with `now_ns()` -- a synthetic "now" from the epoch would be
        // *earlier* than the release and the age would come out negative, which
        // the rule reads as "freshly used" and passes for the wrong reason.
        let idle = i64::try_from(DEFAULT_FEED_IDLE.as_nanos()).expect("fits");
        let just_after = now_ns();
        assert!(
            supervisor.reclaim_idle_feeds(just_after).is_empty(),
            "a feed released a moment ago is not idle yet -- otherwise reopening a bot \
             would reconnect on every sweep"
        );

        // Now genuinely idle, and the sweep may take it.
        let later = just_after + idle * 2;
        assert_eq!(
            supervisor.reclaim_idle_feeds(later),
            vec!["BTCUSDT".to_string()],
            "once nobody has read it for the idle window, a released feed is reclaimed"
        );
    }

    #[tokio::test]
    async fn a_feed_at_the_ceiling_exactly_is_not_evicted_for_itself() {
        // Off-by-one guard: `ensure_feed` calls `evict_for` only after checking
        // the symbol is absent, so the table can legitimately be at the ceiling
        // with room for the incoming entry.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        for i in 0..(MAX_ACTIVE_FEEDS - 1) {
            seed_feed(
                &supervisor,
                &format!("SYM{i}USDT"),
                FeedReason::Route,
                i as i64,
            );
        }
        let mut feeds = supervisor.inner.feeds.lock().expect("feeds");
        supervisor.evict_for(&mut feeds, "NEWUSDT");
        assert_eq!(
            feeds.len(),
            MAX_ACTIVE_FEEDS - 1,
            "one below the ceiling means the new entry fits; nothing is closed"
        );
    }

    #[tokio::test]
    async fn an_unknown_symbol_has_no_feed_to_touch() {
        // `touch_feed` is called on every read, including for a symbol whose
        // feed failed to open. It must be a no-op rather than a panic or an
        // insert -- inserting would let a request create a table entry with no
        // task behind it, which the sweep would then never reclaim.
        let supervisor = BotSupervisor::new(FeedMode::Binance);
        supervisor.touch_feed("NOPEUSDT");
        assert!(supervisor.feed_symbols().is_empty());
    }
}
