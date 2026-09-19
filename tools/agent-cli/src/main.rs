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

/// `MarketDataSource` over the unified RAM + venue window.
///
/// `ai-agent` cannot depend on `db` (`docs/03`), so this adapter lives here --
/// one layer up, exactly as the trait was designed for.
///
/// ## Why this is no longer `DbMarketData`
///
/// It used to read candles out of Postgres. That was the last surviving trace
/// of the assumption that market data is stored, and by the time it was removed
/// from the gateway it had already produced a real defect there: the chart drew
/// a symbol from RAM plus the venue while the agent answered "no data" for the
/// same symbol at the same moment, because nobody had backfilled it by hand.
///
/// The CLI kept a copy, which meant `--trace` audited the agent against a
/// *different* source than the one the product serves. An audit tool that
/// disagrees with production about what a candle was is worse than no audit
/// tool. It now reads [`market_data::WindowService`] -- the same merge the
/// chart, the scanner and `/candles` read through.
struct WindowMarketData {
    windows: market_data::WindowService,
}

impl WindowMarketData {
    /// Build the one service this CLI reads from.
    ///
    /// The registries are empty and stay empty: the CLI opens no websocket
    /// feed, so every bar comes from the venue's REST. That is the point of the
    /// window service -- a consumer with no feed still gets a correct answer --
    /// and it is also why `2026-09-19`'s `1w` work matters here: weekly exists
    /// only on this path.
    fn from_env() -> Self {
        let backfill = market_data::BackfillClient::new(
            std::env::var("MARKET_REST_URL")
                .unwrap_or_else(|_| "https://api.binance.com".to_string()),
        );
        Self {
            windows: market_data::WindowService::new(
                Arc::new(market_data::HistoryRegistry::new()),
                Arc::new(market_data::LiveRegistry::new()),
                backfill,
            ),
        }
    }

    /// The error a failed read produces, in the agent's own vocabulary.
    fn unavailable(
        symbol: &str,
        timeframe: impl Into<String>,
        reason: impl Into<String>,
    ) -> AgentError {
        AgentError::DataUnavailable {
            symbol: symbol.into(),
            timeframe: timeframe.into(),
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl MarketDataSource for WindowMarketData {
    async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, AgentError> {
        let window = self
            .windows
            .candles(symbol, timeframe, from_ns, to_ns)
            .await
            .map_err(|e| Self::unavailable(symbol, timeframe.to_string(), e.to_string()))?;

        // Printed on `--trace` rather than only on failure, because the
        // interesting case is not "the read threw" -- it is "the read returned
        // twelve bars out of three hundred, and nothing said so". The agent
        // reasons over whatever it is handed, so a thin window it cannot see is
        // a confident answer built on a sample nobody checked -- and this is the
        // tool whose whole purpose is auditing that.
        tracing::debug!(
            symbol,
            timeframe = %timeframe,
            bars = window.candles.len(),
            provenance = %window.provenance(),
            "market read"
        );
        Ok(window.candles)
    }

    async fn trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, AgentError> {
        // Never an error, and never a venue call: the tape is the only place
        // trades exist (`docs/04`), and a CLI opens no feed, so its tape is
        // empty. Returning empty is the honest answer -- `ai-agent` reports
        // `NO_TICK_DATA` for it rather than reading an empty tape as "the book
        // was balanced". The old path answered from Postgres, where the rows
        // only existed if someone had pumped them by hand.
        Ok(self.windows.trades(symbol, from_ns, to_ns))
    }

    async fn latest_candle_time(
        &self,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Result<Option<i64>, AgentError> {
        let symbol = symbol.to_uppercase();
        // With no feed the buffer is always empty, so this always pays the
        // venue; kept as the buffer-first check anyway so the two adapters read
        // the same, and so a future `agent-cli` that does open a feed needs no
        // change here.
        if let Some(newest) = self.windows.history().newest(&symbol, timeframe) {
            return Ok(Some(newest));
        }

        self.windows
            .latest(&symbol, timeframe, 1)
            .await
            .map(|window| window.candles.last().map(|c| c.open_time))
            .map_err(|e| Self::unavailable(&symbol, timeframe.to_string(), e.to_string()))
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
    let data = WindowMarketData::from_env();
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
