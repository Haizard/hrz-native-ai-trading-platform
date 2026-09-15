//! Running paper bots (`docs/11-BOT-TRADING-ENGINE.md`, `docs/12-API-GATEWAY.md`).
//!
//! ## What this owns
//!
//! `POST /bots` has to *start* something, and something that runs needs an
//! owner: a task, a lifetime, and a way to stop it. That is this module. The
//! `bots` row is the durable record; a [`RunningBot`] is the in-process
//! instance, and they are deliberately separate -- a deployment restarts, and
//! when it does the rows survive and the tasks do not.
//!
//! ## One feed per symbol, not one per bot
//!
//! Ten bots on BTCUSDT need one websocket, not ten. The supervisor owns a
//! [`MarketBusRegistry`] and starts a collector for a symbol when the first bot
//! needs it, stopping it when the last one goes. That is also why the bus is
//! the interface between the feed and the bots: a bot subscribes to a symbol,
//! and neither knows how many of the other there are.
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

use trading_engine::{BotSession, PaperBot};

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
        self.inner
            .bus
            .bus(&candle.symbol)
            .publish_candle(candle.clone());
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

    /// Start running a bot.
    ///
    /// Returns immediately: the bot runs in its own task. Starting a bot that is
    /// already running is a no-op rather than a second task, because two tasks
    /// on one bot row would interleave their decisions into the same audit
    /// trail and neither would be the truth.
    pub fn start(&self, bot_id: Uuid, user_id: Uuid, database: db::Database, mut bot: PaperBot) {
        if self.is_running(bot_id) {
            warn!(%bot_id, "bot is already running here; not starting a second task");
            return;
        }

        let symbol = bot.symbol().to_string();
        self.ensure_feed(&symbol);

        let flush_interval = self.inner.flush_interval;
        let events = self.inner.events.clone();
        let mut candles = self.inner.bus.bus(&symbol).subscribe_candles();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let (task_stop, task_paused, task_wake) =
            (Arc::clone(&stop), Arc::clone(&paused), Arc::clone(&wake));

        let handle = tokio::spawn(async move {
            let mut session = match BotSession::attach(
                &database,
                user_id,
                bot_id,
                serde_json::json!({ "attached": true }),
            )
            .await
            {
                Ok(session) => session,
                Err(e) => {
                    warn!(%bot_id, "could not attach the bot session: {e}");
                    return;
                }
            };

            let mut flush = tokio::time::interval(flush_interval);
            info!(%bot_id, %symbol, "bot started");
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
                                if let Some(record) = bot.on_candle(&candle) {
                                    events.send(BotEvent::Decision { bot_id, record }).ok();
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
                        if let Err(e) = session.flush(&mut bot).await {
                            warn!(%bot_id, "could not flush the audit trail: {e}");
                        }
                    },
                    // Falls through to the check below rather than breaking
                    // here, so the atomic flag stays the single source of truth.
                    // The notification only decides *when* the flag is read; a
                    // notification that arrives while the task is inside a flush
                    // is still honoured, because the permit is held until the
                    // next `notified()`.
                    _ = task_wake.notified() => {}
                }

                if task_stop.load(Ordering::Relaxed) || bot.is_halted() {
                    if bot.is_halted() {
                        info!(%bot_id, reason = ?bot.halt_reason(), "bot stopped by the risk engine");
                    }
                    break;
                }
            }

            if let Err(e) = session.finish(&mut bot).await {
                warn!(%bot_id, "could not finalise the bot: {e}");
            }
            events
                .send(BotEvent::Stopped {
                    bot_id,
                    trades: bot.trades().len(),
                    halt_reason: bot.halt_reason().map(str::to_string),
                })
                .ok();
            info!(%bot_id, trades = bot.trades().len(), "bot finished");
        });

        if let Ok(mut running) = self.inner.running.lock() {
            running.insert(
                bot_id,
                RunningBot {
                    handle,
                    stop,
                    wake,
                    paused,
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
    }

    #[test]
    fn a_fresh_supervisor_runs_nothing() {
        let supervisor = BotSupervisor::new(FeedMode::Off);
        assert_eq!(supervisor.running_count(), 0);
        assert_eq!(supervisor.feed_mode(), FeedMode::Off);
    }
}
