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

use analytics_core::types::Candle;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tracing::{info, warn};
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

/// A bot that is running in this process.
struct RunningBot {
    /// The task feeding it candles.
    handle: JoinHandle<()>,
    /// Set to stop it; the task checks this between candles.
    stop: Arc<AtomicBool>,
    /// Set while paused. The task keeps reading and discards.
    paused: Arc<AtomicBool>,
}

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
            }),
        }
    }

    /// How often a running bot flushes.
    #[must_use]
    pub fn flush_interval(&self) -> Duration {
        self.inner.flush_interval
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
        let mut candles = self.inner.bus.bus(&symbol).subscribe_candles();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let (task_stop, task_paused) = (Arc::clone(&stop), Arc::clone(&paused));

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

            loop {
                tokio::select! {
                    received = candles.recv() => match received {
                        Ok(candle) => {
                            if task_paused.load(Ordering::Relaxed) {
                                continue;
                            }
                            bot.on_candle(&candle);
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
                    }
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
            info!(%bot_id, trades = bot.trades().len(), "bot finished");
        });

        if let Ok(mut running) = self.inner.running.lock() {
            running.insert(
                bot_id,
                RunningBot {
                    handle,
                    stop,
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
        // The task wakes on its next candle or flush; the flush interval bounds
        // how long this waits.
        let _ = tokio::time::timeout(
            self.inner.flush_interval + Duration::from_secs(1),
            bot.handle,
        )
        .await;
        true
    }

    /// Stop every bot, for a graceful shutdown.
    pub async fn stop_all(&self) {
        let ids: Vec<Uuid> = self
            .inner
            .running
            .lock()
            .map_or_else(|_| Vec::new(), |running| running.keys().copied().collect());
        for id in ids {
            self.stop(id).await;
        }
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
