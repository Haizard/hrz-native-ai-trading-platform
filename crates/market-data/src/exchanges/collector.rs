//! One collector, driven by a [`WireCodec`].
//!
//! ## Why there is one of these and not one per venue
//!
//! `BinanceCollector` grew a reconnect loop with exponential backoff, a
//! periodic re-snapshot for books that never bridged (`RESYNC_SECS`), a
//! bounded diff buffer, a trade-id gap detector, a candle fanout across six
//! resolutions and an `MD_CLOSE_LATENCY` observation. **Every one of those was
//! a fixed defect.** A second `impl ExchangeCollector`, copied and pointed at
//! another venue, would duplicate all of them and re-open every one -- and the
//! copies would drift, because the thing that forced each fix was a live
//! failure on one venue that the other's tests would not reproduce.
//!
//! So the loop is here once, and everything venue-specific is behind
//! [`WireCodec`]. Adding a venue is a codec and a config; it is not a collector.
//!
//! ## What stays venue-aware
//!
//! Exactly one thing, and it is guarded rather than assumed: whether the venue
//! needs a **REST snapshot** to bootstrap a book. Binance has no in-band reset
//! boundary, so its book cannot bridge without one. A venue that marks its own
//! reset (`u == 1`) is subscribed cold and never calls REST. The collector asks
//! the codec, via [`BookBootstrap`], instead of asking the venue's name.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use analytics_core::{Candle, OrderBookSnapshot, Trade};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::codec::{DepthDiff, Frame, Incoming, Subscription, WireCodec};
use super::ExchangeCollector;
use crate::bus::MarketBusRegistry;
use crate::candle_builder::MultiTimeframeCandleBuilder;
use crate::error::MarketDataError;
use crate::health::{CollectorHealth, HealthStatus, TradeGapDetector};
use crate::orderbook::OrderBookSynchronizer;

/// How often an unsynced book is offered a fresh snapshot.
///
/// Only a venue that bootstraps over REST reaches this. The venue's REST
/// snapshot lags its own diff stream by a roughly constant number of update
/// ids, so the bridge only lands once `lastUpdateId` has walked forward into
/// the diffs we have retained. On Binance's BTCUSDT that took about thirty
/// seconds. Polling every two seconds costs one small HTTP request per unsynced
/// symbol and stops the moment the book syncs.
const RESYNC_SECS: u64 = 2;

/// How often the codec is offered a chance to send a heartbeat.
///
/// Bybit drops a silent socket after ten minutes and wants a ping roughly every
/// twenty seconds, so this is well inside both. A codec that wants no heartbeat
/// returns `None` and nothing is sent.
const HEARTBEAT_SECS: u64 = 20;

/// How a venue's order book gets its first snapshot.
///
/// This is the one place the collector cannot be venue-blind, and it is asked
/// rather than inferred. Assuming REST-as-Binance-does for a venue that carries
/// its own boundary would mean a book that could have synced on the first frame
/// instead spending thirty seconds polling a REST endpoint -- and, if that
/// endpoint is ever slow or blocked, a book that never syncs at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookBootstrap {
    /// The book arrives in-band; subscribe and install what the stream sends.
    InBand,
    /// The book must be fetched over REST, and re-fetched while unsynced.
    Rest,
}

/// Where a venue lives and how aggressively we read it.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// REST base URL. Only used when [`Self::book_bootstrap`] is
    /// [`BookBootstrap::Rest`].
    pub rest_url: String,
    /// Levels per side to publish in an order-book snapshot.
    pub depth_levels: usize,
    /// Minimum milliseconds between published order-book snapshots.
    pub orderbook_publish_ms: u64,
    /// `limit` for the REST depth snapshot.
    pub snapshot_limit: u16,
    /// First reconnect delay.
    pub initial_backoff_ms: u64,
    /// Reconnect delay ceiling.
    pub max_backoff_ms: u64,
    /// Whether this venue's book is bootstrapped from the stream or from REST.
    pub book_bootstrap: BookBootstrap,
    /// Disable the periodic REST re-snapshot.
    ///
    /// Only meaningful for diagnostics and tests (`xtask` runs the collector
    /// without a REST fallback to prove the in-band path alone works). Live
    /// callers leave this `false`; a book that never bridges is silent, and the
    /// poll is what stops that silence lasting the whole run.
    pub no_rest_resync: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            rest_url: "https://api.binance.com".to_string(),
            depth_levels: 50,
            orderbook_publish_ms: 1_000,
            snapshot_limit: 1000,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            book_bootstrap: BookBootstrap::Rest,
            no_rest_resync: false,
        }
    }
}

/// A REST depth fetch, as the collector needs it.
///
/// Boxed rather than generic so `Collector<C>` stays a single concrete type per
/// codec: the fetch is a closure the codec's module supplies, and the collector
/// never learns a URL shape.
pub type SnapshotFetcher = Arc<
    dyn Fn(
            String,
            String,
            u16,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Incoming, MarketDataError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug)]
enum CollectorCommand {
    Subscribe {
        subs: Vec<Subscription>,
    },
    Install(Incoming),
}

/// A live market-data collector, driven by `C`.
pub struct Collector<C: WireCodec> {
    codec: Arc<C>,
    config: CollectorConfig,
    fetcher: Option<SnapshotFetcher>,
    buses: Arc<MarketBusRegistry>,
    health: Arc<CollectorHealth>,
    cmd_tx: Option<mpsc::UnboundedSender<CollectorCommand>>,
    subscriptions: Vec<Subscription>,
    handle: Option<JoinHandle<()>>,
    /// The task copying health into the metric registry.
    ///
    /// Held so it can be stopped with the collector. Left running, it would go
    /// on publishing a dead collector's last state -- including
    /// `market_data_connected = 1` if that was the state when the collector was
    /// dropped -- and a gauge that says a collector is up after it has been
    /// destroyed is worse than no gauge.
    health_task: Option<JoinHandle<()>>,
}

impl<C: WireCodec + 'static> Collector<C> {
    /// Build a collector publishing onto `buses`.
    #[must_use]
    pub fn new(codec: Arc<C>, config: CollectorConfig, buses: Arc<MarketBusRegistry>) -> Self {
        Self {
            codec,
            config,
            fetcher: None,
            buses,
            health: Arc::new(CollectorHealth::new()),
            cmd_tx: None,
            subscriptions: Vec::new(),
            handle: None,
            health_task: None,
        }
    }

    /// Supply the REST snapshot fetcher this venue's book needs.
    ///
    /// Required when [`CollectorConfig::book_bootstrap`] is
    /// [`BookBootstrap::Rest`]; [`Self::connect`] refuses without it rather than
    /// opening a socket whose book can never bridge.
    #[must_use]
    pub fn with_snapshot_fetcher(mut self, fetcher: SnapshotFetcher) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    /// Live health counters.
    #[must_use]
    pub fn health_counters(&self) -> Arc<CollectorHealth> {
        self.health.clone()
    }

    /// Subscriptions currently active.
    #[must_use]
    pub fn subscriptions(&self) -> &[Subscription] {
        &self.subscriptions
    }

    /// The venue this collector speaks.
    #[must_use]
    pub fn codec(&self) -> &Arc<C> {
        &self.codec
    }

    fn command(&self, cmd: CollectorCommand) -> Result<(), MarketDataError> {
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| MarketDataError::Transport("collector is not connected".into()))?
            .send(cmd)
            .map_err(|e| MarketDataError::Transport(format!("command channel closed: {e}")))
    }

    /// Open the connection and start the ingest loop.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if the initial connection fails.
    pub async fn connect(&mut self) -> Result<(), MarketDataError> {
        if self.cmd_tx.is_some() {
            return Ok(());
        }

        if self.config.book_bootstrap == BookBootstrap::Rest && self.fetcher.is_none() {
            return Err(MarketDataError::Transport(format!(
                "{} needs a REST snapshot fetcher to bootstrap its book",
                self.codec.name()
            )));
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let codec = self.codec.clone();
        let config = self.config.clone();
        let buses = self.buses.clone();
        let health = self.health.clone();
        let fetcher = self.fetcher.clone();

        let handle = tokio::spawn(async move {
            run(codec, config, fetcher, buses, health, cmd_rx).await;
        });

        // Publish this collector's counters into the process registry.
        //
        // Spawned here rather than inside `run` so it survives a reconnect: the
        // reconnect loop tears the socket down and builds it again, and a
        // publisher that died with the socket would go quiet at exactly the
        // moment `market_data_connected` matters most. It is kept on the struct
        // so `Drop` stops it with everything else.
        self.health_task = Some(crate::health::spawn_health_publisher(
            self.health.clone(),
            observability::metrics::Registry::global_handle(),
            self.codec.name(),
        ));

        self.cmd_tx = Some(cmd_tx);
        self.handle = Some(handle);
        Ok(())
    }

    /// Subscribe to the trade stream for `symbol`.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if not connected.
    pub async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        let subs = vec![Subscription::Trades {
            symbol: symbol.to_string(),
        }];
        self.command(CollectorCommand::Subscribe { subs: subs.clone() })?;
        for sub in subs {
            if !self.subscriptions.contains(&sub) {
                self.subscriptions.push(sub);
            }
        }
        Ok(())
    }

    /// Subscribe to the order-book diff stream for `symbol`.
    ///
    /// ## Subscribe first, snapshot second
    ///
    /// On a venue that bootstraps over REST the diffs that arrive before the
    /// snapshot are retained by the synchronizer and replayed onto it, so
    /// subscribing first loses nothing and snapshotting first loses everything
    /// that happened in between. On a venue that carries its own boundary the
    /// snapshot is simply the first frame, and this is a no-op beyond the
    /// subscription itself.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Websocket`] if not connected, or
    /// [`MarketDataError::Normalization`] if the REST snapshot is unusable.
    pub async fn subscribe_order_book(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        let subs = vec![Subscription::OrderBook {
            symbol: symbol.to_string(),
        }];
        self.command(CollectorCommand::Subscribe { subs: subs.clone() })?;
        for sub in subs {
            if !self.subscriptions.contains(&sub) {
                self.subscriptions.push(sub);
            }
        }

        if self.config.book_bootstrap == BookBootstrap::InBand {
            return Ok(());
        }

        let Some(fetcher) = self.fetcher.clone() else {
            return Err(MarketDataError::Transport(
                "no snapshot fetcher configured".into(),
            ));
        };
        let incoming = fetcher(
            self.config.rest_url.clone(),
            symbol.to_string(),
            self.config.snapshot_limit,
        )
        .await?;

        self.command(CollectorCommand::Install(incoming))
    }

    /// Subscribe to the trade stream for `symbol`, if it has one.
    #[must_use]
    pub fn trade_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Trade>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_trades())
    }

    /// Subscribe to the order-book stream for `symbol`, if it has one.
    #[must_use]
    pub fn order_book_stream(&self, symbol: &str) -> Option<broadcast::Receiver<OrderBookSnapshot>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_orderbook())
    }

    /// Subscribe to the closed-candle stream for `symbol`, if it has one.
    #[must_use]
    pub fn candle_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Candle>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_candles())
    }

    /// Current health of the connection.
    #[must_use]
    pub fn health(&self) -> HealthStatus {
        self.health.snapshot(
            self.subscriptions
                .iter()
                .map(|s| format!("{}:{:?}", s.symbol(), s))
                .collect(),
        )
    }
}

#[async_trait]
impl<C: WireCodec + 'static> ExchangeCollector for Collector<C> {
    fn name(&self) -> &'static str {
        self.codec.name()
    }

    async fn connect(&mut self) -> Result<(), MarketDataError> {
        Collector::connect(self).await
    }

    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        Collector::subscribe_trades(self, symbol).await
    }

    async fn subscribe_order_book(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        Collector::subscribe_order_book(self, symbol).await
    }

    fn trade_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Trade>> {
        Collector::trade_stream(self, symbol)
    }

    fn order_book_stream(&self, symbol: &str) -> Option<broadcast::Receiver<OrderBookSnapshot>> {
        Collector::order_book_stream(self, symbol)
    }

    fn candle_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Candle>> {
        Collector::candle_stream(self, symbol)
    }

    fn health(&self) -> HealthStatus {
        Collector::health(self)
    }

    fn health_counters(&self) -> Arc<CollectorHealth> {
        Collector::health_counters(self)
    }
}

impl<C: WireCodec> Drop for Collector<C> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        if let Some(task) = self.health_task.take() {
            task.abort();
        }
    }
}

/// Per-symbol state that must survive reconnects.
struct SymbolState {
    candles: MultiTimeframeCandleBuilder,
    book: OrderBookSynchronizer,
    gap_detector: TradeGapDetector,
    last_book_publish: Instant,
    /// Depth diffs seen for this symbol, and whether the book ever synced.
    ///
    /// Only here so the one failure that is otherwise completely silent is not:
    /// diffs arriving forever against a book that never bridges means the DOM
    /// shows nothing and nothing anywhere says why.
    depth_diffs: u64,
    book_announced: bool,
}

/// Reconnect loop: connect, pump, back off, repeat.
async fn run<C: WireCodec + 'static>(
    codec: Arc<C>,
    config: CollectorConfig,
    fetcher: Option<SnapshotFetcher>,
    buses: Arc<MarketBusRegistry>,
    health: Arc<CollectorHealth>,
    mut cmd_rx: mpsc::UnboundedReceiver<CollectorCommand>,
) {
    let mut backoff = config.initial_backoff_ms;
    let name = codec.name();

    loop {
        health.set_connected(false);

        let url = codec.ws_url();
        match tokio_tungstenite::connect_async(&url).await {
            Ok((socket, _)) => {
                health.set_connected(true);
                backoff = config.initial_backoff_ms;
                debug!(venue = name, url = %url, "websocket connected");

                let outcome = pump(
                    socket,
                    codec.clone(),
                    &config,
                    fetcher.clone(),
                    &buses,
                    &health,
                    &mut cmd_rx,
                    &mut HashMap::new(),
                )
                .await;

                if let Err(e) = outcome {
                    warn!(venue = name, error = %e, "websocket pump ended");
                }
            }
            Err(e) => {
                warn!(venue = name, error = %e, "websocket connect failed");
            }
        }

        health.set_connected(false);
        health.record_reconnect();
        warn!(venue = name, delay_ms = backoff, "reconnecting");

        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff.saturating_mul(2)).min(config.max_backoff_ms);
    }
}

type PumpResult = Result<(), MarketDataError>;

/// Give every book that has diffs but no snapshot to bridge them a fresh one.
///
/// Without this the DOM is silently dead: the first snapshot's bridge event was
/// emitted before we subscribed, nothing re-tries, and the book never syncs for
/// the life of the process.
async fn resync_unsynced_books<C: WireCodec + 'static>(
    codec: &C,
    config: &CollectorConfig,
    fetcher: Option<&SnapshotFetcher>,
    states: &mut HashMap<String, SymbolState>,
) {
    if config.no_rest_resync || config.book_bootstrap != BookBootstrap::Rest {
        return;
    }
    let Some(fetcher) = fetcher else {
        return;
    };

    let unsynced: Vec<String> = states
        .iter()
        .filter(|(_, state)| state.depth_diffs > 0 && !state.book.is_synced())
        .map(|(symbol, _)| symbol.clone())
        .collect();

    for symbol in unsynced {
        match fetcher(config.rest_url.clone(), symbol.clone(), config.snapshot_limit).await {
            Ok(Incoming::BookSnapshot {
                symbol: canonical,
                bids,
                asks,
                last_update_id,
            }) => {
                let Some(state) = states.get_mut(&canonical) else {
                    continue;
                };
                state
                    .book
                    .set_snapshot(&bids, &asks, last_update_id, now_ns());
            }
            Ok(_) => {
                warn!(venue = codec.name(), symbol, "snapshot fetch returned non-snapshot data");
            }
            Err(e) => warn!(venue = codec.name(), symbol, error = %e, "depth snapshot refetch failed"),
        }
    }
}

/// Drive one established connection until it closes or errors.
#[allow(clippy::too_many_arguments)]
async fn pump<S, C>(
    socket: S,
    codec: Arc<C>,
    config: &CollectorConfig,
    fetcher: Option<SnapshotFetcher>,
    buses: &Arc<MarketBusRegistry>,
    health: &Arc<CollectorHealth>,
    cmd_rx: &mut mpsc::UnboundedReceiver<CollectorCommand>,
    states: &mut HashMap<String, SymbolState>,
) -> PumpResult
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    C: WireCodec + 'static,
{
    use tokio_tungstenite::tungstenite::Error as WsError;

    let (mut sink, mut stream) = socket.split();
    let mut resync = tokio::time::interval(Duration::from_secs(RESYNC_SECS));
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    // The one clock the codec's cadence is measured against. Created when the
    // pump starts, so a reconnect restarts the cadence with the socket -- a ping
    // schedule that survived a reconnect would ping a socket that never saw the
    // subscribe.
    let connected_at = Instant::now();
    // The first heartbeat tick fires immediately; skip it so a fresh connection
    // is not greeted with a control frame before it has answered the subscribe.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = resync.tick() => {
                resync_unsynced_books(codec.as_ref(), config, fetcher.as_ref(), states).await;
            }
            _ = heartbeat.tick() => {
                let now_ms = connected_at.elapsed().as_millis() as u64;
                if let Some(payload) = codec.heartbeat(now_ms) {
                    sink.send(Message::Text(payload.into())).await
                        .map_err(|e: WsError| MarketDataError::Transport(e.to_string()))?;
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    CollectorCommand::Subscribe { subs } => {
                        let text = codec.subscribe_payload(&subs)?;
                        sink.send(Message::Text(text.into())).await
                            .map_err(|e: WsError| MarketDataError::Transport(e.to_string()))?;
                    }
                    CollectorCommand::Install(incoming) => {
                        install(incoming, config, buses, health, states);
                    }
                }
            }
            next = stream.next() => {
                let Some(msg) = next else { return Ok(()) };
                match msg.map_err(|e: WsError| MarketDataError::Transport(e.to_string()))? {
                    Message::Text(text) => {
                        health.record_message(now_ns());
                        match codec.parse_frame(&text) {
                            Frame::Data(incoming) => install(*incoming, config, buses, health, states),
                            Frame::Malformed(reason) => {
                                health.record_decode_error();
                                warn!(venue = codec.name(), %reason, "frame did not decode");
                            }
                            Frame::Control => {}
                            Frame::Unrecognised => {
                                debug!(venue = codec.name(), "unrecognised frame ignored");
                            }
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {
                        // tungstenite answers pings itself; a pong is a liveness
                        // signal we have already recorded above.
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

/// Apply one decoded message to the per-symbol state and the bus.
fn install(
    incoming: Incoming,
    config: &CollectorConfig,
    buses: &Arc<MarketBusRegistry>,
    health: &Arc<CollectorHealth>,
    states: &mut HashMap<String, SymbolState>,
) {
    match incoming {
        Incoming::Trade(trade) => handle_trade(trade, buses, health, states),
        Incoming::Trades(trades) => {
            for trade in trades {
                handle_trade(trade, buses, health, states);
            }
        }
        Incoming::BookSnapshot {
            symbol,
            bids,
            asks,
            last_update_id,
        } => {
            let state = symbol_state(states, buses, &symbol);
            state
                .book
                .set_snapshot(&bids, &asks, last_update_id, now_ns());
        }
        Incoming::BookDelta(diff) => handle_depth(diff, config, buses, states),
    }
}

fn symbol_state<'a>(
    states: &'a mut HashMap<String, SymbolState>,
    buses: &Arc<MarketBusRegistry>,
    symbol: &str,
) -> &'a mut SymbolState {
    states.entry(symbol.to_string()).or_insert_with(|| {
        let _ = buses.bus(symbol); // make sure the bus exists
        SymbolState {
            candles: MultiTimeframeCandleBuilder::standard(symbol),
            book: OrderBookSynchronizer::new(symbol),
            gap_detector: TradeGapDetector::new(),
            last_book_publish: Instant::now(),
            depth_diffs: 0,
            book_announced: false,
        }
    })
}

/// Health is written here rather than at the subscribe site, because a gap is
/// only observable on the trade path.
fn handle_trade(
    trade: Trade,
    buses: &Arc<MarketBusRegistry>,
    health: &Arc<CollectorHealth>,
    states: &mut HashMap<String, SymbolState>,
) {
    let symbol = trade.symbol.clone();
    let state = symbol_state(states, buses, &symbol);

    if let Some((expected, actual)) = state.gap_detector.on_trade(&trade) {
        health.record_gap();
        warn!(symbol = %symbol, expected, actual, "trade id gap detected; resync needed");
    }

    let closed = state.candles.on_trade(&trade);
    let bus = buses.bus(&symbol);
    bus.publish_trade(trade);
    for candle in closed {
        // How late this candle is relative to its own close.
        //
        // The one number that separates "the venue is slow" from "we are slow":
        // a candle closing at 12:05:00 that reaches the bus at 12:05:02 is a
        // two-second delay in a strategy that thinks it is trading the close.
        // Negative is clamped rather than reported, because a candle whose
        // close is in the future means a clock disagreement, not a fast feed.
        let closed_at = candle.open_time + candle.timeframe.nanos();
        let latency = (now_ns().saturating_sub(closed_at)) as f64 / 1_000_000_000.0;
        observability::metrics::Registry::global().observe(
            observability::metrics::MD_CLOSE_LATENCY,
            "Seconds from a candle's close to it reaching the bus",
            &observability::metrics::Labels::new(&[("symbol", symbol.as_str())]),
            latency.max(0.0),
        );
        bus.publish_candle(candle);
    }
}

fn handle_depth(
    diff: DepthDiff,
    config: &CollectorConfig,
    buses: &Arc<MarketBusRegistry>,
    states: &mut HashMap<String, SymbolState>,
) {
    let symbol = diff.symbol.clone();
    let state = symbol_state(states, buses, &symbol);
    let ts = now_ns();

    state.depth_diffs += 1;
    if !state.book.is_synced() && state.depth_diffs % 100 == 1 {
        warn!(
            symbol,
            diffs = state.depth_diffs,
            snapshot = ?state.book.snapshot_id(),
            first = diff.first_update_id,
            last = diff.final_update_id,
            "depth diffs are arriving but the book has not bridged onto its snapshot"
        );
    }

    state.book.on_diff(
        crate::orderbook::DepthDiff {
            first_update_id: diff.first_update_id,
            final_update_id: diff.final_update_id,
            bids: diff.bids,
            asks: diff.asks,
        },
        ts,
    );

    publish_book_if_due(state, config, buses, &symbol);
}

fn publish_book_if_due(
    state: &mut SymbolState,
    config: &CollectorConfig,
    buses: &Arc<MarketBusRegistry>,
    symbol: &str,
) {
    let due = state.last_book_publish.elapsed() >= Duration::from_millis(config.orderbook_publish_ms);
    if !due {
        return;
    }

    if let Some(book) = state.book.book() {
        if state.book.is_synced() {
            if !state.book_announced {
                state.book_announced = true;
                info!(symbol, diffs = state.depth_diffs, "order book synced");
            }
            buses
                .bus(symbol)
                .publish_orderbook(book.snapshot(config.depth_levels));
        }
    }
    state.last_book_publish = Instant::now();
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchanges::binance_codec::BinanceCodec;
    use analytics_core::Timeframe;

    fn collector() -> Collector<BinanceCodec> {
        let registry = Arc::new(MarketBusRegistry::new());
        Collector::new(
            Arc::new(BinanceCodec::new()),
            CollectorConfig::default(),
            registry,
        )
    }

    /// A collector that cannot hand out a candle stream cannot have its candles
    /// persisted, however well it aggregates them.
    ///
    /// Constructing without `connect()` keeps this offline: it exercises the
    /// subscription wiring, not the socket.
    #[test]
    fn the_collector_hands_out_a_reachable_candle_stream() {
        let registry = Arc::new(MarketBusRegistry::new());
        let collector = Collector::new(
            Arc::new(BinanceCodec::new()),
            CollectorConfig::default(),
            Arc::clone(&registry),
        );

        let mut rx = collector
            .candle_stream("btcusdt")
            .expect("a collector aggregates candles and must expose them");

        registry.bus("BTCUSDT").publish_candle(Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: 1_700_000_000_000_000_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 3.0,
            buy_volume: 2.0,
            sell_volume: 1.0,
        });

        let received = rx.try_recv().expect("the subscription must be live");
        assert_eq!(received.timeframe, Timeframe::M5);
        assert_eq!(received.close, 100.5);
    }

    /// The three streams are independent channels, not one shared queue.
    #[test]
    fn the_candle_stream_is_not_the_trade_or_book_stream() {
        let registry = Arc::new(MarketBusRegistry::new());
        let collector = Collector::new(
            Arc::new(BinanceCodec::new()),
            CollectorConfig::default(),
            Arc::clone(&registry),
        );

        let mut candles = collector.candle_stream("BTCUSDT").expect("candle stream");
        let bus = registry.bus("BTCUSDT");

        bus.publish_trade(Trade {
            symbol: "BTCUSDT".into(),
            price: 100.0,
            quantity: 1.0,
            timestamp: 1,
            is_buyer_maker: false,
            trade_id: 1,
        });
        bus.publish_orderbook(OrderBookSnapshot {
            symbol: "BTCUSDT".into(),
            timestamp: 1,
            bids: vec![],
            asks: vec![],
        });

        assert_eq!(
            candles.try_recv().unwrap_err(),
            broadcast::error::TryRecvError::Empty,
            "a trade or a snapshot must not arrive on the candle channel"
        );
    }

    /// A REST-bootstrapped venue must not open a socket without a fetcher.
    ///
    /// The failure it prevents is the silent one: a book with diffs arriving
    /// forever and no snapshot to bridge them, which looks exactly like a quiet
    /// market from every panel that reads it.
    #[tokio::test]
    async fn a_rest_bootstrapped_venue_refuses_to_connect_without_a_fetcher() {
        let mut collector = collector();
        assert_eq!(collector.config.book_bootstrap, BookBootstrap::Rest);
        let err = Collector::connect(&mut collector)
            .await
            .expect_err("connecting without a snapshot fetcher must be refused");
        assert!(
            matches!(err, MarketDataError::Transport(ref m) if m.contains("snapshot fetcher")),
            "got {err:?}"
        );
    }

    /// The control for the above: an in-band venue connects with no fetcher,
    /// because it never needs one.
    #[tokio::test]
    async fn an_in_band_venue_accepts_connect_without_a_fetcher() {
        let registry = Arc::new(MarketBusRegistry::new());
        let mut collector = Collector::new(
            Arc::new(BinanceCodec::new()),
            CollectorConfig {
                book_bootstrap: BookBootstrap::InBand,
                ..CollectorConfig::default()
            },
            registry,
        );

        // No real socket is opened here: `connect` spawns the loop, which fails
        // to reach the network and backs off. What is under test is the guard,
        // not the socket.
        assert!(Collector::connect(&mut collector).await.is_ok());
    }

    /// The collector's name is the codec's name, so a metric label and a REST
    /// log line for the same venue cannot disagree.
    #[test]
    fn the_name_comes_from_the_codec() {
        assert_eq!(ExchangeCollector::name(&collector()), "binance");
    }

    #[tokio::test]
    async fn subscribing_before_connecting_is_an_error_not_a_silent_no_op() {
        // `subscribe_*` needs a channel, so this asserts the negative: with no
        // connection it is an error rather than a silent no-op.
        let mut collector = collector();
        assert!(Collector::subscribe_trades(&mut collector, "BTCUSDT")
            .await
            .is_err());
        assert!(collector.subscriptions().is_empty());
    }
}
