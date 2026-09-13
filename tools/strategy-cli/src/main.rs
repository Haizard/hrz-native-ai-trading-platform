//! Strategy CLI (`docs/07-BACKTESTING-ENGINE.md`).
//!
//! Target interface, implemented in Phase 3:
//!
//! ```text
//! strategy-cli backtest run \
//!   --strategy strategies/liquidity-sweep.yaml \
//!   --symbol BTCUSDT --from 2024-01-01 --to 2024-07-01 \
//!   --report-out reports/liquidity-sweep-2024h1.json
//! ```
//!
//! Phase 0: argument surface only, so the interface is fixed before the
//! implementation exists.

use clap::{Parser, Subcommand};

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
        /// Start date (YYYY-MM-DD).
        #[arg(long)]
        from: String,
        /// End date (YYYY-MM-DD).
        #[arg(long)]
        to: String,
        /// Where to write the JSON performance report.
        #[arg(long)]
        report_out: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();

    match cli.command {
        Command::Backtest { action } => match action {
            BacktestAction::Run {
                strategy,
                symbol,
                from,
                to,
                report_out,
            } => {
                println!("backtest run (Phase 3):");
                println!("  strategy:   {strategy}");
                println!("  symbol:     {symbol}");
                println!("  window:     {from} .. {to}");
                if let Some(out) = report_out {
                    println!("  report-out: {out}");
                }
                anyhow::bail!("backtesting is implemented in Phase 3");
            }
        },
        Command::Validate { strategy } => {
            println!("validate (Phase 3): {strategy}");
            anyhow::bail!("strategy validation is implemented in Phase 3");
        }
    }
}
