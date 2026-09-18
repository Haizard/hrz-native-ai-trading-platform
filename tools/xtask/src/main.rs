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

use analytics_core::Timeframe;
use clap::{Parser, Subcommand, ValueEnum};
use db::repositories;
use db::Database;
use market_data::backfill::{BackfillClient, BackfillSource};
use market_data::bus::MarketBusRegistry;
use market_data::{BinanceCollector, ExchangeCollector};
use tokio::time::interval;

// Batch sizes and the flush interval used to live here. They moved to
// `db::pump` when the gateway needed the same loop: two copies of a batching
// writer is the `docs/19` row 20 defect, and the tuning is not interesting
// enough to justify keeping two sets of it.

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
    /// Backfill raw aggregate trades, for the footprint chart.
    ///
    /// `backfill --source trades` builds *candles* from trades and discards the
    /// trades themselves, which is why `trades` is empty after a normal backfill
    /// and why the footprint chart has nothing to draw. This persists them.
    BackfillTrades {
        /// Symbol, e.g. BTCUSDT.
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Window start, `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`.
        #[arg(long)]
        from: String,
        /// Window end, `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`.
        #[arg(long)]
        to: String,
        /// Fetch and report, but write nothing.
        #[arg(long, default_value_t = false)]
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
        Command::BackfillTrades {
            symbol,
            from,
            to,
            dry_run,
        } => {
            backfill_trades(&symbol, &from, &to, dry_run).await?;
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
    // The collector aggregates every resolution the platform uses, but until
    // this pump existed nothing outside `market-data` ever read them: a
    // collector run persisted trades and books and **zero candles**, so the
    // candle table could only be filled by `backfill`. Phase 1's exit criterion
    // is about a collector run producing that table.
    let mut candles_rx = collector
        .candle_stream(&symbol)
        .ok_or_else(|| anyhow::anyhow!("no candle stream for {symbol}"))?;

    collector.subscribe_trades(&symbol).await?;
    collector.subscribe_order_book(&symbol).await?;

    let health = collector.health_counters();

    let trade_db = db.clone();
    let trade_handle = tokio::spawn(async move {
        db::pump::pump_trades(&mut trades_rx, trade_db, None).await;
    });

    let book_db = db.clone();
    let book_handle = tokio::spawn(async move {
        db::pump::pump_books(&mut books_rx, book_db, None).await;
    });

    // Counted so the status line can show candles landing. Without it a soak
    // proves only that the process stayed up: the thing this pump exists to
    // fix is "no candles were written", and that is invisible from the outside
    // until you query the table afterwards.
    let candles = Arc::new(db::pump::Report::new());

    let candle_db = db.clone();
    let candle_report = Arc::clone(&candles);
    let candle_handle = tokio::spawn(async move {
        db::pump::pump_candles(&mut candles_rx, candle_db, Some(&candle_report)).await;
    });

    let mut ticker = interval(Duration::from_secs(30));
    let status_candles = Arc::clone(&candles);
    let status_handle = tokio::spawn(async move {
        loop {
            ticker.tick().await;
            println!(
                "health: connected={} messages={} gaps={} reconnects={} candles_written={}",
                health.is_connected(),
                health.messages(),
                health.gaps(),
                health.reconnects(),
                status_candles.written()
            );
        }
    });

    println!("collecting {symbol}; press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;

    println!("shutting down");
    status_handle.abort();
    trade_handle.abort();
    book_handle.abort();
    candle_handle.abort();

    println!("{} candles written this run", candles.written());

    Ok(())
}

/// Persist raw aggregate trades for a window.
///
/// ## Why this is separate from `backfill`
///
/// `backfill --source trades` fetches aggTrades and *aggregates them into
/// candles*, then discards the trades. That is the right thing for a chart and
/// the wrong thing for a footprint, which needs the trades themselves -- so
/// `trades` stayed empty and `/footprint` had nothing to show.
///
/// ## The window is capped, and that is not an oversight
///
/// Binance returns ~1000 trades per request, so a day of BTCUSDT is thousands of
/// round trips. `MAX_TRADE_BACKFILL_HOURS` refuses anything longer rather than
/// appearing to hang. Backfill the hours you intend to look at.
async fn backfill_trades(symbol: &str, from: &str, to: &str, dry_run: bool) -> anyhow::Result<()> {
    let from_ns = market_data::backfill::parse_datetime_ns(from)?;
    let to_ns = market_data::backfill::parse_datetime_ns(to)?;
    if to_ns <= from_ns {
        anyhow::bail!("`--to` ({to}) must be after `--from` ({from})");
    }

    let hours = (to_ns - from_ns) / 3_600_000_000_000;
    println!("backfilling {symbol} trades from {from} to {to} ({hours}h)");

    let client = BackfillClient::binance();
    let trades = client.fetch_agg_trades(symbol, from_ns, to_ns).await?;

    if trades.is_empty() {
        anyhow::bail!("no trades came back for {symbol} in that window");
    }
    let buys = trades.iter().filter(|t| !t.is_buyer_maker).count();
    println!(
        "fetched {} trades ({} buy-aggressed, {} sell-aggressed)",
        trades.len(),
        buys,
        trades.len() - buys
    );
    if let (Some(first), Some(last)) = (trades.first(), trades.last()) {
        println!("  range: {} .. {}", first.timestamp, last.timestamp);
    }

    if dry_run {
        println!("dry run -- nothing written");
        return Ok(());
    }

    let db = Database::from_env().await?;
    db.migrate().await?;
    repositories::insert_trades(db.pool(), &trades).await?;
    println!("inserted {} trades (idempotent upsert)", trades.len());
    println!("the footprint chart will now have data for this window");
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
