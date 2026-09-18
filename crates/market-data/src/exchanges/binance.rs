//! Binance collector.
//!
//! Uses the combined WebSocket endpoint (`/stream`) so one connection can carry
//! both the trade stream and the depth-diff stream, subscribing with
//! `{"method":"SUBSCRIBE"}` control messages.
//!
//! Reconnect is automatic with exponential backoff, and after every reconnect
//! the collector re-SUBSCRIBEs everything it had before -- without that, a
//! dropped socket would silently turn into "no data" rather than an error.

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

use super::wire::{
    self, CombinedMessage, DepthMessage, DepthSnapshotResponse, SubscribeMessage, TradeMessage,
};
use super::ExchangeCollector;
use crate::bus::MarketBusRegistry;
use crate::candle_builder::MultiTimeframeCandleBuilder;
use crate::error::MarketDataError;
use crate::health::{CollectorHealth, HealthStatus, TradeGapDetector};
use crate::orderbook::OrderBookSynchronizer;

/// Where Binance lives and how aggressively we read it.
#[derive(Debug, Clone)]
pub struct BinanceConfig {
    /// Combined WebSocket endpoint.
    pub ws_url: String,
    /// REST base URL.
    pub rest_url: String,
    /// Levels per side to publish in an order-book snapshot.
    pub depth_levels: usize,
    /// Minimum milliseconds between published order-book snapshots.
    pub orderbook_publish_ms: u64,
    /// `limit` for the REST depth snapshot (1000 is Binance's max).
    pub snapshot_limit: u16,
    /// First reconnect delay.
    pub initial_backoff_ms: u64,
    /// Reconnect delay ceiling.
    pub max_backoff_ms: u64,
}

impl Default for BinanceConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://stream.binance.com:9443/stream".to_string(),
            rest_url: "https://api.binance.com".to_string(),
            depth_levels: 50,
            orderbook_publish_ms: 1_000,
            snapshot_limit: 1000,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
        }
    }
}

#[derive(Debug)]
enum CollectorCommand {
    Subscribe {
        params: Vec<String>,
    },
    SetSnapshot {
        symbol: String,
        bids: Vec<(f64, f64)>,
        asks: Vec<(f64, f64)>,
        last_update_id: u64,
    },
}

/// A live Binance market-data collector.
pub struct BinanceCollector {
    config: BinanceConfig,
    buses: Arc<MarketBusRegistry>,
    health: Arc<CollectorHealth>,
    client: reqwest::Client,
    cmd_tx: Option<mpsc::UnboundedSender<CollectorCommand>>,
    streams: Vec<String>,
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

impl BinanceCollector {
    /// Build a collector publishing onto `buses`.
    #[must_use]
    pub fn new(config: BinanceConfig, buses: Arc<MarketBusRegistry>) -> Self {
        Self {
            config,
            buses,
            health: Arc::new(CollectorHealth::new()),
            client: reqwest::Client::new(),
            cmd_tx: None,
            streams: Vec::new(),
            handle: None,
            health_task: None,
        }
    }

    /// A collector with default endpoints.
    #[must_use]
    pub fn with_defaults(buses: Arc<MarketBusRegistry>) -> Self {
        Self::new(BinanceConfig::default(), buses)
    }

    /// Live health counters.
    #[must_use]
    pub fn health_counters(&self) -> Arc<CollectorHealth> {
        self.health.clone()
    }

    /// Streams currently subscribed.
    #[must_use]
    pub fn streams(&self) -> &[String] {
        &self.streams
    }

    fn command(&self, cmd: CollectorCommand) -> Result<(), MarketDataError> {
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| MarketDataError::Transport("collector is not connected".into()))?
            .send(cmd)
            .map_err(|e| MarketDataError::Transport(format!("command channel closed: {e}")))
    }

    /// Fetch the REST depth snapshot used to bootstrap the local book.
    async fn fetch_depth_snapshot(
        &self,
        symbol: &str,
    ) -> Result<DepthSnapshotResponse, MarketDataError> {
        fetch_depth_snapshot(&self.client, &self.config, symbol).await
    }

    /// Stream name for the trade feed.
    #[must_use]
    pub fn trade_stream_name(symbol: &str) -> String {
        format!("{}@trade", symbol.to_ascii_lowercase())
    }

    /// Stream name for the depth-diff feed.
    #[must_use]
    pub fn depth_stream_name(symbol: &str) -> String {
        format!("{}@depth@100ms", symbol.to_ascii_lowercase())
    }
}

#[async_trait]
impl ExchangeCollector for BinanceCollector {
    fn name(&self) -> &'static str {
        "binance"
    }

    async fn connect(&mut self) -> Result<(), MarketDataError> {
        if self.cmd_tx.is_some() {
            return Ok(());
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let config = self.config.clone();
        let buses = self.buses.clone();
        let health = self.health.clone();

        let handle = tokio::spawn(async move { run(config, buses, health, cmd_rx).await });

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
            self.name(),
        ));

        self.cmd_tx = Some(cmd_tx);
        self.handle = Some(handle);
        Ok(())
    }

    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        let stream = Self::trade_stream_name(symbol);
        self.command(CollectorCommand::Subscribe {
            params: vec![stream.clone()],
        })?;
        if !self.streams.contains(&stream) {
            self.streams.push(stream);
        }
        Ok(())
    }

    async fn subscribe_order_book(&mut self, symbol: &str) -> Result<(), MarketDataError> {
        let stream = Self::depth_stream_name(symbol);

        // Subscribe first so no diff is missed; diffs that arrive before the
        // snapshot are buffered by the synchronizer and replayed onto it.
        self.command(CollectorCommand::Subscribe {
            params: vec![stream.clone()],
        })?;
        if !self.streams.contains(&stream) {
            self.streams.push(stream);
        }

        let snapshot = self.fetch_depth_snapshot(symbol).await?;
        let bids = wire::parse_levels(&snapshot.bids, "bid")?;
        let asks = wire::parse_levels(&snapshot.asks, "ask")?;

        self.command(CollectorCommand::SetSnapshot {
            symbol: symbol.to_uppercase(),
            bids,
            asks,
            last_update_id: snapshot.last_update_id,
        })
    }

    fn trade_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Trade>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_trades())
    }

    fn order_book_stream(&self, symbol: &str) -> Option<broadcast::Receiver<OrderBookSnapshot>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_orderbook())
    }

    fn candle_stream(&self, symbol: &str) -> Option<broadcast::Receiver<Candle>> {
        let bus = self.buses.bus(&symbol.to_uppercase());
        Some(bus.subscribe_candles())
    }

    fn health(&self) -> HealthStatus {
        self.health.snapshot(self.streams.clone())
    }
}

impl Drop for BinanceCollector {
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
async fn run(
    config: BinanceConfig,
    buses: Arc<MarketBusRegistry>,
    health: Arc<CollectorHealth>,
    mut cmd_rx: mpsc::UnboundedReceiver<CollectorCommand>,
) {
    let mut backoff = config.initial_backoff_ms;

    loop {
        health.set_connected(false);

        match tokio_tungstenite::connect_async(&config.ws_url).await {
            Ok((socket, _)) => {
                health.set_connected(true);
                backoff = config.initial_backoff_ms;
                debug!(url = %config.ws_url, "binance websocket connected");

                let outcome = pump(
                    socket,
                    &config,
                    &buses,
                    &health,
                    &mut cmd_rx,
                    &mut HashMap::new(),
                )
                .await;

                if let Err(e) = outcome {
                    warn!(error = %e, "binance websocket pump ended");
                }
            }
            Err(e) => {
                warn!(error = %e, "binance websocket connect failed");
            }
        }

        health.set_connected(false);
        health.record_reconnect();
        warn!(delay_ms = backoff, "reconnecting to binance");

        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff.saturating_mul(2)).min(config.max_backoff_ms);
    }
}

type PumpResult = Result<(), MarketDataError>;

/// How often an unsynced book is given a fresh snapshot to try.
///
/// The venue's REST snapshot lags its own diff stream by a roughly constant
/// number of update ids, so the bridge only lands once `lastUpdateId` has walked
/// forward into the diffs we have retained. On BTCUSDT that took about thirty
/// seconds. Polling every two seconds costs one small HTTP request per unsynced
/// symbol and stops the moment the book syncs.
const RESYNC_SECS: u64 = 2;

/// `GET /api/v3/depth`, for both the first snapshot and the retries.
async fn fetch_depth_snapshot(
    client: &reqwest::Client,
    config: &BinanceConfig,
    symbol: &str,
) -> Result<DepthSnapshotResponse, MarketDataError> {
    let url = format!("{}/api/v3/depth", config.rest_url);
    client
        .get(url)
        .query(&[
            ("symbol", symbol.to_uppercase()),
            ("limit", config.snapshot_limit.to_string()),
        ])
        .send()
        .await
        .map_err(|e| MarketDataError::Transport(format!("depth snapshot request failed: {e}")))?
        .error_for_status()
        .map_err(|e| MarketDataError::Transport(format!("depth snapshot HTTP error: {e}")))?
        .json::<DepthSnapshotResponse>()
        .await
        .map_err(|e| MarketDataError::Normalization(format!("depth snapshot decode: {e}")))
}

/// Give every book that has diffs but no snapshot to bridge them a fresh one.
///
/// Without this the DOM is silently dead: the first snapshot's bridge event was
/// emitted before we subscribed, nothing re-tries, and the book never syncs for
/// the life of the process.
async fn resync_unsynced_books(
    client: &reqwest::Client,
    config: &BinanceConfig,
    states: &mut HashMap<String, SymbolState>,
) {
    let unsynced: Vec<String> = states
        .iter()
        .filter(|(_, state)| state.depth_diffs > 0 && !state.book.is_synced())
        .map(|(symbol, _)| symbol.clone())
        .collect();

    for symbol in unsynced {
        let snapshot = match fetch_depth_snapshot(client, config, &symbol).await {
            Ok(snapshot) => snapshot,
            Err(e) => {
                warn!(symbol, error = %e, "depth snapshot refetch failed");
                continue;
            }
        };

        let bids = wire::parse_levels(&snapshot.bids, "bid");
        let asks = wire::parse_levels(&snapshot.asks, "ask");
        let (Ok(bids), Ok(asks)) = (bids, asks) else {
            warn!(symbol, "depth snapshot levels were unusable");
            continue;
        };

        let Some(state) = states.get_mut(&symbol) else {
            continue;
        };
        state
            .book
            .set_snapshot(&bids, &asks, snapshot.last_update_id, now_ns());
    }
}

/// Drive one established connection until it closes or errors.
async fn pump<S>(
    socket: S,
    config: &BinanceConfig,
    buses: &Arc<MarketBusRegistry>,
    health: &Arc<CollectorHealth>,
    cmd_rx: &mut mpsc::UnboundedReceiver<CollectorCommand>,
    states: &mut HashMap<String, SymbolState>,
) -> PumpResult
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    use tokio_tungstenite::tungstenite::Error as WsError;

    let (mut sink, mut stream) = socket.split();
    let client = reqwest::Client::new();
    let mut resync = tokio::time::interval(Duration::from_secs(RESYNC_SECS));

    loop {
        tokio::select! {
            _ = resync.tick() => {
                resync_unsynced_books(&client, config, states).await;
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    CollectorCommand::Subscribe { params } => {
                        let msg = SubscribeMessage { method: "SUBSCRIBE", params, id: 1 };
                        let text = serde_json::to_string(&msg)
                            .map_err(|e| MarketDataError::Normalization(e.to_string()))?;
                        sink.send(Message::Text(text.into())).await
                            .map_err(|e: WsError| MarketDataError::Transport(e.to_string()))?;
                    }
                    CollectorCommand::SetSnapshot { symbol, bids, asks, last_update_id } => {
                        let state = symbol_state(states, buses, &symbol);
                        state.book.set_snapshot(&bids, &asks, last_update_id, now_ns());
                    }
                }
            }
            next = stream.next() => {
                let Some(msg) = next else { return Ok(()) };
                match msg.map_err(|e: WsError| MarketDataError::Transport(e.to_string()))? {
                    Message::Text(text) => {
                        health.record_message(now_ns());
                        handle_text(&text, config, buses, health, states)?;
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
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

fn handle_text(
    text: &str,
    config: &BinanceConfig,
    buses: &Arc<MarketBusRegistry>,
    health: &Arc<CollectorHealth>,
    states: &mut HashMap<String, SymbolState>,
) -> PumpResult {
    let envelope: CombinedMessage = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => return Ok(()), // control/ack frames aren't JSON envelopes
    };

    if envelope.stream.ends_with("@trade") {
        let message: TradeMessage = serde_json::from_value(envelope.data)
            .map_err(|e| MarketDataError::Normalization(e.to_string()))?;
        let trade = message.to_trade()?;
        handle_trade(trade, buses, health, states);
    } else if envelope.stream.contains("@depth") {
        let message: DepthMessage = serde_json::from_value(envelope.data)
            .map_err(|e| MarketDataError::Normalization(e.to_string()))?;
        handle_depth(message, config, buses, states)?;
    }

    Ok(())
}

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
    message: DepthMessage,
    config: &BinanceConfig,
    buses: &Arc<MarketBusRegistry>,
    states: &mut HashMap<String, SymbolState>,
) -> PumpResult {
    // Stream names are lowercase; symbols on the bus are uppercase.
    let symbol = message.symbol.to_uppercase();
    let state = symbol_state(states, buses, &symbol);

    let bids = wire::parse_levels(&message.bids, "bid")?;
    let asks = wire::parse_levels(&message.asks, "ask")?;
    let ts = now_ns();

    state.depth_diffs += 1;
    if !state.book.is_synced() && state.depth_diffs % 100 == 1 {
        warn!(
            symbol,
            diffs = state.depth_diffs,
            snapshot = ?state.book.snapshot_id(),
            first = message.first_update_id,
            last = message.final_update_id,
            "depth diffs are arriving but the book has not bridged onto its snapshot"
        );
    }

    state.book.on_diff(
        crate::orderbook::DepthDiff {
            first_update_id: message.first_update_id,
            final_update_id: message.final_update_id,
            bids,
            asks,
        },
        ts,
    );

    let due =
        state.last_book_publish.elapsed() >= Duration::from_millis(config.orderbook_publish_ms);
    if due {
        if let Some(book) = state.book.book() {
            if state.book.is_synced() {
                if !state.book_announced {
                    state.book_announced = true;
                    info!(symbol, diffs = state.depth_diffs, "order book synced");
                }
                buses
                    .bus(&symbol)
                    .publish_orderbook(book.snapshot(config.depth_levels));
            }
        }
        state.last_book_publish = Instant::now();
    }

    Ok(())
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
    use analytics_core::Timeframe;

    /// A collector that cannot hand out a candle stream cannot have its candles
    /// persisted, however well it aggregates them.
    ///
    /// This is the row-12 gap in miniature. The collector has always built all
    /// six resolutions and published the closed ones to the bus, and
    /// `bus.rs`'s own test proves the bus delivers them -- but the bus is
    /// *internal* to `market-data`, so that test passed for as long as
    /// `xtask collect` wrote zero candles. What was missing was a way out of
    /// the crate, and only the collector can offer one.
    ///
    /// Constructing without `connect()` keeps this offline: it exercises the
    /// subscription wiring, not the socket.
    #[test]
    fn the_collector_hands_out_a_reachable_candle_stream() {
        let registry = Arc::new(MarketBusRegistry::new());
        let collector = BinanceCollector::with_defaults(Arc::clone(&registry));

        let mut rx = collector
            .candle_stream("btcusdt")
            .expect("a Binance collector aggregates candles and must expose them");

        // Lower case in, upper case out: the caller passes whatever the user
        // typed and the bus keys on the canonical symbol.
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
    ///
    /// Worth pinning because the fix for row 12 was a *third* subscription on
    /// an existing collector: if `candle_stream` had returned the trade or
    /// order-book channel, the pump would have been wired to the wrong bus and
    /// the failure would look exactly like the bug it replaced.
    #[test]
    fn the_candle_stream_is_not_the_trade_or_book_stream() {
        let registry = Arc::new(MarketBusRegistry::new());
        let collector = BinanceCollector::with_defaults(Arc::clone(&registry));

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
}
