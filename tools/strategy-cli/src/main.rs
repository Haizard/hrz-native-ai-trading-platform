//! Strategy CLI (`docs/06-STRATEGY-DSL.md`, `docs/07-BACKTESTING-ENGINE.md`).
//!
//! ```text
//! strategy-cli validate --strategy strategies/liquidity-sweep.yaml
//!
//! strategy-cli backtest run \
//!   --strategy strategies/liquidity-sweep.yaml \
//!   --symbol BTCUSDT --from 2024-01-01 --to 2024-07-01 \
//!   --report-out reports/liquidity-sweep-2024h1.json
//!
//! strategy-cli verify --symbol BTCUSDT --trades 25
//! ```
//!
//! ## `verify` checks stored trades against the candle table
//!
//! Phase 3's exit criterion asks for "manually-verified spot checks" and, until
//! `verify` existed, the repository had none — the phrase appears in the
//! roadmap and nowhere else. The golden-file test cannot stand in for it: its
//! fixture was generated from this implementation, so it detects a *changed*
//! backtester and never a wrong one.
//!
//! `verify` asks the other question: given the trades a stored run recorded,
//! does the candle table actually support them? That table is produced by the
//! collector and the backfill, not by the backtester, so it is external to the
//! thing under test. See `verify.rs` for exactly which claims are checked and
//! which are not.
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
//!
//! ## A stored series is only used if it covers the window
//!
//! "Non-empty" is not the same as "usable". A handful of stale 4h candles left
//! in the database by an earlier backfill covers a couple of days of a
//! six-month window, leaves the 4h view cold everywhere else, and turns every
//! condition written against it permanently false -- a backtest that reports
//! zero trades and looks perfectly valid. So a stored series must span at
//! least [`MIN_COVERAGE`] of the requested window, or it is treated as absent
//! and resampled.

use std::path::Path;

mod verify;

use analytics_core::Timeframe;
use anyhow::{bail, Context, Result};
use backtester::replay::{run_backtest, ReplayConfig, ReplayInput};
use clap::{Parser, Subcommand};
use db::Database;
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
    /// Re-check a backtest's trades against the candle table.
    ///
    /// Exits non-zero when any checked trade contradicts itself or the market
    /// data, so it is usable as a gate rather than only as a report.
    Verify {
        /// A JSON report written by `backtest run --report-out`.
        ///
        /// The run Phase 3's exit criterion refers to lives in `reports/`, not in
        /// the `backtests` table, so reading the file is the path that actually
        /// reaches it.
        #[arg(long, conflicts_with = "symbol")]
        report: Option<String>,
        /// Symbol whose newest stored run should be checked.
        #[arg(long)]
        symbol: Option<String>,
        /// How many trades to check, spread across the run.
        #[arg(long, default_value_t = 25)]
        trades: usize,
        /// Override the resolution to check against. Defaults to the run's own
        /// decision timeframe, discovered from the candle table.
        #[arg(long)]
        timeframe: Option<String>,
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
        Command::Verify {
            report,
            symbol,
            trades,
            timeframe,
        } => {
            let clean = verify::run(
                report.as_deref(),
                symbol.as_deref(),
                trades,
                timeframe.as_deref(),
            )
            .await?;
            // A failed check is a real answer, not an operational error, so it
            // leaves through the exit code rather than an `Err`. A gate that
            // printed the failure and exited zero would be a gate nobody could
            // wire into CI.
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
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

    let series = db::loading::load_timeframe_series(
        database.pool(),
        symbol,
        &validated.document().timeframes,
        from_ns,
        to_ns,
        source_timeframe,
    )
    .await?;
    db::loading::warn_about_short_series(&series, &validated.document().timeframes, from_ns, to_ns);

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
