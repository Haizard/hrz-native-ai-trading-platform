//! Paper-bot runner (`docs/11-BOT-TRADING-ENGINE.md`).
//!
//! ```text
//! # Replay a window through both paths and diff them.
//! paper-cli run --strategy strategies/liquidity-sweep-btcusdt-5m.yaml \
//!   --symbol BTCUSDT --from 2026-09-10 --to 2026-09-12
//!
//! # Run against the live feed.
//! paper-cli live --strategy strategies/liquidity-sweep-btcusdt-5m.yaml \
//!   --symbol BTCUSDT --minutes 2880
//! ```
//!
//! ## Why `run` exists
//!
//! The Phase 6 done criterion is that paper trades are "consistent with what a
//! manual replay of the same period through the backtester would produce".
//! `run` is how that is checked rather than asserted: it drives the paper bot
//! and the backtester over the same candles and diffs the trades, number by
//! number. If the two ever drift, this command says so on the bar where it
//! starts, instead of leaving someone to notice that a live result looks off.
//!
//! ## Why the feed order matters
//!
//! Both paths are fed the way the replay's visibility rule dictates: at each
//! decision close, every coarser candle that has already closed is delivered
//! first, then the decision candle. Feeding them in any other order would
//! produce a different context and a different trade -- which is exactly the
//! bug this command is here to catch.

use std::collections::BTreeMap;
use std::sync::Arc;

use analytics_core::types::{Candle, Timeframe};
use anyhow::{bail, Context, Result};
use backtester::replay::{replay, ReplayConfig, ReplayInput};
use clap::{Parser, Subcommand};
use db::Database;
use market_data::{Collector, MarketBusRegistry, MultiTimeframeCandleBuilder};
use strategy_dsl::ValidatedStrategy;
use strategy_runtime::{ExitTrigger, RuntimeConfig, SimulatorConfig, StrategyEngine};
use trading_engine::{BotSession, PaperBot, PaperConfig, RiskLimits};

#[derive(Parser)]
#[command(
    name = "paper-cli",
    about = "run a strategy as a paper bot and check it against a replay"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Replay a historical window through the paper bot and the backtester,
    /// then diff the two.
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
        /// Resolution to resample from when a declared timeframe has no
        /// candles of its own.
        #[arg(long, default_value = "1m")]
        source_timeframe: String,
        /// Per-trade risk cap, in percent of equity. Clamped to the platform
        /// ceiling of 5%.
        #[arg(long, default_value_t = 1.0)]
        max_risk_pct: f64,
        /// Daily loss budget, in R.
        #[arg(long, default_value_t = 3.0)]
        daily_loss_limit_r: f64,
        /// Weekly loss budget, in R.
        #[arg(long, default_value_t = 8.0)]
        weekly_loss_limit_r: f64,
        /// Write trades and decisions to Postgres. Off by default: a dry run
        /// should not leave rows behind.
        #[arg(long, default_value_t = false)]
        persist: bool,
        /// Owner of the bot row. Required with `--persist`.
        #[arg(long)]
        user_email: Option<String>,
        /// Where to write a JSON summary of the comparison.
        #[arg(long)]
        report_out: Option<String>,
    },
    /// Provision the owner a paper bot writes under, and print its id.
    ///
    /// A bot row references `users` and there is no signup flow until Phase 7,
    /// so this is a deliberate act rather than something a run does quietly on
    /// its own.
    Owner {
        /// Email to provision.
        #[arg(long)]
        email: String,
    },
    /// Show what a bot has done, from the tables it wrote.
    Status {
        /// The bot id, as printed when it started.
        #[arg(long)]
        bot: String,
        /// Print the newest decisions as well as the totals.
        #[arg(long, default_value_t = false)]
        decisions: bool,
    },
    /// Run against the live feed until stopped.
    Live {
        /// Path to the YAML/JSON strategy document.
        #[arg(long)]
        strategy: String,
        /// Symbol, e.g. BTCUSDT.
        #[arg(long)]
        symbol: String,
        /// Stop after this many minutes. Without it, run until Ctrl-C.
        #[arg(long)]
        minutes: Option<u64>,
        /// Per-trade risk cap, in percent of equity.
        #[arg(long, default_value_t = 1.0)]
        max_risk_pct: f64,
        /// Daily loss budget, in R.
        #[arg(long, default_value_t = 3.0)]
        daily_loss_limit_r: f64,
        /// Weekly loss budget, in R.
        #[arg(long, default_value_t = 8.0)]
        weekly_loss_limit_r: f64,
        /// Write trades and decisions to Postgres.
        #[arg(long, default_value_t = false)]
        persist: bool,
        /// Owner of the bot row. Required with `--persist`.
        #[arg(long)]
        user_email: Option<String>,
        /// How often to flush the audit trail, in seconds.
        #[arg(long, default_value_t = 30)]
        flush_secs: u64,
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

    match Cli::parse().command {
        Command::Owner { email } => owner(&email).await,
        Command::Status { bot, decisions } => status(&bot, decisions).await,
        Command::Run {
            strategy,
            symbol,
            from,
            to,
            source_timeframe,
            max_risk_pct,
            daily_loss_limit_r,
            weekly_loss_limit_r,
            persist,
            user_email,
            report_out,
        } => {
            run(
                &strategy,
                &symbol,
                &from,
                &to,
                &source_timeframe,
                limits(max_risk_pct, daily_loss_limit_r, weekly_loss_limit_r),
                persist,
                user_email.as_deref(),
                report_out.as_deref(),
            )
            .await
        }
        Command::Live {
            strategy,
            symbol,
            minutes,
            max_risk_pct,
            daily_loss_limit_r,
            weekly_loss_limit_r,
            persist,
            user_email,
            flush_secs,
        } => {
            live(
                &strategy,
                &symbol,
                minutes,
                limits(max_risk_pct, daily_loss_limit_r, weekly_loss_limit_r),
                persist,
                user_email.as_deref(),
                flush_secs,
            )
            .await
        }
    }
}

async fn owner(email: &str) -> Result<()> {
    let database = Database::from_env()
        .await
        .context("could not connect to postgres")?;
    database.migrate().await?;
    let id = db::paper::create_owner(database.pool(), email).await?;
    println!("owner {email} -> {id}");
    Ok(())
}

async fn status(bot: &str, show_decisions: bool) -> Result<()> {
    let bot_id = uuid::Uuid::parse_str(bot).with_context(|| format!("`{bot}` is not a uuid"))?;
    let database = Database::from_env()
        .await
        .context("could not connect to postgres")?;

    let Some(summary) = db::paper::bot_summary(database.pool(), bot_id).await? else {
        bail!("no bot with id {bot_id}");
    };

    println!("bot       {}", summary.id);
    println!("mode      {}   status {}", summary.mode, summary.status);
    if let Some(venue) = &summary.venue {
        println!("venue     {venue}");
    }
    println!(
        "created   {}",
        chrono::DateTime::from_timestamp(
            summary.created_at / 1_000_000_000,
            (summary.created_at % 1_000_000_000) as u32,
        )
        .map_or_else(|| "unknown".to_string(), |t| t.to_rfc3339())
    );
    println!(
        "trades    {} closed, {} open, cumulative {:.4}R",
        summary.trades, summary.open_trades, summary.cumulative_r
    );
    println!(
        "decisions {}   last {}",
        summary.decisions,
        summary
            .last_decision_at
            .and_then(|ns| chrono::DateTime::from_timestamp(
                ns / 1_000_000_000,
                (ns % 1_000_000_000) as u32
            ))
            .map_or_else(|| "never".to_string(), |t| t.to_rfc3339())
    );
    println!("alerts    {}", summary.notifications);

    // A bot that started and never stopped is the signature of a crash, and it
    // is the one thing a status view has to say out loud.
    if summary.status == "running" {
        let stalled = summary
            .last_decision_at
            .is_some_and(|ns| now_ns() - ns > 30 * 60 * 1_000_000_000);
        if stalled {
            println!(
                "\nWARNING: status is `running` but the newest decision is over 30 minutes old. \
                 Either the feed is quiet or the process died without stopping cleanly."
            );
        }
    }

    if show_decisions {
        println!();
        for payload in db::paper::recent_decisions(database.pool(), bot_id, 20).await? {
            println!(
                "{:>13}  {:>10}  {:<14} {}",
                payload["at"].as_i64().unwrap_or_default() / 1_000_000_000,
                payload["price"].as_f64().unwrap_or_default(),
                payload["kind"].as_str().unwrap_or("?"),
                payload["detail"],
            );
        }
    }
    Ok(())
}

fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

fn limits(max_risk_pct: f64, daily_loss_limit_r: f64, weekly_loss_limit_r: f64) -> RiskLimits {
    RiskLimits {
        max_risk_pct,
        daily_loss_limit_r,
        weekly_loss_limit_r,
        ..RiskLimits::default()
    }
}

/// Build a bot for a validated document.
fn bot_for(validated: &ValidatedStrategy, symbol: &str, limits: RiskLimits) -> Result<PaperBot> {
    let engine = StrategyEngine::new(validated, RuntimeConfig::default())
        .context("the document is not executable")?;
    Ok(PaperBot::new(
        engine,
        PaperConfig {
            symbol: symbol.to_string(),
            limits,
            fills: SimulatorConfig::default(),
            rolling: strategy_runtime::RollingConfig::new(
                RuntimeConfig::default().max_history,
                500,
                Default::default(),
            ),
        },
    ))
}

/// The decision timeframe's candles, plus every other declared series.
struct Window {
    decision_name: String,
    decision: Vec<Candle>,
    others: Vec<(String, Timeframe, Vec<Candle>)>,
}

fn window_for(
    document: &strategy_dsl::StrategyDocument,
    series: BTreeMap<String, Vec<Candle>>,
) -> Result<Window> {
    let (decision_name, _) = document
        .timeframes
        .iter()
        .min_by_key(|(_, tf)| tf.nanos())
        .map(|(name, tf)| (name.clone(), *tf))
        .context("the document declares no timeframes")?;

    let decision = series
        .get(&decision_name)
        .cloned()
        .context("the decision timeframe has no candles")?;

    let others = document
        .timeframes
        .iter()
        .filter(|(name, _)| **name != decision_name)
        .map(|(name, tf)| {
            (
                name.clone(),
                *tf,
                series.get(name).cloned().unwrap_or_default(),
            )
        })
        .collect();

    Ok(Window {
        decision_name,
        decision,
        others,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    strategy: &str,
    symbol: &str,
    from: &str,
    to: &str,
    source_timeframe: &str,
    limits: RiskLimits,
    persist: bool,
    user_email: Option<&str>,
    report_out: Option<&str>,
) -> Result<()> {
    let symbol = symbol.to_uppercase();
    let source_timeframe: Timeframe = source_timeframe
        .parse()
        .map_err(|e| anyhow::anyhow!("unknown source timeframe `{source_timeframe}`: {e}"))?;

    let yaml =
        std::fs::read_to_string(strategy).with_context(|| format!("could not read {strategy}"))?;
    let validated = strategy_dsl::parse_and_validate(&yaml)
        .with_context(|| format!("{strategy} is not a valid strategy document"))?;
    let document = validated.document().clone();

    let from_ns = parse_date_ns(from)?;
    let to_ns = parse_date_ns(to)? + 86_400 * 1_000_000_000;
    if to_ns <= from_ns {
        bail!("`--to` must not be before `--from`");
    }

    let database = Database::from_env()
        .await
        .context("could not connect to postgres")?;
    database.migrate().await?;

    let series = db::loading::load_timeframe_series(
        database.pool(),
        &symbol,
        &document.timeframes,
        from_ns,
        to_ns,
        source_timeframe,
    )
    .await?;
    db::loading::warn_about_short_series(&series, &document.timeframes, from_ns, to_ns);

    for (name, candles) in &series {
        println!("loaded {name}: {} candles", candles.len());
    }

    let window = window_for(&document, series.clone())?;

    // ---- path 1: the paper bot, fed the way a live feed would arrive ----
    let mut bot = bot_for(&validated, &symbol, limits)?;
    let mut decisions = 0u64;
    let mut others_seen = 0u64;
    let mut cursor: Vec<usize> = vec![0; window.others.len()];

    for candle in &window.decision {
        let now = candle.open_time + candle.timeframe.nanos();

        // Deliver every coarser candle that has already closed.
        for (index, (_, timeframe, candles)) in window.others.iter().enumerate() {
            let width = timeframe.nanos();
            while cursor[index] < candles.len() && candles[cursor[index]].open_time + width <= now {
                bot.on_candle(&candles[cursor[index]]);
                cursor[index] += 1;
                others_seen += 1;
            }
        }

        if bot.on_candle(candle).is_some() {
            decisions += 1;
        }
    }
    let bot_trades = bot.trades().to_vec();

    // ---- path 2: the backtester, over exactly the same candles ----
    let mut timeframes = document.timeframes.clone();
    timeframes.retain(|name, _| {
        name == &window.decision_name || window.others.iter().any(|(n, _, _)| n == name)
    });
    let mut replay_candles: BTreeMap<String, Vec<Candle>> = BTreeMap::new();
    replay_candles.insert(window.decision_name.clone(), window.decision.clone());
    for (name, _, candles) in &window.others {
        replay_candles.insert(name.clone(), candles.clone());
    }

    let engine = StrategyEngine::new(&validated, RuntimeConfig::default())?;
    let mut engine = engine;
    let output = replay(
        &mut engine,
        &ReplayInput {
            timeframes,
            candles: replay_candles,
        },
        &ReplayConfig {
            symbol: symbol.clone(),
            from: from_ns,
            to: to_ns,
            ..ReplayConfig::default()
        },
    )?;

    // ---- compare ----
    println!();
    println!(
        "paper bot:  {} decisions, {} trades",
        decisions,
        bot_trades.len()
    );
    println!(
        "backtester: {} decisions, {} trades",
        output.candles_processed,
        output.trades.len()
    );
    println!("context candles delivered: {others_seen}");

    let mut mismatches: Vec<String> = Vec::new();
    if decisions != output.candles_processed {
        mismatches.push(format!(
            "decision count differs: the bot decided {decisions} times, the replay {}",
            output.candles_processed
        ));
    }

    // A replay closes whatever is still open when the data runs out, because
    // dropping an open loser would flatter the statistics. A live bot has no
    // such event: the position is simply still open. That is an expected
    // difference, so it is named rather than reported as a divergence.
    let replay_closed_at_end = output
        .trades
        .last()
        .is_some_and(|trade| trade.exit_trigger == ExitTrigger::EndOfData);
    let open_position_accounted = bot.in_position() && replay_closed_at_end;
    let expected_trades = if open_position_accounted {
        output.trades.len() - 1
    } else {
        output.trades.len()
    };

    if bot_trades.len() != expected_trades {
        mismatches.push(format!(
            "trade count differs: the bot closed {}, the replay {}",
            bot_trades.len(),
            output.trades.len()
        ));
    }
    if open_position_accounted {
        println!(
            "note: the replay closed a still-open position at the end of the data; the bot \
             leaves it open, as a live bot would"
        );
    }

    for (index, (ours, theirs)) in bot_trades.iter().zip(output.trades.iter()).enumerate() {
        if ours.entry_time != theirs.entry_time
            || (ours.entry_price - theirs.entry_price).abs() > 1e-9
        {
            mismatches.push(format!(
                "trade {index}: the bot entered at {} ({}) but the replay at {} ({})",
                ours.entry_price, ours.entry_time, theirs.entry_price, theirs.entry_time
            ));
        }
        if (ours.r_multiple - theirs.r_multiple).abs() > 1e-9 {
            mismatches.push(format!(
                "trade {index}: the bot made {:.6}R, the replay {:.6}R",
                ours.r_multiple, theirs.r_multiple
            ));
        }
    }

    // Risk interventions are expected to cause a difference -- but they must
    // be counted and shown, never left as an unexplained gap.
    let decisions_taken = bot.take_decisions();
    let denied = decisions_taken
        .iter()
        .filter(|d| {
            matches!(
                d.outcome,
                trading_engine::DecisionOutcome::EntryDenied { .. }
            )
        })
        .count();
    let refused = decisions_taken
        .iter()
        .filter(|d| {
            matches!(
                d.outcome,
                trading_engine::DecisionOutcome::EntryRefused { .. }
            )
        })
        .count();

    println!();
    if mismatches.is_empty() {
        println!("MATCH: the paper bot and the replay agree on every trade.");
    } else {
        println!("MISMATCH ({} differences):", mismatches.len());
        for line in mismatches.iter().take(10) {
            println!("  - {line}");
        }
        if denied > 0 || refused > 0 {
            println!(
                "  ({denied} entries denied by risk, {refused} refused by the simulator -- \
                 those legitimately diverge from a backtest, which has no risk engine)"
            );
        }
    }

    println!(
        "bot cumulative R: {:.4}   replay final R: {:.4}",
        bot.cumulative_r(),
        output.final_r
    );
    if let Some(reason) = bot.halt_reason() {
        println!("kill-switch engaged: {reason}");
    }

    // ---- optionally persist ----
    if persist {
        let email = user_email.context("--persist needs --user-email")?;
        let user_id = db::paper::find_user_by_email(database.pool(), email)
            .await?
            .with_context(|| format!("no user with email {email}"))?;

        let document_json = serde_json::to_value(&document)?;
        let mut session = BotSession::start(
            &database,
            user_id,
            &document.name,
            &document.version,
            &document_json,
            "paper",
            None,
        )
        .await?;
        session.finish(&mut bot).await?;
        println!("persisted to bot {}", session.bot_id());
    }

    if let Some(path) = report_out {
        let summary = serde_json::json!({
            "strategy": document.name,
            "symbol": symbol,
            "decisions": decisions,
            "replay_decisions": output.candles_processed,
            "bot_trades": bot_trades.len(),
            "replay_trades": output.trades.len(),
            "mismatches": mismatches,
            "entries_denied_by_risk": denied,
            "entries_refused_by_simulator": refused,
            "bot_in_position": bot.in_position(),
            "replay_closed_open_position_at_end": open_position_accounted,
            "bot_cumulative_r": bot.cumulative_r(),
            "replay_final_r": output.final_r,
            "halted": bot.halt_reason(),
        });
        std::fs::write(path, serde_json::to_string_pretty(&summary)?)
            .with_context(|| format!("could not write {path}"))?;
        println!("report written to {path}");
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn live(
    strategy: &str,
    symbol: &str,
    minutes: Option<u64>,
    limits: RiskLimits,
    persist: bool,
    user_email: Option<&str>,
    flush_secs: u64,
) -> Result<()> {
    let symbol = symbol.to_uppercase();

    let yaml =
        std::fs::read_to_string(strategy).with_context(|| format!("could not read {strategy}"))?;
    let validated = strategy_dsl::parse_and_validate(&yaml)
        .with_context(|| format!("{strategy} is not a valid strategy document"))?;
    let document = validated.document().clone();

    let timeframes: Vec<Timeframe> = document.timeframes.values().copied().collect();
    let mut bot = bot_for(&validated, &symbol, limits)?;
    if let Some(note) = bot.clamp_note() {
        tracing::warn!("{note}");
    }

    let database = if persist {
        let database = Database::from_env()
            .await
            .context("could not connect to postgres")?;
        database.migrate().await?;
        Some(database)
    } else {
        tracing::info!("--persist not set: the audit trail will not be written");
        None
    };

    let mut session = match (&database, user_email) {
        (Some(database), Some(email)) => {
            let user_id = db::paper::find_user_by_email(database.pool(), email)
                .await?
                .with_context(|| format!("no user with email {email}"))?;
            let document_json = serde_json::to_value(&document)?;
            Some(
                BotSession::start(
                    database,
                    user_id,
                    &document.name,
                    &document.version,
                    &document_json,
                    "paper",
                    Some("binance"),
                )
                .await?,
            )
        }
        (Some(_), None) => bail!("--persist needs --user-email"),
        _ => None,
    };

    // The collector publishes to the bus; the bot consumes closed candles.
    //
    // Binance, explicitly: paper trading defaults to the venue whose REST
    // adapter the live bot is built against (`LiveBot<BinanceRest>`), and a
    // venue switch here would need that half to move too rather than only this
    // constructor.
    let registry = Arc::new(MarketBusRegistry::new());
    let mut collector = Collector::new(
        Arc::new(market_data::BinanceCodec::new()),
        market_data::CollectorConfig::default(),
        registry.clone(),
    );
    collector.connect().await?;

    let mut trades_rx = collector
        .trade_stream(&symbol)
        .context("no trade stream for this symbol")?;
    collector.subscribe_trades(&symbol).await?;

    println!(
        "paper bot live on {symbol} ({:?}); {}",
        timeframes,
        minutes.map_or_else(|| "Ctrl-C to stop".to_string(), |m| format!("{m} minutes"))
    );

    let mut builder = MultiTimeframeCandleBuilder::new(&symbol, &timeframes);
    let mut flush = tokio::time::interval(std::time::Duration::from_secs(flush_secs.max(1)));
    let deadline =
        minutes.map(|m| tokio::time::Instant::now() + std::time::Duration::from_secs(m * 60));

    loop {
        tokio::select! {
            received = trades_rx.recv() => match received {
                Ok(trade) => {
                    for candle in builder.on_trade(&trade) {
                        if bot.on_candle(&candle).is_some() {
                            tracing::debug!(
                                at = candle.open_time,
                                timeframe = %candle.timeframe,
                                "decision"
                            );
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // Losing trades means the candles built from them are
                    // wrong, so this is loud rather than a debug line.
                    tracing::warn!("trade feed lagged by {skipped} messages; candles may be short");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    bail!("the trade feed closed");
                }
            },
            _ = flush.tick() => {
                for alert in bot.take_alerts() {
                    // docs/11: a breach is meant to reach the user, so it goes
                    // to the console as well as into the audit trail.
                    println!(
                        "ALERT [{}] {}: {}",
                        alert.severity(),
                        alert.title(),
                        alert.body()
                    );
                }
                // Counted before the flush: `take_decisions` drains, so asking
                // afterwards reports zero for ever.
                let pending = bot.pending_decisions();
                if let Some(session) = session.as_mut() {
                    session.flush(&mut bot).await?;
                }
                println!(
                    "trades={} decisions_written={} cumulative_r={:.4}{}",
                    bot.trades().len(),
                    pending,
                    bot.cumulative_r(),
                    bot.halt_reason().map(|r| format!("  HALTED: {r}")).unwrap_or_default(),
                );
            }
            _ = tokio::signal::ctrl_c() => {
                println!("interrupted");
                break;
            }
            () = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                println!("reached the configured run length");
                break;
            }
        }

        if bot.is_halted() {
            tracing::warn!("kill-switch engaged; stopping the bot");
            break;
        }
    }

    if let Some(session) = session.as_mut() {
        session.finish(&mut bot).await?;
        println!("persisted to bot {}", session.bot_id());
    }

    println!(
        "finished: {} trades, cumulative R {:.4}",
        bot.trades().len(),
        bot.cumulative_r()
    );
    Ok(())
}

/// Parse `YYYY-MM-DD` into unix nanoseconds at 00:00 UTC.
fn parse_date_ns(date: &str) -> Result<i64> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .with_context(|| format!("could not parse `{date}` as YYYY-MM-DD"))?;
    let datetime = parsed
        .and_hms_opt(0, 0, 0)
        .context("midnight is not a valid time")?;
    Ok(datetime.and_utc().timestamp() * 1_000_000_000)
}
