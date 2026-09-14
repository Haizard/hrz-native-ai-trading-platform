//! Repo automation. Run via the cargo alias: `cargo xtask <command>`.
//!
//! Commands:
//!
//! * `migrate` -- apply all pending migrations to `DATABASE_URL`.
//! * `db-status` -- report connectivity and applied migrations.
//! * `backfill` -- pull historical candles from the exchange REST API.
//! * `collect` -- run the live Binance collector against one symbol.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use analytics_core::{OrderBookSnapshot, Timeframe, Trade};
use clap::{Parser, Subcommand, ValueEnum};
use db::repositories;
use db::Database;
use market_data::backfill::{BackfillClient, BackfillSource};
use market_data::bus::MarketBusRegistry;
use market_data::{BinanceCollector, ExchangeCollector};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::interval;

/// Trades buffered before a forced insert.
const TRADE_BATCH: usize = 1_000;
/// Order-book snapshots buffered before a forced insert.
const BOOK_BATCH: usize = 100;
/// How often buffered rows are flushed even if the batch isn't full.
const FLUSH_SECS: u64 = 5;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "repository automation for ai-trading-platform"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply all pending database migrations.
    Migrate,
    /// Print connectivity and applied-migration status.
    DbStatus,
    /// Backfill historical candles from the exchange REST API.
    Backfill {
        /// Symbol, e.g. BTCUSDT.
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Candle resolution to fetch.
        #[arg(long, default_value = "1m")]
        timeframe: String,
        /// Start date, YYYY-MM-DD.
        #[arg(long)]
        from: String,
        /// End date, YYYY-MM-DD.
        #[arg(long)]
        to: String,
        /// Where the candles come from.
        #[arg(long, value_enum, default_value_t = SourceArg::Klines)]
        source: SourceArg,
        /// Fetch and report, but don't write to the database.
        #[arg(long)]
        dry_run: bool,
    },
    /// Build the WASM chart engine and place it beside the shell.
    BuildFrontend,
    /// Run the live collector until interrupted.
    Collect {
        /// Symbol to collect.
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Don't touch the database; just stream and count.
        #[arg(long)]
        no_persist: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// REST klines -- fast, for multi-month windows.
    Klines,
    /// REST aggregate trades -- exact, capped at 24h.
    Trades,
}

impl From<SourceArg> for BackfillSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Klines => Self::Klines,
            SourceArg::Trades => Self::Trades,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if dotenvy::dotenv().is_ok() {
        eprintln!("loaded .env");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Migrate => {
            let db = Database::from_env().await?;
            db.migrate().await?;
            println!("migrations applied");
        }
        Command::DbStatus => {
            let db = Database::from_env().await?;
            db.migrate().await?;
            println!("connected: {}", db.config().redacted_url());
            println!("database: up");
        }
        Command::Backfill {
            symbol,
            timeframe,
            from,
            to,
            source,
            dry_run,
        } => {
            backfill(&symbol, &timeframe, &from, &to, source.into(), dry_run).await?;
        }
        Command::Collect { symbol, no_persist } => {
            collect(&symbol, !no_persist).await?;
        }
        Command::BuildFrontend => {
            build_frontend()?;
        }
    }

    Ok(())
}

async fn backfill(
    symbol: &str,
    timeframe: &str,
    from: &str,
    to: &str,
    source: BackfillSource,
    dry_run: bool,
) -> anyhow::Result<()> {
    let timeframe = Timeframe::from_str(timeframe)?;
    let from_ns = market_data::backfill::parse_date_ns(from)?;
    let to_ns = market_data::backfill::parse_date_ns(to)?;

    if to_ns <= from_ns {
        anyhow::bail!("`--to` ({to}) must be after `--from` ({from})");
    }

    let client = BackfillClient::binance();
    println!("backfilling {symbol} {timeframe} from {from} to {to} (source: {source:?})");

    let candles = client
        .backfill_candles(symbol, timeframe, from_ns, to_ns, source)
        .await?;

    println!("fetched {} candles", candles.len());
    if let (Some(first), Some(last)) = (candles.first(), candles.last()) {
        println!("  range: {} .. {}", first.open_time, last.open_time);
    }

    if dry_run {
        println!("dry run -- nothing written");
        return Ok(());
    }

    let db = Database::from_env().await?;
    db.migrate().await?;
    repositories::insert_candles(db.pool(), &candles).await?;
    println!("inserted {} candles (upsert)", candles.len());

    Ok(())
}

async fn collect(symbol: &str, persist: bool) -> anyhow::Result<()> {
    let symbol = symbol.to_uppercase();
    let registry = Arc::new(MarketBusRegistry::new());

    let db: Option<Arc<Database>> = if persist {
        let db = Database::from_env().await?;
        db.migrate().await?;
        Some(Arc::new(db))
    } else {
        println!("--no-persist: streaming without writing to the database");
        None
    };

    let mut collector = BinanceCollector::with_defaults(registry.clone());
    collector.connect().await?;

    // Subscribe to the buses BEFORE asking the exchange for data, otherwise the
    // first messages can arrive with no receiver attached.
    let mut trades_rx = collector
        .trade_stream(&symbol)
        .ok_or_else(|| anyhow::anyhow!("no trade stream for {symbol}"))?;
    let mut books_rx = collector
        .order_book_stream(&symbol)
        .ok_or_else(|| anyhow::anyhow!("no order book stream for {symbol}"))?;

    collector.subscribe_trades(&symbol).await?;
    collector.subscribe_order_book(&symbol).await?;

    let health = collector.health_counters();

    let trade_db = db.clone();
    let trade_handle = tokio::spawn(async move {
        pump_trades(&mut trades_rx, trade_db).await;
    });

    let book_db = db.clone();
    let book_handle = tokio::spawn(async move {
        pump_books(&mut books_rx, book_db).await;
    });

    let mut ticker = interval(Duration::from_secs(30));
    let status_handle = tokio::spawn(async move {
        loop {
            ticker.tick().await;
            println!(
                "health: connected={} messages={} gaps={} reconnects={}",
                health.is_connected(),
                health.messages(),
                health.gaps(),
                health.reconnects()
            );
        }
    });

    println!("collecting {symbol}; press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;

    println!("shutting down");
    status_handle.abort();
    trade_handle.abort();
    book_handle.abort();

    Ok(())
}

/// Build the chart engine for the browser and put it where the shell expects.
///
/// `docs/14`: the chart is a Rust crate compiled to `wasm32-unknown-unknown`,
/// and the shell loads it from `frontend/app`. This is the step between the two.
///
/// Deliberately a `cargo` invocation rather than a build script: the wasm is an
/// *artifact to ship*, not something every `cargo build` should regenerate. The
/// gateway serves whatever `.wasm` is checked in beside the shell, so a
/// deployment that never runs this command still serves a working chart -- just
/// not a rebuilt one.
fn build_frontend() -> anyhow::Result<()> {
    use anyhow::{bail, Context};

    const ARTIFACT: &str = "target/wasm32-unknown-unknown/release/chart_engine.wasm";
    const DESTINATION: &str = "frontend/app/chart_engine.wasm";

    println!("building chart-engine for wasm32-unknown-unknown");
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "chart-engine",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()
        .context("could not run cargo; is it on PATH?")?;

    if !status.success() {
        bail!("the wasm build failed");
    }

    std::fs::create_dir_all("frontend/app").context("could not create frontend/app")?;
    std::fs::copy(ARTIFACT, DESTINATION)
        .with_context(|| format!("could not copy {ARTIFACT} to {DESTINATION}"))?;

    let bytes = std::fs::metadata(DESTINATION).map(|m| m.len()).unwrap_or(0);
    println!("wrote {DESTINATION} ({bytes} bytes)");
    println!("the gateway serves it at /chart_engine.wasm");
    Ok(())
}

/// Consume trades, buffering and flushing to Postgres.
async fn pump_trades(rx: &mut tokio::sync::broadcast::Receiver<Trade>, db: Option<Arc<Database>>) {
    let mut buffer: Vec<Trade> = Vec::new();
    let mut flush = interval(Duration::from_secs(FLUSH_SECS));
    let mut total: u64 = 0;

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(trade) => {
                    buffer.push(trade);
                    total += 1;
                    if buffer.len() >= TRADE_BATCH {
                        flush_trades(&db, &mut buffer).await;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("trade subscriber lagged by {n} messages");
                }
                Err(RecvError::Closed) => break,
            },
            _ = flush.tick() => flush_trades(&db, &mut buffer).await,
        }
    }

    flush_trades(&db, &mut buffer).await;
    println!("trade pump stopped after {total} trades");
}

/// Consume order-book snapshots, buffering and flushing to Postgres.
async fn pump_books(
    rx: &mut tokio::sync::broadcast::Receiver<OrderBookSnapshot>,
    db: Option<Arc<Database>>,
) {
    let mut buffer: Vec<OrderBookSnapshot> = Vec::new();
    let mut flush = interval(Duration::from_secs(FLUSH_SECS));
    let mut total: u64 = 0;

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(snapshot) => {
                    buffer.push(snapshot);
                    total += 1;
                    if buffer.len() >= BOOK_BATCH {
                        flush_books(&db, &mut buffer).await;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("order book subscriber lagged by {n} messages");
                }
                Err(RecvError::Closed) => break,
            },
            _ = flush.tick() => flush_books(&db, &mut buffer).await,
        }
    }

    flush_books(&db, &mut buffer).await;
    println!("order book pump stopped after {total} snapshots");
}

async fn flush_trades(db: &Option<Arc<Database>>, buffer: &mut Vec<Trade>) {
    if buffer.is_empty() {
        return;
    }
    let batch = std::mem::take(buffer);
    if let Some(db) = db {
        if let Err(e) = repositories::insert_trades(db.pool(), &batch).await {
            tracing::error!("failed to insert {} trades: {e}", batch.len());
        }
    }
}

async fn flush_books(db: &Option<Arc<Database>>, buffer: &mut Vec<OrderBookSnapshot>) {
    if buffer.is_empty() {
        return;
    }
    let batch = std::mem::take(buffer);
    if let Some(db) = db {
        if let Err(e) = repositories::insert_orderbook_snapshots(db.pool(), &batch).await {
            tracing::error!("failed to insert {} order book snapshots: {e}", batch.len());
        }
    }
}
