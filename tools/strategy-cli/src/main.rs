//! Strategy CLI (`docs/06-STRATEGY-DSL.md`, `docs/07-BACKTESTING-ENGINE.md`).
//!
//! ```text
//! strategy-cli validate --strategy strategies/liquidity-sweep.yaml
//!
//! strategy-cli backtest run \
//!   --strategy strategies/liquidity-sweep.yaml \
//!   --symbol BTCUSDT --from 2024-01-01 --to 2024-07-01 \
//!   --report-out reports/liquidity-sweep-2024h1.json
//! ```
//!
//! ## Loading a multi-timeframe document
//!
//! The database stores candles per resolution, but a backfill normally only
//! populates one. For every timeframe the document declares, this CLI loads
//! that resolution if it exists and otherwise **resamples** the finer source
//! series (1m by default) up to it. Aggregating 1m candles built from the trade
//! stream is exact -- volume and the buy/sell split are sums, so delta and CVD
//! stay consistent -- which is why this is safe rather than a shortcut.
//!
//! The resampled series is aligned to the target's bucket boundaries, so a
//! 4h view begins at a real 4h boundary rather than wherever `--from` landed.

use std::collections::BTreeMap;
use std::path::Path;

use analytics_core::{resample, Candle, Timeframe};
use anyhow::{bail, Context, Result};
use backtester::replay::{run_backtest, ReplayConfig, ReplayInput};
use clap::{Parser, Subcommand};
use db::repositories::load_candles;
use db::Database;
use strategy_dsl::ValidatedStrategy;
use strategy_runtime::{RuntimeConfig, StrategyEngine};

#[derive(Parser)]
#[command(
    name = "strategy-cli",
    about = "validate and backtest strategy documents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a backtest for a strategy document.
    Backtest {
        #[command(subcommand)]
        action: BacktestAction,
    },
    /// Parse and validate a strategy document without executing it.
    Validate {
        /// Path to the YAML/JSON strategy document.
        #[arg(long)]
        strategy: String,
    },
}

#[derive(Subcommand)]
enum BacktestAction {
    /// Replay the strategy over a historical window.
    Run {
        /// Path to the YAML/JSON strategy document.
        #[arg(long)]
        strategy: String,
        /// Symbol, e.g. BTCUSDT.
        #[arg(long)]
        symbol: String,
        /// Start date (YYYY-MM-DD), inclusive.
        #[arg(long)]
        from: String,
        /// End date (YYYY-MM-DD), inclusive.
        #[arg(long)]
        to: String,
        /// Where to write the JSON performance report.
        #[arg(long)]
        report_out: Option<String>,
        /// Resolution to resample from when a declared timeframe has no candles
        /// of its own.
        #[arg(long, default_value = "1m")]
        source_timeframe: String,
        /// Slippage applied to market orders, in basis points.
        #[arg(long, default_value_t = 2.0)]
        slippage_bps: f64,
        /// Print every trade after the summary.
        #[arg(long, default_value_t = false)]
        print_trades: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // Credentials live in `.env` only, and `.env` is gitignored.
    if dotenvy::dotenv().is_ok() {
        tracing::debug!("loaded .env");
    }

    let cli = Cli::parse();

    match cli.command {
        Command::Validate { strategy } => run_validate(&strategy),
        Command::Backtest { action } => match action {
            BacktestAction::Run {
                strategy,
                symbol,
                from,
                to,
                report_out,
                source_timeframe,
                slippage_bps,
                print_trades,
            } => {
                run_backtest_command(
                    &strategy,
                    &symbol,
                    &from,
                    &to,
                    report_out.as_deref(),
                    &source_timeframe,
                    slippage_bps,
                    print_trades,
                )
                .await
            }
        },
    }
}

/// Parse and validate, reporting every field-level problem.
fn run_validate(path: &str) -> Result<()> {
    let source = read_document(path)?;

    match strategy_dsl::parse_and_validate(&source) {
        Ok(validated) => {
            let document = validated.document();
            println!("valid: {}", document.name);
            println!("  version:   {}", document.version);
            println!("  kind:      {}", document.kind);
            println!("  market:    {}", document.market);
            println!("  direction: {:?}", document.direction());
            if let Some((name, timeframe)) = document.decision_timeframe() {
                println!("  decides on: {name} ({timeframe})");
            }
            println!("  timeframes:");
            for (name, timeframe) in &document.timeframes {
                println!("    {name}: {timeframe}");
            }
            println!("  conditions: {}", document.all_conditions().len());
            Ok(())
        }
        Err(error) => {
            // The agent's retry loop depends on this being specific, so print it
            // verbatim rather than wrapping it in something friendlier.
            eprintln!("invalid: {error}");
            bail!("strategy document failed validation")
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_backtest_command(
    strategy_path: &str,
    symbol: &str,
    from: &str,
    to: &str,
    report_out: Option<&str>,
    source_timeframe: &str,
    slippage_bps: f64,
    print_trades: bool,
) -> Result<()> {
    let source = read_document(strategy_path)?;
    let validated = strategy_dsl::parse_and_validate(&source)
        .with_context(|| format!("`{strategy_path}` is not a valid strategy document"))?;

    let from_ns = parse_date_ns(from)?;
    // `--to` is inclusive, so the exclusive upper bound is the next midnight.
    let to_ns = parse_date_ns(to)? + 86_400 * 1_000_000_000;
    if to_ns <= from_ns {
        bail!("`--to` must not be before `--from`");
    }

    let source_timeframe: Timeframe = source_timeframe
        .parse()
        .map_err(|e| anyhow::anyhow!("unknown source timeframe `{source_timeframe}`: {e}"))?;

    let database = Database::from_env()
        .await
        .context("could not connect to postgres")?;

    let series = load_all_timeframes(
        &database,
        symbol,
        &validated,
        from_ns,
        to_ns,
        source_timeframe,
    )
    .await?;

    for (name, candles) in &series {
        let timeframe = validated.document().timeframes.get(name).copied();
        let span = match (candles.first(), candles.last()) {
            (Some(first), Some(last)) => format!("{} .. {}", first.open_time, last.open_time),
            _ => "empty".to_string(),
        };
        println!(
            "loaded {name} ({:?}): {} candles, {span}",
            timeframe,
            candles.len()
        );
    }

    let input = ReplayInput {
        timeframes: validated.document().timeframes.clone(),
        candles: series,
    };

    let config = ReplayConfig {
        symbol: symbol.to_string(),
        from: from_ns,
        to: to_ns - 1,
        simulator: backtester::SimulatorConfig {
            slippage_bps,
            ..backtester::SimulatorConfig::default()
        },
        ..ReplayConfig::default()
    };

    let mut engine = StrategyEngine::new(&validated, RuntimeConfig::default())
        .context("the document parsed and validated but cannot be executed")?;

    let report = run_backtest(&mut engine, &input, &config).context("backtest failed")?;

    println!();
    println!("{}", report.summary());
    if report.skipped_signals_count > 0 {
        println!(
            "{} setup(s) fired but produced no trade:",
            report.skipped_signals_count
        );
        for reason in report.skipped_signals.iter().take(5) {
            println!("  - {reason}");
        }
        if report.skipped_signals_count > 5 {
            println!("  ... and {} more", report.skipped_signals_count - 5);
        }
    }

    if print_trades {
        println!();
        println!(
            "{:>13} {:>6} {:>11} {:>11} {:>8} {:>12}",
            "entry (unix s)", "dir", "entry", "exit", "R", "trigger"
        );
        for trade in &report.trades {
            println!(
                "{:>13} {:>6} {:>11.2} {:>11.2} {:>+8.2} {:>12}",
                trade.entry_time / 1_000_000_000,
                format!("{:?}", trade.direction).to_lowercase(),
                trade.entry_price,
                trade.exit_price,
                trade.r_multiple,
                trade.exit_trigger,
            );
        }
    }

    if let Some(path) = report_out {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("could not create {}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(path, json).with_context(|| format!("could not write {path}"))?;
        println!();
        println!("report written to {path}");
    }

    Ok(())
}

/// Load one series per declared timeframe, resampling when necessary.
///
/// Two passes on purpose. The first tries every declared resolution directly, so
/// a document whose timeframes are all present in the database costs one query
/// each and nothing more. Only if some resolution is missing do we load the
/// source series -- once, however many timeframes end up built from it. Loading
/// it eagerly would drag tens of thousands of candles off a managed instance
/// even when every timeframe was already there.
///
/// The source series is read with a padded window so the first and last bucket of
/// each aggregated series are whole. When the source resolution is *also*
/// declared, that means its candles are read twice: once as the declared view,
/// once padded for aggregation. Reusing a single read would mean giving up
/// either the padding or the preference for native-resolution data; at the
/// current data volumes the extra read is the cheaper mistake, and it is worth
/// revisiting only if a backtest's dominant cost becomes the load rather than the
/// replay.
async fn load_all_timeframes(
    database: &Database,
    symbol: &str,
    validated: &ValidatedStrategy,
    from_ns: i64,
    to_ns: i64,
    source_timeframe: Timeframe,
) -> Result<BTreeMap<String, Vec<Candle>>> {
    let pool = database.pool();
    let mut out = BTreeMap::new();
    let mut missing: Vec<(String, Timeframe)> = Vec::new();

    for (name, timeframe) in &validated.document().timeframes {
        let direct = load_candles(pool, symbol, *timeframe, from_ns, to_ns)
            .await
            .with_context(|| format!("could not load {timeframe} candles for {symbol}"))?;

        if direct.is_empty() {
            missing.push((name.clone(), *timeframe));
        } else {
            out.insert(name.clone(), direct);
        }
    }

    if missing.is_empty() {
        return Ok(out);
    }

    // Pad the source window by the coarsest declared resolution so that the
    // first and last bucket of every resampled series are whole.
    let pad = validated
        .document()
        .timeframes
        .values()
        .copied()
        .max()
        .map_or(0, Timeframe::nanos);

    let source = load_candles(pool, symbol, source_timeframe, from_ns - pad, to_ns + pad)
        .await
        .with_context(|| format!("could not load {source_timeframe} candles for {symbol}"))?;

    for (name, timeframe) in missing {
        // Only a *strictly finer* source is a problem. An equal one is a no-op
        // copy that `resample` handles, and the trim below then reduces it to
        // exactly what the direct query would have returned.
        if timeframe < source_timeframe {
            bail!(
                "no {timeframe} candles for {symbol} in this window, and {timeframe} cannot be \
                 built by aggregating {source_timeframe} candles"
            );
        }

        if source.is_empty() {
            bail!("no {source_timeframe} candles for {symbol} in this window either");
        }

        let series: Vec<Candle> = resample(&source, timeframe)
            .into_iter()
            // Trim the padding back off, keeping only buckets inside the window.
            .filter(|candle| candle.open_time >= from_ns && candle.open_time < to_ns)
            .collect();

        if series.is_empty() {
            bail!("{timeframe} aggregated from {source_timeframe} produced no candles");
        }
        out.insert(name, series);
    }

    Ok(out)
}

/// Read a document from disk.
fn read_document(path: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("could not read {path}"))
}

/// `YYYY-MM-DD` to unix nanoseconds at midnight UTC.
fn parse_date_ns(value: &str) -> Result<i64> {
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("`{value}` is not a YYYY-MM-DD date"))?;
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .context("could not build midnight")?;
    midnight
        .and_utc()
        .timestamp_nanos_opt()
        .context("date is outside the representable range")
}
