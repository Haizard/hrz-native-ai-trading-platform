# 04 — Market Data Engine

## Purpose
Ingest, normalize, and persist real-time and historical market data (trades, order
book, OHLCV) from one or more exchanges, and make it available to the rest of the
platform via internal pub/sub and database queries.

## Scope (Phase 1)
- Binance first (per source research); design the collector trait so a second exchange
  is a new implementation, not a rewrite.
- Trade stream, order-book diff stream, and candle (OHLCV) aggregation at multiple
  resolutions (1m, 5m, 15m, 1h, 4h, 1d).
- Historical backfill for a symbol/date range.

## Core types (owned by `analytics-core::types`, used here)
```rust
pub struct Candle {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub open_time: i64,   // unix nanos, UTC
    pub open: f64, pub high: f64, pub low: f64, pub close: f64,
    pub volume: f64,
    pub buy_volume: f64,
    pub sell_volume: f64,
}

pub struct Trade {
    pub symbol: String,
    pub trade_id: u64,
    pub price: f64,
    pub quantity: f64,
    pub is_buyer_maker: bool,
    pub timestamp: i64,
}

pub struct OrderBookLevel { pub price: f64, pub quantity: f64 }

pub struct OrderBookSnapshot {
    pub symbol: String,
    pub timestamp: i64,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
}
```

## Exchange collector trait
```rust
#[async_trait]
pub trait ExchangeCollector {
    async fn connect(&mut self) -> Result<(), MarketDataError>;
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), MarketDataError>;
    async fn subscribe_order_book(&mut self, symbol: &str) -> Result<(), MarketDataError>;
    fn trade_stream(&self, symbol: &str) -> Option<BroadcastReceiver<Trade>>;
    fn order_book_stream(&self, symbol: &str) -> Option<BroadcastReceiver<OrderBookSnapshot>>;
    /// Closed candles for every resolution the collector aggregates.
    ///
    /// This belongs on the trait rather than being read off the bus directly
    /// because the aggregator lives *inside* the collector: nothing outside
    /// `market-data` can reach the candle channel any other way. It was absent
    /// until 2026-09-17, and its absence is why a collector run built candles
    /// and persisted none of them — see `docs/19` row 12.
    fn candle_stream(&self, symbol: &str) -> Option<BroadcastReceiver<Candle>>;
}
```
Implement `BinanceCollector` against this trait first.

## Candle aggregation
- Build candles from the trade stream (not from exchange-provided klines) so
  `buy_volume`/`sell_volume` split is derived consistently with the rest of the
  analytics (needed for delta/CVD downstream).
- Maintain in-memory rolling aggregation per (symbol, timeframe); flush closed candles
  to Postgres; emit closed candles on the internal pub/sub.

  **The flush is `xtask collect`, and it did not exist until 2026-09-17.** The
  aggregator was always correct — it built all six resolutions and published them —
  but `collect()` spawned only `pump_trades` and `pump_books`, so the only writer of
  the `candles` table was `xtask backfill`. A 24h collector run added **zero** rows.
  `collect` now drains `candle_stream` through a third pump and upserts in batches of
  100 with `ON CONFLICT DO NOTHING`, counting **successes** (a counter that counts
  attempts reports a healthy run against an unreachable database). `docs/19` row 12
  records the closure and `reports/phase1-verification.md` the observation.

## Order book handling
- Maintain an in-memory order book per symbol from the diff stream; periodically persist
  snapshots (not every diff) for historical footprint/DOM reconstruction — snapshot
  frequency is a config value, tune per storage budget.
- Expose current book state via a query API for the AI agent's `get_orderbook()` tool.

## Backfill tool
- CLI: `market-data backfill --symbol BTCUSDT --from 2024-01-01 --to 2024-06-01 --timeframe 1m`
- Pull historical klines/trades from the exchange REST API, run them through the same
  normalization path as live data so historical and live data are bit-for-bit
  consistent in shape.

## Persistence
- See `docs/13-DATABASE-SCHEMA.md` for exact tables. Use `sqlx` with compile-time
  checked queries. Batch-insert trades/candles; never insert row-by-row in the hot path.

## Internal pub/sub
- A simple in-process broadcast (tokio `broadcast` channel) per (symbol, stream-type) is
  sufficient for Phase 1. Do not introduce Kafka/NATS until a concrete cross-process
  scaling need is identified — avoid premature infrastructure.

## Reliability requirements
- Automatic reconnect with exponential backoff on WebSocket disconnect.
- Gap detection: if a sequence number or trade ID gap is detected, trigger a REST
  resync for the affected window and log it.
- Health endpoint reporting: connection status, last message timestamp per stream,
  detected-gap count.

## Done criteria
- 24h continuous run against BTCUSDT with zero unhandled panics.
- Backfilled and live-collected candles for the same historical window are identical.
- Health endpoint accurately reflects induced disconnects during a chaos test.
