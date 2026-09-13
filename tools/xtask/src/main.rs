//! Repo automation. Run via the cargo alias: `cargo xtask <command>`.
//!
//! Commands:
//!
//! * `migrate` -- apply all pending migrations to `DATABASE_URL`.
//! * `db-status` -- report connectivity and which migrations are applied.

use clap::{Parser, Subcommand};
use db::Database;

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
    }

    Ok(())
}
