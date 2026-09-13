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

use analytics_core::{OrderBookSnapshot, Trade};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

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
        let url = format!("{}/api/v3/depth", self.config.rest_url);
        let response = self
            .client
            .get(url)
            .query(&[
                ("symbol", symbol.to_uppercase()),
                ("limit", self.config.snapshot_limit.to_string()),
            ])
            .send()
            .await
            .map_err(|e| MarketDataError::Transport(format!("depth snapshot request failed: {e}")))?
            .error_for_status()
            .map_err(|e| MarketDataError::Transport(format!("depth snapshot HTTP error: {e}")))?
            .json::<DepthSnapshotResponse>()
            .await
            .map_err(|e| MarketDataError::Normalization(format!("depth snapshot decode: {e}")))?;

        Ok(response)
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

    fn health(&self) -> HealthStatus {
        self.health.snapshot(self.streams.clone())
    }
}

impl Drop for BinanceCollector {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Per-symbol state that must survive reconnects.
struct SymbolState {
    candles: MultiTimeframeCandleBuilder,
    book: OrderBookSynchronizer,
    gap_detector: TradeGapDetector,
    last_book_publish: Instant,
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

    loop {
        tokio::select! {
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
