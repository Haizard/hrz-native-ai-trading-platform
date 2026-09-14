//! # `agent-cli`
//!
//! Ask the agent real questions against real data, and watch what it actually
//! did.
//!
//! ```text
//! agent-cli ask --symbol BTCUSDT \
//!   --question "find me a long setup using my liquidity-sweep skill"
//!
//! agent-cli strategy --market BTCUSDT --timeframe 5m \
//!   --description "buy when price sweeps a low and reclaims it, stop beyond the sweep"
//! ```
//!
//! ## Why this exists separately from the API
//!
//! The Phase 5 exit criterion is about *grounding*: every number in the thesis
//! must trace to a tool call, and nothing may be hallucinated. That is only
//! checkable if the tool calls and their raw results are printed next to the
//! answer. `--trace` dumps them, so a thesis can be audited without stepping
//! through a debugger.
//!
//! It is also the cheapest way to iterate on prompts: no server, no browser,
//! one process, real Bedrock, real Postgres.

use std::path::Path;
use std::sync::Arc;

use ai_agent::{
    Agent, AgentConfig, AgentError, AskRequest, MarketDataSource, SkillLibrary, StrategyRequest,
};
use analytics_core::types::{Candle, Trade};
use analytics_core::Timeframe;
use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use db::repositories::{candles_range, load_candles, load_trades};
use db::Database;

#[derive(Parser)]
#[command(name = "agent-cli", about = "ask the trading agent real questions")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ask a question and get back an explainable thesis.
    Ask {
        /// Symbol, e.g. BTCUSDT.
        #[arg(long)]
        symbol: String,
        /// The question, in plain language.
        #[arg(long)]
        question: String,
        /// Pin a skill by id instead of retrieving by relevance.
        #[arg(long)]
        skill: Option<String>,
        /// Comma-separated ladder override, coarse to fine, e.g. "4h,1h,5m".
        #[arg(long)]
        timeframes: Option<String>,
        /// Where skills live.
        #[arg(long, default_value = "skills")]
        skills_dir: String,
        /// Print every tool call and its raw result.
        #[arg(long, default_value_t = false)]
        trace: bool,
        /// Write the thesis and its trace to a JSON file.
        #[arg(long)]
        json_out: Option<String>,
    },
    /// Generate a validated Strategy DSL document from a description.
    Strategy {
        /// What the strategy should do.
        #[arg(long)]
        description: String,
        /// Market the strategy trades.
        #[arg(long, default_value = "BTCUSDT")]
        market: String,
        /// Timeframe the entry fires on.
        #[arg(long, default_value = "5m")]
        timeframe: String,
        /// Pin a skill whose methodology to encode.
        #[arg(long)]
        skill: Option<String>,
        /// Where skills live.
        #[arg(long, default_value = "skills")]
        skills_dir: String,
        /// Where to write the resulting YAML.
        #[arg(long)]
        out: Option<String>,
    },
    /// List the skills in a directory and show which one a question retrieves.
    Skills {
        /// Where skills live.
        #[arg(long, default_value = "skills")]
        dir: String,
        /// Optional question, to show what would be retrieved for it.
        #[arg(long)]
        question: Option<String>,
        /// Symbol the question is about.
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
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

    if dotenvy::dotenv().is_ok() {
        tracing::debug!("loaded .env");
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Ask {
            symbol,
            question,
            skill,
            timeframes,
            skills_dir,
            trace,
            json_out,
        } => {
            run_ask(
                &symbol,
                &question,
                skill.as_deref(),
                timeframes.as_deref(),
                Path::new(&skills_dir),
                trace,
                json_out.as_deref(),
            )
            .await
        }
        Command::Strategy {
            description,
            market,
            timeframe,
            skill,
            skills_dir,
            out,
        } => {
            run_strategy(
                &description,
                &market,
                &timeframe,
                skill.as_deref(),
                Path::new(&skills_dir),
                out.as_deref(),
            )
            .await
        }
        Command::Skills {
            dir,
            question,
            symbol,
        } => run_skills(Path::new(&dir), question.as_deref(), &symbol),
    }
}

// ---------------------------------------------------------------------------
// Data source
// ---------------------------------------------------------------------------

/// `MarketDataSource` over Postgres.
///
/// `ai-agent` cannot depend on `db` (`docs/03`), so this adapter lives here --
/// one layer up, exactly as the trait was designed for.
struct DbMarketData {
    db: Arc<Database>,
}

#[async_trait]
impl MarketDataSource for DbMarketData {
    async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, AgentError> {
        load_candles(self.db.pool(), symbol, timeframe, from_ns, to_ns)
            .await
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: timeframe.to_string(),
                reason: e.to_string(),
            })
    }

    async fn trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, AgentError> {
        load_trades(self.db.pool(), symbol, from_ns, to_ns)
            .await
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: "ticks".into(),
                reason: e.to_string(),
            })
    }

    async fn latest_candle_time(
        &self,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Result<Option<i64>, AgentError> {
        candles_range(self.db.pool(), symbol, timeframe)
            .await
            .map(|range| range.map(|(_, hi)| hi))
            .map_err(|e| AgentError::DataUnavailable {
                symbol: symbol.into(),
                timeframe: timeframe.to_string(),
                reason: e.to_string(),
            })
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn run_ask(
    symbol: &str,
    question: &str,
    skill: Option<&str>,
    timeframes: Option<&str>,
    skills_dir: &Path,
    show_trace: bool,
    json_out: Option<&str>,
) -> Result<()> {
    let library = load_skills(skills_dir)?;
    let data = DbMarketData {
        db: Arc::new(connect().await?),
    };
    let agent = Agent::new(bedrock()?, library, AgentConfig::default());

    let mut request = AskRequest::new(symbol, question);
    if let Some(id) = skill {
        request = request.with_skill(id);
    }
    if let Some(frames) = timeframes {
        request = request.with_timeframes(
            frames
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }

    println!("question: {question}");
    println!("symbol:   {symbol}\n");

    let answer = agent.ask(&request, &data).await?;

    print_thesis(&answer);

    if show_trace {
        println!("\n--- ladder read before the model spoke ---");
        println!("{}", answer.ladder.digest());
        println!("--- tool calls ({}) ---", answer.trace.len());
        for (i, entry) in answer.trace.iter().enumerate() {
            println!(
                "{:>2}. {}({})",
                i + 1,
                entry.tool,
                serde_json::to_string(&entry.args).unwrap_or_default()
            );
        }
    }

    if let Some(path) = json_out {
        let payload = serde_json::json!({
            "question": question,
            "symbol": symbol,
            "skill": answer.skill,
            "turns": answer.turns,
            "thesis": answer.thesis,
            "trace": answer.trace,
        });
        std::fs::write(path, serde_json::to_string_pretty(&payload)?)?;
        println!("\nwrote {path}");
    }

    Ok(())
}

async fn run_strategy(
    description: &str,
    market: &str,
    timeframe: &str,
    skill: Option<&str>,
    skills_dir: &Path,
    out: Option<&str>,
) -> Result<()> {
    let library = load_skills(skills_dir)?;
    let agent = Agent::new(bedrock()?, library, AgentConfig::default());

    let mut request = StrategyRequest::new(description, market, timeframe);
    if let Some(id) = skill {
        request.skill_id = Some(id.into());
    }

    let generated = agent.generate_strategy(&request).await?;
    let document = generated.document();

    println!("valid after {} attempt(s)", generated.attempts);
    for error in &generated.repaired_errors {
        println!("  repaired: {error}");
    }
    println!("\nname:    {}", document.name);
    println!("version: {}", document.version);
    println!("market:  {}", document.market);
    if let Some(direction) = document.direction() {
        println!("side:    {direction:?}");
    }

    if let Some(path) = out {
        std::fs::write(path, &generated.yaml)?;
        println!("\nwrote {path}");
    } else {
        println!("\n{}", generated.yaml);
    }
    Ok(())
}

fn run_skills(dir: &Path, question: Option<&str>, symbol: &str) -> Result<()> {
    let library = load_skills(dir)?;
    println!("{} skill(s) in {}\n", library.len(), dir.display());
    for skill in library.latest_versions() {
        println!(
            "  {:<40} v{:<6} {}",
            skill.id(),
            skill.version,
            skill.category
        );
    }

    if let Some(question) = question {
        let query = ai_agent::SkillQuery::for_market(symbol)
            .with_terms(question.split_whitespace().map(str::to_string));
        let hits = library.retrieve(&query);
        println!("\nretrieved for \"{question}\" on {symbol}:");
        if hits.is_empty() {
            println!("  (none -- the agent will say so rather than improvise)");
        }
        for skill in hits {
            println!("  {} v{}", skill.id(), skill.version);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn print_thesis(answer: &ai_agent::AgentAnswer) {
    let t = &answer.thesis;
    println!("{} on {}  ({:?})", t.symbol, t.timeframe, t.direction);
    println!("confidence: {:.0}%", t.confidence_pct);
    if let Some(skill) = &t.skill_used {
        println!("skill:      {skill}");
    }

    println!("\nhigher timeframe:");
    for check in &t.higher_timeframe_checks {
        println!(
            "  [{}] {} -- {}",
            check.status.mark(),
            check.label,
            check.detail
        );
        if let Some(observed) = check.observed {
            println!("         observed {observed} (via {})", check.source);
        }
    }
    println!("order flow:");
    for check in &t.order_flow_checks {
        println!(
            "  [{}] {} -- {}",
            check.status.mark(),
            check.label,
            check.detail
        );
        if let Some(observed) = check.observed {
            println!("         observed {observed} (via {})", check.source);
        }
    }

    println!(
        "\nentry {:.4}  stop {:.4}  target {:.4}  R:R {:.2}",
        t.entry_price, t.stop_price, t.target_price, t.risk_reward
    );
    println!("invalidation: {}", t.invalidation);
    if let Some(n) = t.historical_similar_setups {
        let rate = t
            .historical_win_rate
            .map(|r| format!(", {:.1}% win rate", r * 100.0))
            .unwrap_or_default();
        println!("history: {n} similar setups{rate}");
    }

    let (passed, failed, unknown) = t.check_tally();
    println!("\nchecks: {passed} passed, {failed} failed, {unknown} unknown");
    println!("\n{}", t.narrative);
    println!(
        "\n({} tool calls, {} model turn(s))",
        answer.trace.len(),
        answer.turns
    );
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

fn load_skills(dir: &Path) -> Result<SkillLibrary> {
    if !dir.exists() {
        // Not an error: a user with no skills defined yet should get "no
        // matching skill", not a crash on a missing directory.
        tracing::warn!("skills directory {} does not exist", dir.display());
        return Ok(SkillLibrary::new());
    }
    SkillLibrary::load_dir(dir).context("loading skills")
}

fn bedrock() -> Result<Arc<ai_agent::BedrockClient>> {
    let config = ai_agent::BedrockConfig::from_env()?;
    tracing::info!(model = %config.model_id, region = %config.region, "using bedrock");
    Ok(Arc::new(ai_agent::BedrockClient::new(config)?))
}

async fn connect() -> Result<Database> {
    let db = Database::from_env()
        .await
        .context("connecting to the database -- is DATABASE_URL set?")?;
    Ok(db)
}
