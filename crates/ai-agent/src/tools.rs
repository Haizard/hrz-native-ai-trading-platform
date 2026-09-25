//! The tool registry: everything the model is allowed to ask the platform for.
//!
//! ## Why this module is a thin shell, not an implementation
//!
//! `docs/09` is blunt about it: each tool is "a thin wrapper calling the Rust
//! functions from `analytics-core`". Nothing here calculates anything that
//! `analytics-core` already calculates. A tool that did would create a second
//! source of truth, and then the number on the chart, the number in the
//! backtest and the number in the thesis could disagree.
//!
//! ## Why data access is a trait, not a dependency
//!
//! `ai-agent` may depend on `analytics-core` and `strategy-dsl` and nothing
//! else (`docs/03`). It therefore cannot read Postgres, and it cannot run a
//! backtest -- both of which the tools plainly need. So the two capabilities
//! are declared here as [`MarketDataSource`] and [`BacktestRunner`] and
//! implemented one layer up. That keeps the dependency rule intact, and it
//! makes the whole tool layer testable against a fixture source.
//!
//! The same rule puts the user's drawings behind [`UserDrawingsSource`]
//! (`crate::user_drawings`): the model may *read* what the user drew, through
//! `get_user_drawings`, and the host decides whose drawings those are.
//! Read-only is the whole contract -- an agent that could move a level would
//! be able to edit the analysis it is being asked to reason about.

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tracing::debug;

use analytics_core::types::{Candle, Timeframe, Trade};
use analytics_core::volume_profile::VolumeNode;
use analytics_core::{
    build_market_state, calculate_volume_profile_from_candles, detect_absorption,
    detect_imbalances_with, detect_liquidity_levels_with, detect_market_structure, MarketState,
    MarketStateConfig,
};

use crate::error::AgentError;
use crate::llm_client::ToolCall;
use crate::user_drawings::{DrawingWriter, NewAgentDrawing, UserDrawingsSource};

use crate::agent_memory::{MemorySource, MemoryWriter, NewMemory};

/// Where market data comes from.
///
/// Implemented by the binary (against `db`) rather than by this crate, so the
/// agent stays free of storage concerns and testable against fixtures.
#[async_trait]
pub trait MarketDataSource: Send + Sync {
    /// Candles in `[from_ns, to_ns)`, ascending by time.
    async fn candles(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Candle>, AgentError>;

    /// Trades in `[from_ns, to_ns)`, ascending by time.
    ///
    /// May legitimately return empty: trade history is expensive to store and
    /// is often absent for older windows. Every tool that needs tick data
    /// degrades gracefully (see the note on footprint-level signals below).
    async fn trades(
        &self,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<Vec<Trade>, AgentError>;

    /// Open time of the newest stored candle, if any.
    ///
    /// This is what anchors "the last N bars". Deriving it from the wall clock
    /// instead would ask for a window the collector has not filled yet, which
    /// reads as data loss rather than as "not collected yet".
    async fn latest_candle_time(
        &self,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Result<Option<i64>, AgentError>;
}

/// Headline statistics from a backtest.
///
/// Mirrors `backtester::BacktestReport`, deliberately by hand: `ai-agent` does
/// not depend on `backtester`, and duplicating an eight-field summary is
/// cheaper than the dependency edge, which would drag `strategy-runtime` in
/// with it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BacktestSummary {
    /// Strategy name from the document.
    pub strategy: String,
    /// Timeframe decisions were made on.
    pub decision_timeframe: String,
    /// Completed trades.
    pub total_trades: u32,
    /// Fraction of trades that made money, 0..1.
    pub win_rate: f64,
    /// Gross profit / gross loss, in R.
    pub profit_factor: f64,
    /// Total R.
    pub net_return_r: f64,
    /// Largest drawdown of the cumulative R curve, in R.
    pub max_drawdown_r: f64,
    /// Mean R per trade.
    pub average_r: f64,
    /// Setups that fired but never became trades.
    pub skipped_signals: u32,
    /// Anything the caller should know about how this number was produced.
    pub note: Option<String>,
}

/// Runs backtests on the agent's behalf.
#[async_trait]
pub trait BacktestRunner: Send + Sync {
    /// Backtest a DSL document (YAML or JSON text) over a window.
    async fn run(
        &self,
        document: &str,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<BacktestSummary, AgentError>;

    /// Historical base rate for the setups a skill describes.
    ///
    /// `skill_ref` is the skill's stable id (e.g.
    /// `liquidity-sweep-absorption-v2`). The runner resolves it to a reference
    /// strategy document; the agent does not get to invent that mapping.
    async fn similar_setups(
        &self,
        skill_ref: &str,
        symbol: &str,
        from_ns: i64,
        to_ns: i64,
    ) -> Result<BacktestSummary, AgentError>;
}

/// Everything a tool needs in order to run.
pub struct ToolContext<'a> {
    /// Where candles and trades come from.
    pub data: &'a dyn MarketDataSource,
    /// Backtests, when the host provides them.
    pub backtests: Option<&'a dyn BacktestRunner>,
    /// Analytics tuning (bucket size, lookbacks, ...).
    pub config: MarketStateConfig,
    /// Bars of history each tool reads unless the model asks for a specific
    /// amount.
    pub default_lookback: usize,
    /// Upper bound on bars per call, so a model asking for `limit: 100000`
    /// gets a bounded answer instead of a 60-second query.
    pub max_lookback: usize,
    /// The asking user's drawings, when the host provided them.
    ///
    /// `None` is a fact the tool reports ("no drawings source is attached"),
    /// not a blank chart the model may assume. Read-only by construction:
    /// [`UserDrawingsSource`] has no write method to call.
    pub drawings: Option<&'a dyn UserDrawingsSource>,
    /// Whose drawings [`Self::drawings`] holds, opaque to this crate.
    pub user_id: Option<&'a str>,
    /// Where the agent may put its own objects, when the host allows writes.
    ///
    /// A separate capability from [`Self::drawings`], and `None` by default:
    /// a host that attaches only a reader gets a read-only agent, and the
    /// write tools report the absence rather than pretending. Wired by
    /// `with_drawing_writer` alongside the reader, under the same
    /// authenticated identity.
    pub drawing_writer: Option<&'a dyn DrawingWriter>,
    /// The asking user's memory, when the host provided it.
    ///
    /// The same posture as the drawings: `None` is a fact the tools report,
    /// not an empty memory the model may assume. The writer is a separate
    /// `Option` so a host can grant read-only memory, mirroring the drawing
    /// reader/writer split.
    pub memory: Option<&'a dyn MemorySource>,
    /// Where the agent may store facts, when the host allows writes.
    pub memory_writer: Option<&'a dyn MemoryWriter>,
    /// A separate copy of the user id for the memory tools.
    ///
    /// Deliberately the *same* value as [`Self::user_id`] whenever either is
    /// set (the builders enforce it); a distinct field keeps each capability's
    /// builder signature self-contained the way `with_drawing_writer` does.
    pub memory_user_id: Option<&'a str>,
}

impl<'a> ToolContext<'a> {
    /// A context with default analytics tuning.
    #[must_use]
    pub fn new(data: &'a dyn MarketDataSource) -> Self {
        Self {
            data,
            backtests: None,
            config: MarketStateConfig::default(),
            default_lookback: 300,
            max_lookback: 2000,
            drawings: None,
            user_id: None,
            drawing_writer: None,
            memory: None,
            memory_writer: None,
            memory_user_id: None,
        }
    }

    /// Attach the asking user's memory, and whose it is.
    #[must_use]
    pub fn with_memory(
        mut self,
        source: &'a dyn MemorySource,
        writer: Option<&'a dyn MemoryWriter>,
        user_id: &'a str,
    ) -> Self {
        self.memory = Some(source);
        self.memory_writer = writer;
        self.memory_user_id = Some(user_id);
        self
    }

    /// Attach the asking user's drawings, and whose they are.
    #[must_use]
    pub fn with_drawings(mut self, drawings: &'a dyn UserDrawingsSource, user_id: &'a str) -> Self {
        self.drawings = Some(drawings);
        self.user_id = Some(user_id);
        self
    }

    /// Allow the agent to write chart objects, as this user.
    ///
    /// Both the reader and the writer take the same opaque id: a host that
    /// wires one without the other gets exactly the posture it asked for,
    /// and there is no path by which the two identities can diverge.
    #[must_use]
    pub fn with_drawing_writer(mut self, writer: &'a dyn DrawingWriter, user_id: &'a str) -> Self {
        self.drawing_writer = Some(writer);
        self.user_id = Some(user_id);
        self
    }

    /// Attach a backtest runner.
    #[must_use]
    pub fn with_backtests(mut self, backtests: &'a dyn BacktestRunner) -> Self {
        self.backtests = Some(backtests);
        self
    }

    /// Override the analytics tuning.
    #[must_use]
    pub fn with_config(mut self, config: MarketStateConfig) -> Self {
        self.config = config;
        self
    }

    /// Clamp a requested lookback into `[1, max_lookback]`.
    #[must_use]
    pub fn clamp_lookback(&self, requested: Option<u64>) -> usize {
        let requested = requested
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(self.default_lookback);
        requested.clamp(1, self.max_lookback)
    }
}

/// The set of tools exposed to the model for one request.
///
/// Schemas and dispatch live together on purpose. The failure mode of splitting
/// them is a tool the model can see but the registry cannot run, which surfaces
/// as a mysterious "model is confused" error rather than as a build error; the
/// test `every_registered_tool_is_dispatchable` asserts they cannot drift.
pub struct ToolRegistry {
    specs: Vec<crate::llm_client::ToolSpec>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::market_analysis()
    }
}

impl ToolRegistry {
    /// The standard analysis tool set from `docs/09`.
    #[must_use]
    pub fn market_analysis() -> Self {
        use crate::llm_client::ToolSpec;

        Self {
            specs: vec![
                ToolSpec {
                    name: "analyze_timeframe".into(),
                    description: ANALYZE_TIMEFRAME.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "analyze_multi_timeframe".into(),
                    description: ANALYZE_MULTI_TIMEFRAME.into(),
                    input_schema: multi_timeframe_schema(),
                },
                ToolSpec {
                    name: "get_candles".into(),
                    description: GET_CANDLES.into(),
                    input_schema: symbol_timeframe_schema(Some("limit")),
                },
                ToolSpec {
                    name: "get_volume_profile".into(),
                    description: GET_VOLUME_PROFILE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "get_footprint".into(),
                    description: GET_FOOTPRINT.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                },
                ToolSpec {
                    name: "get_delta".into(),
                    description: GET_DELTA.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "get_cvd".into(),
                    description: GET_CVD.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "get_vwap".into(),
                    description: GET_VWAP.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "detect_liquidity".into(),
                    description: DETECT_LIQUIDITY.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "detect_absorption".into(),
                    description: DETECT_ABSORPTION.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "detect_imbalance".into(),
                    description: DETECT_IMBALANCE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "detect_market_structure".into(),
                    description: DETECT_MARKET_STRUCTURE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                },
                ToolSpec {
                    name: "backtest_strategy".into(),
                    description: BACKTEST_STRATEGY.into(),
                    input_schema: backtest_strategy_schema(),
                },
                ToolSpec {
                    name: "backtest_similar_setups".into(),
                    description: BACKTEST_SIMILAR_SETUPS.into(),
                    input_schema: backtest_similar_schema(),
                },
                ToolSpec {
                    name: "get_user_drawings".into(),
                    description: GET_USER_DRAWINGS.into(),
                    input_schema: get_user_drawings_schema(),
                },
                ToolSpec {
                    name: "create_drawing".into(),
                    description: CREATE_DRAWING.into(),
                    input_schema: drawing_write_schema(false),
                },
                ToolSpec {
                    name: "update_drawing".into(),
                    description: UPDATE_DRAWING.into(),
                    input_schema: drawing_write_schema(true),
                },
                ToolSpec {
                    name: "delete_drawing".into(),
                    description: DELETE_DRAWING.into(),
                    input_schema: drawing_delete_schema(),
                },
                ToolSpec {
                    name: "remember".into(),
                    description: REMEMBER.into(),
                    input_schema: remember_schema(),
                },
                ToolSpec {
                    name: "recall_memories".into(),
                    description: RECALL_MEMORIES.into(),
                    input_schema: recall_memories_schema(),
                },
                ToolSpec {
                    name: "forget_memory".into(),
                    description: FORGET_MEMORY.into(),
                    input_schema: forget_memory_schema(),
                },
            ],
        }
    }

    /// Every schema, in registration order, ready for a `toolConfig`.
    #[must_use]
    pub fn specs(&self) -> Vec<crate::llm_client::ToolSpec> {
        self.specs.clone()
    }

    /// Tool names, for logging and tests.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.specs.iter().map(|s| s.name.as_str()).collect()
    }

    /// Whether a tool is registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.specs.iter().any(|s| s.name == name)
    }

    /// Run one tool call.
    ///
    /// # Errors
    /// [`AgentError::UnknownTool`] when the model hallucinates a tool name;
    /// [`AgentError::ToolFailed`] when a registered tool fails. The two are
    /// deliberately different: the first means the model is confused, the
    /// second means the platform is.
    pub async fn execute(
        &self,
        call: &ToolCall,
        ctx: &ToolContext<'_>,
    ) -> Result<Value, AgentError> {
        if !self.contains(&call.name) {
            return Err(AgentError::UnknownTool(call.name.clone()));
        }

        debug!(target: "ai_agent", tool = %call.name, args = %call.input, "tool call");
        let started = std::time::Instant::now();
        let result = match call.name.as_str() {
            "analyze_timeframe" => analyze_timeframe(ctx, &call.input).await,
            "analyze_multi_timeframe" => analyze_multi_timeframe(ctx, &call.input).await,
            "get_candles" => get_candles(ctx, &call.input).await,
            "get_volume_profile" => get_volume_profile(ctx, &call.input).await,
            "get_footprint" => get_footprint(ctx, &call.input).await,
            "get_delta" => get_delta(ctx, &call.input).await,
            "get_cvd" => get_cvd(ctx, &call.input).await,
            "get_vwap" => get_vwap(ctx, &call.input).await,
            "detect_liquidity" => detect_liquidity(ctx, &call.input).await,
            "detect_absorption" => detect_absorption_tool(ctx, &call.input).await,
            "detect_imbalance" => detect_imbalance(ctx, &call.input).await,
            "detect_market_structure" => detect_market_structure_tool(ctx, &call.input).await,
            "backtest_strategy" => backtest_strategy(ctx, &call.input).await,
            "backtest_similar_setups" => backtest_similar_setups(ctx, &call.input).await,
            "get_user_drawings" => get_user_drawings(ctx, &call.input).await,
            "create_drawing" => create_drawing(ctx, &call.input).await,
            "update_drawing" => update_drawing(ctx, &call.input).await,
            "delete_drawing" => delete_drawing(ctx, &call.input).await,
            "remember" => remember(ctx, &call.input).await,
            "recall_memories" => recall_memories(ctx, &call.input).await,
            "forget_memory" => forget_memory(ctx, &call.input).await,
            other => return Err(AgentError::UnknownTool(other.to_string())),
        };
        let elapsed = started.elapsed();

        match &result {
            Ok(_) => debug!(target: "ai_agent", tool = %call.name, ?elapsed, "tool ok"),
            Err(e) => {
                debug!(target: "ai_agent", tool = %call.name, ?elapsed, error = %e, "tool failed")
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// The timeframe choices the schemas advertise, finest first.
///
/// Built from [`Timeframe::all`] rather than typed out, for the same reason
/// `timeframe_arg`'s error message is: the schema the model reads and the
/// parser the tools run must accept the same set. This enum was hand-written
/// once and `1w` was added to the engine without it, so the model was being
/// told weekly did not exist while the rest of the platform charted it -- and
/// a model that obeys its schema will never try what the schema forbids.
fn timeframe_choices() -> Vec<&'static str> {
    Timeframe::all()
        .iter()
        .rev()
        .map(|tf| tf.as_str())
        .collect()
}

/// The schema `enum` for a timeframe, from [`timeframe_choices`].
fn timeframe_enum() -> Vec<Value> {
    timeframe_choices().into_iter().map(Value::from).collect()
}

/// The schema `description` for a timeframe, from [`timeframe_choices`], so
/// the prose and the enum cannot disagree.
fn timeframe_description() -> String {
    let choices = timeframe_choices();
    let (last, rest) = choices.split_last().expect("Timeframe::all is non-empty");
    format!("Bar size: {} or {last}", rest.join(", "))
}

fn symbol_timeframe_schema(extra: Option<&str>) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "symbol".into(),
        json!({"type": "string", "description": "Trading symbol, e.g. BTCUSDT"}),
    );
    properties.insert(
        "timeframe".into(),
        json!({
            "type": "string",
            "description": timeframe_description(),
            "enum": timeframe_enum(),
        }),
    );
    if let Some(name) = extra {
        properties.insert(
            name.into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "Bars of history to read. Omit for the default window.",
            }),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["symbol", "timeframe"],
    })
}

fn multi_timeframe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "timeframes": {
                "type": "array",
                "items": {"type": "string", "enum": timeframe_enum()},
                "description": "Timeframes to read, e.g. [\"4h\", \"1h\", \"5m\"]",
            },
            "lookback": {"type": "integer", "minimum": 1, "description": "Bars per timeframe"},
        },
        "required": ["symbol", "timeframes"],
    })
}

fn backtest_strategy_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document": {
                "type": "string",
                "description": "A complete Strategy DSL document in YAML.",
            },
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "days": {"type": "integer", "minimum": 1, "description": "Window length in days (default 180)"},
        },
        "required": ["document", "symbol"],
    })
}

fn get_user_drawings_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
        },
        "required": ["symbol"],
    })
}

/// The write tools' schema. `kind` is deliberately a free string, **not** an
/// enum: the storage's vocabulary is the source of truth, an unknown kind
/// gets a correctable error naming the valid ones, and a stale schema enum
/// would read to the model as "this kind is unsupported" (the same lesson
/// `timeframe_arg` records for the same reason). `with_id` is the update
/// form, which names an existing drawing rather than describing a new one.
fn drawing_write_schema(with_id: bool) -> Value {
    let mut properties = json!({
        "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
        "kind": {
            "type": "string",
            "description": "What to draw. Kinds: trendline, hline, vline, ray, extended, rect, fib, measure, channel (3 anchors), angle, arc (3 anchors), circle, triangle (3 anchors), position_long, position_short, dateprice_range",
        },
        "time1_ms": {"type": "number", "description": "First anchor, milliseconds since the epoch"},
        "price1": {"type": "number", "description": "First anchor's price"},
        "time2_ms": {"type": "number", "description": "Second anchor's time, for kinds that need two anchors"},
        "price2": {"type": "number", "description": "Second anchor's price"},
        "time3_ms": {"type": "number", "description": "Third anchor's time, only for channel, arc and triangle"},
        "price3": {"type": "number", "description": "Third anchor's price, only for channel, arc and triangle"},
        "label": {"type": "string", "description": "A short name the user will read on the chart"},
        "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "How confident you are in this object, 0-1. Optional."},
        "reason": {"type": "string", "description": "Why this object belongs on the chart, in one or two sentences. Shown to the user."},
    });
    if with_id {
        properties["id"] = json!({
            "type": "string",
            "description": "The drawing's id, from get_user_drawings or a create_drawing answer",
        });
    }
    let mut required = vec!["symbol", "kind", "time1_ms", "price1"];
    if with_id {
        required.push("id");
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn drawing_delete_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "id": {"type": "string", "description": "The drawing's id, from get_user_drawings"},
        },
        "required": ["symbol", "id"],
    })
}

fn remember_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scope": {"type": "string", "enum": ["symbol", "global"], "description": "`symbol` for a fact about this market, `global` for a preference about this user"},
            "symbol": {"type": "string", "description": "The symbol, required exactly when scope is `symbol`"},
            "key": {"type": "string", "description": "Short slug for the fact, e.g. 4h_resistance or risk_style. Stating the same key again replaces the earlier fact"},
            "content": {"type": "string", "description": "The fact, one or two sentences, in numbers where possible"},
        },
        "required": ["scope", "key", "content"],
    })
}

fn recall_memories_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "How many facts to read (default 24)"},
        },
        "required": ["symbol"],
    })
}

fn forget_memory_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "The symbol of a symbol-scoped fact; omit for a global preference"},
            "key": {"type": "string", "description": "The key of the fact to drop, e.g. 4h_resistance"},
        },
        "required": ["key"],
    })
}

fn backtest_similar_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "skill_ref": {
                "type": "string",
                "description": "The skill id from the retrieved skill, e.g. liquidity-sweep-absorption-v2",
            },
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "days": {"type": "integer", "minimum": 1, "description": "Window length in days (default 180)"},
        },
        "required": ["skill_ref", "symbol"],
    })
}

// ---------------------------------------------------------------------------
// Argument readers. The model's `input` is JSON and arrives untrusted: a
// missing field or a string where a number belongs must produce a message the
// model can recover from, not a panic in the orchestrator.
// ---------------------------------------------------------------------------

fn string_arg(args: &Value, key: &str, tool: &str) -> Result<String, AgentError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: format!("`{key}` is required and must be a non-empty string"),
        })
}

fn timeframe_arg(args: &Value, tool: &str) -> Result<Timeframe, AgentError> {
    let raw = string_arg(args, "timeframe", tool)?;
    raw.parse::<Timeframe>()
        .map_err(|_| AgentError::InvalidToolArgs {
            tool: tool.into(),
            // Built from `Timeframe::all()` rather than typed out, so a new
            // resolution cannot be added to the engine and stay invisible to
            // the model. The list was hand-written once and `1w` was promptly
            // missing from it, which reads to a model as "weekly is
            // unsupported" rather than as a stale string.
            reason: format!(
                "`timeframe` must be one of {}; got `{raw}`",
                Timeframe::all()
                    .iter()
                    .rev()
                    .map(|tf| tf.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
}

fn optional_u64(args: &Value, key: &str, tool: &str) -> Result<Option<u64>, AgentError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| AgentError::InvalidToolArgs {
                tool: tool.into(),
                reason: format!("`{key}` must be a positive integer"),
            }),
    }
}

// ---------------------------------------------------------------------------
// Window resolution
// ---------------------------------------------------------------------------

/// Resolve `[from, to)` covering the last `lookback` bars ending at the newest
/// stored candle.
async fn window(
    ctx: &ToolContext<'_>,
    symbol: &str,
    timeframe: Timeframe,
    lookback: usize,
) -> Result<(i64, i64), AgentError> {
    let latest = ctx
        .data
        .latest_candle_time(symbol, timeframe)
        .await?
        .ok_or_else(|| AgentError::NoData {
            symbol: symbol.to_string(),
            timeframe: timeframe.to_string(),
        })?;

    let width = timeframe.nanos();
    // `latest` is the open time of the newest candle, so the window must extend
    // one full bar past it to cover that candle's own span.
    let bars = i64::try_from(lookback.saturating_sub(1)).unwrap_or(i64::MAX);
    Ok((latest - bars * width, latest + width))
}

/// Load the window the model asked for, or explain why there is nothing there.
async fn load_window(
    ctx: &ToolContext<'_>,
    symbol: &str,
    timeframe: Timeframe,
    lookback: usize,
) -> Result<(Vec<Candle>, Vec<Trade>), AgentError> {
    let (from, to) = window(ctx, symbol, timeframe, lookback).await?;
    let candles = ctx.data.candles(symbol, timeframe, from, to).await?;
    if candles.is_empty() {
        return Err(AgentError::NoData {
            symbol: symbol.to_string(),
            timeframe: timeframe.to_string(),
        });
    }
    let trades = ctx.data.trades(symbol, from, to).await.unwrap_or_default();
    if trades.is_empty() {
        debug!(
            target: "ai_agent",
            %symbol, %timeframe,
            "no trade history in window; footprint-level signals will be unavailable"
        );
    }
    Ok((candles, trades))
}

/// Build the `MarketState` one tool will describe.
async fn state_for(
    ctx: &ToolContext<'_>,
    symbol: &str,
    timeframe: Timeframe,
    lookback: usize,
) -> Result<(MarketState, Vec<Candle>, Vec<Trade>), AgentError> {
    let (candles, trades) = load_window(ctx, symbol, timeframe, lookback).await?;
    let state =
        build_market_state(&candles, &trades, &ctx.config).ok_or_else(|| AgentError::NoData {
            symbol: symbol.to_string(),
            timeframe: timeframe.to_string(),
        })?;
    Ok((state, candles, trades))
}

/// Footprints for the trailing window, using the same cut-off as
/// `build_market_state`.
fn recent_footprints(
    candles: &[Candle],
    trades: &[Trade],
    bucket_size: f64,
    max_candles: usize,
) -> Vec<analytics_core::FootprintCandle> {
    let start = candles.len().saturating_sub(max_candles.max(1));
    analytics_core::build_footprints(&candles[start..], trades, bucket_size)
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

const ANALYZE_TIMEFRAME: &str = "Complete order-flow read of one symbol on one timeframe: \
    price, delta, CVD, VWAP, POC/VAH/VAL, structural trend, and the most recent \
    liquidity levels, absorption events and imbalances. This is the main tool: \
    call it before forming any view.";

const ANALYZE_MULTI_TIMEFRAME: &str = "Run analyze_timeframe across several timeframes at \
    once, ordered coarse to fine. Use it to check higher-timeframe context before \
    an entry trigger. Returns one state per timeframe.";

const GET_CANDLES: &str = "Recent OHLCV candles with buy/sell split, oldest first. Use it \
    to see the actual price path, not just the aggregate order-flow read.";

const GET_VOLUME_PROFILE: &str = "Volume profile over the window: point of control (POC), \
    value area high/low (VAH/VAL), total volume, and the highest-volume nodes.";

const GET_FOOTPRINT: &str = "Per-price bid vs ask volume for the most recent candles. \
    Only available when tick data exists for the window; returns an explicit \
    note when it does not.";

const GET_DELTA: &str = "Buy-aggressed minus sell-aggressed volume for the recent candles, \
    and the cumulative total over the window.";

const GET_CVD: &str = "Cumulative volume delta over the window, plus whether CVD and price \
    are diverging. Divergence is computed by the deterministic core, never inferred.";

const GET_VWAP: &str = "Session VWAP at the newest candle and a preview of the recent VWAP \
    series, plus whether price is above it.";

const DETECT_LIQUIDITY: &str = "Resting-liquidity levels (swing highs/lows where stops sit), \
    which of them have been swept, and the nearest levels above and below price. \
    Each level carries `side`: `buy_side` for levels above the market (sweeping \
    them releases buying) and `sell_side` for levels below it (sweeping them \
    releases selling). A long setup wants sell_side swept; a short wants \
    buy_side swept. The `reclaim` block answers whether price is back on the \
    correct side of a swept level (`long_reclaim` / `short_reclaim`) and names \
    the level itself -- cite `reclaimed` and `swept_level` from it rather than \
    comparing the price against a level yourself.";

const DETECT_ABSORPTION: &str = "Absorption events: heavy opposing volume that failed to \
    move price. Requires tick data; reports unavailable when there is none.";

const DETECT_IMBALANCE: &str = "Footprint imbalances: price levels where one side \
    overwhelmed the other. Requires tick data; reports unavailable when there is none.";

const DETECT_MARKET_STRUCTURE: &str = "Confirmed swing highs/lows, the structural trend, \
    and the most recent breaks of structure (BOS/CHoCH).";

const BACKTEST_STRATEGY: &str = "Backtest a Strategy DSL document over a historical window \
    and return headline statistics in R. The document must be complete and valid; \
    validation errors are returned verbatim so you can fix them.";

const BACKTEST_SIMILAR_SETUPS: &str = "Historical base rate for the setups a skill describes: \
    how often they fired and what they returned in R. Call it once you believe a skill's \
    conditions are satisfied, before finalising the thesis.";

const GET_USER_DRAWINGS: &str = "The levels and shapes the user themselves drew on this symbol, \
    with their labels and both anchors. Read-only. Call it before answering anything about \
    'my level', 'my trendline', 'the zone I marked' or whether the user's marks still hold -- \
    a drawn level is the user's own analysis, and citing it beats rediscovering it.";

const CREATE_DRAWING: &str = "Draw an object on the user's chart -- a level, a zone, a trendline, \
    a Fibonacci retracement. Use it when the analysis produced something worth seeing: a \
    resistance zone, a fair-value gap, the entry and stop of a setup. Anchors are absolute \
    (epoch milliseconds and price), which you can get from get_candles. Always give a reason: \
    it is stored with the object and the user can read it. Mark created objects are recorded \
    as yours, and the user can hide or delete them like any other drawing.";

const UPDATE_DRAWING: &str = "Move, resize or relabel one drawing you or the user placed, by id. \
    Use it to tighten an object to the data -- 'make the zone cover the whole rejection area' -- \
    rather than deleting and redrawing, which loses the object's id and history. Pass only the \
    anchors you want after the move; the full shape is replaced.";

const DELETE_DRAWING: &str = "Remove one drawing, by id, when the analysis says it no longer \
    holds or the user asked for its removal. Prefer update_drawing when the object is only \
    misplaced. There is no undo for the agent: if the drawing was the user's own work, say so \
    and prefer to leave it.";

const REMEMBER: &str = "Store a fact about this user or market so you still know it next \
    conversation: a level that mattered and why, the user's risk preferences, their plan. \
    Stating the same key again replaces what you said before -- memory is what you currently \
    stand behind, not a chat log. You do not need this for the thesis levels themselves; \
    those are kept automatically. Use it for what would not otherwise be written down.";

const RECALL_MEMORIES: &str = "Re-read what you know about this user and symbol. Your newest \
    memories are already placed in your context before the first turn; call this when you \
    suspect something relevant was stored beyond what is shown there, or after storing \
    several facts and wanting to see the current set.";

const FORGET_MEMORY: &str = "Drop one stored fact, by key, when you can see it is no longer \
    true -- a level that was broken and confirmed, a preference the user has revised. \
    Forgetting a stale fact is better than contradicting it every session.";

/// Absence-of-tick-data message, shared so every affected tool says the same
/// thing. The model needs to distinguish "no events occurred" from "events
/// could not be detected here" -- otherwise it will report a clean bill of
/// health on a window it never actually looked at.
const NO_TICK_DATA: &str = "no tick data in this window: per-price footprint signals cannot \
    be computed. This is not evidence that none occurred. Use get_candles, get_delta and \
    the candle-derived volume profile instead.";

async fn analyze_timeframe(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "analyze_timeframe";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (state, _, trades) = state_for(ctx, &symbol, timeframe, lookback).await?;
    Ok(render_state(&state, trades.is_empty()))
}

async fn analyze_multi_timeframe(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "analyze_multi_timeframe";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let raw = args
        .get("timeframes")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "`timeframes` must be an array of strings, e.g. [\"4h\", \"1h\", \"5m\"]"
                .into(),
        })?;

    let mut timeframes = Vec::new();
    for value in raw {
        let text = value.as_str().ok_or_else(|| AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: format!("`timeframes` entries must be strings, got {value}"),
        })?;
        timeframes.push(
            text.parse::<Timeframe>()
                .map_err(|_| AgentError::InvalidToolArgs {
                    tool: TOOL.into(),
                    reason: format!("unknown timeframe `{text}`"),
                })?,
        );
    }
    if timeframes.is_empty() {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "`timeframes` must not be empty".into(),
        });
    }

    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let mut frames = Vec::new();
    for timeframe in timeframes {
        match state_for(ctx, &symbol, timeframe, lookback).await {
            Ok((state, _, trades)) => frames.push(render_state(&state, trades.is_empty())),
            // One missing timeframe must not sink the whole ladder: the higher
            // timeframes are still useful context. The gap is reported inline
            // so the model can see what it is missing.
            Err(e) => frames.push(json!({
                "timeframe": timeframe.to_string(),
                "available": false,
                "reason": e.to_string(),
            })),
        }
    }
    Ok(json!({ "symbol": symbol, "frames": frames }))
}

async fn get_candles(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_candles";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let limit = ctx.clamp_lookback(optional_u64(args, "limit", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, limit).await?;
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "count": candles.len(),
        "candles": candles.iter().map(candle_json).collect::<Vec<_>>(),
    }))
}

async fn get_volume_profile(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_volume_profile";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;

    // Both branches call analytics-core; this only chooses the input the
    // profile is built from, never how it is computed.
    let profile = if trades.is_empty() {
        calculate_volume_profile_from_candles(&candles, ctx.config.bucket_size)
    } else {
        analytics_core::calculate_volume_profile(&trades, ctx.config.bucket_size)
    };
    let price = candles.last().map_or(0.0, |c| c.close);

    // Only the nodes that matter: a 4h window on 1m data can produce hundreds
    // of buckets, and the model cannot use most of them.
    let top_nodes = top_nodes(&profile.histogram, 12);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "poc": profile.poc,
        "vah": profile.vah,
        "val": profile.val,
        "total_volume": profile.total_volume,
        "price": price,
        "in_value_area": price >= profile.val && price <= profile.vah,
        "high_volume_nodes": profile.hvn,
        "low_volume_nodes": profile.lvn,
        "top_nodes": top_nodes,
        "source": if trades.is_empty() { "candles" } else { "trades" },
    }))
}

async fn get_footprint(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_footprint";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    // Footprints are verbose; 20 is plenty and keeps the result readable.
    let count = ctx
        .clamp_lookback(optional_u64(args, "count", TOOL)?)
        .min(20);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, count).await?;

    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let footprints = recent_footprints(
        &candles,
        &trades,
        ctx.config.bucket_size,
        ctx.config.footprint_candles,
    );
    let rendered: Vec<_> = footprints
        .iter()
        .map(|footprint| {
            let mut cells: Vec<&analytics_core::FootprintCell> = footprint.cells.iter().collect();
            cells.sort_by(|a, b| {
                b.total_volume()
                    .partial_cmp(&a.total_volume())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            json!({
                "open_time": footprint.candle.open_time,
                "high": footprint.candle.high,
                "low": footprint.candle.low,
                "close": footprint.candle.close,
                "delta": footprint.candle.delta(),
                "levels": cells.into_iter().take(12).map(|cell| json!({
                    "price": cell.price_level,
                    "bid_volume": cell.bid_volume,
                    "ask_volume": cell.ask_volume,
                    "delta": cell.delta,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": true,
        "candles": rendered,
    }))
}

async fn get_delta(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_delta";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx
        .clamp_lookback(optional_u64(args, "lookback", TOOL)?)
        .min(50);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let series: Vec<_> = candles
        .iter()
        .map(|c| {
            json!({
                "open_time": c.open_time,
                "close": c.close,
                "delta": c.delta(),
                "buy_volume": c.buy_volume,
                "sell_volume": c.sell_volume,
            })
        })
        .collect();

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "latest_delta": candles.last().map_or(0.0, Candle::delta),
        "window_delta": candles.iter().map(Candle::delta).sum::<f64>(),
        "candles": series,
    }))
}

async fn get_cvd(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_cvd";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (state, candles, _) = state_for(ctx, &symbol, timeframe, lookback).await?;
    let series = analytics_core::calculate_cvd(&candles, ctx.config.cvd_reset_at_session);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "cvd": state.cvd,
        "price": state.price,
        "divergence": format!("{:?}", state.divergence),
        "series_preview": series.iter().rev().take(20).copied().collect::<Vec<_>>(),
    }))
}

async fn get_vwap(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_vwap";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let series =
        analytics_core::vwap::calculate_vwap_series(&candles, ctx.config.cvd_reset_at_session);
    let candle_vwap = series.last().copied().flatten();
    let price = candles.last().map_or(0.0, |c| c.close);

    // With trades in the window the true VWAP is trade-weighted; without them
    // the best available answer is the candle-typical-price series. Both come
    // from analytics-core -- this only picks which one to report.
    let trade_vwap = if trades.is_empty() {
        None
    } else {
        analytics_core::calculate_vwap_from_trades(&trades)
    };
    let vwap = trade_vwap.or(candle_vwap);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "vwap": vwap,
        "price": price,
        "above_vwap": vwap.map(|v| price > v),
        "source": if trade_vwap.is_some() { "trades" } else { "candle_typical_price" },
        "series_preview": series.iter().rev().take(20).map(|v| v.unwrap_or(0.0)).collect::<Vec<_>>(),
    }))
}

async fn detect_liquidity(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_liquidity";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let levels = detect_liquidity_levels_with(&candles, ctx.config.liquidity);
    let price = candles.last().map_or(0.0, |c| c.close);
    let rendered: Vec<_> = levels
        .iter()
        .map(|level| {
            json!({
                "price": level.price,
                "kind": format!("{:?}", level.kind),
                // Which side's liquidity this is, stated outright rather than
                // left for the model to infer from `kind`. A live run had it
                // report a swept *high* as "sell-side liquidity swept", which
                // is the opposite of the truth and of what the skill asked
                // for: highs hold buy-side liquidity, lows hold sell-side.
                "side": if level.kind.is_above() { "buy_side" } else { "sell_side" },
                "above_market": level.kind.is_above(),
                "touches": level.touches,
                "swept": level.swept,
            })
        })
        .collect();

    let above = analytics_core::liquidity::nearest_liquidity_above(&levels, price);
    let below = analytics_core::liquidity::nearest_liquidity_below(&levels, price);

    // The reclaim verdict is computed here rather than left to the model.
    // A live run reported "price 77221.1 > swept level 77290.0" -- a
    // comparison it got backwards -- and the reclaim is the condition the
    // whole setup rests on, so it is not something prose should decide.
    let reclaim = reclaim_verdict(&levels, price);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "price": price,
        "count": levels.len(),
        "levels": rendered,
        "nearest_above": above.map(|l| json!({"price": l.price, "swept": l.swept})),
        "nearest_below": below.map(|l| json!({"price": l.price, "swept": l.swept})),
        "reclaim": reclaim,
    }))
}

/// Whether price has reclaimed a swept level, per direction.
///
/// A long setup needs sell-side liquidity swept and price back **above** it;
/// a short needs buy-side swept and price back below. `swept_level` is the
/// nearest such level on the correct side, so it doubles as the level a stop
/// belongs beyond.
fn reclaim_verdict(levels: &[analytics_core::liquidity::LiquidityLevel], price: f64) -> Value {
    // Highest swept sell-side level below price / lowest swept buy-side above.
    let long_level = levels
        .iter()
        .filter(|l| l.kind.is_below() && l.swept && l.price < price)
        .map(|l| l.price)
        .reduce(f64::max);
    let short_level = levels
        .iter()
        .filter(|l| l.kind.is_above() && l.swept && l.price > price)
        .map(|l| l.price)
        .reduce(f64::min);

    json!({
        "price": price,
        "long_reclaim": {
            "reclaimed": long_level.is_some(),
            "swept_level": long_level,
            "side": "sell_side",
        },
        "short_reclaim": {
            "reclaimed": short_level.is_some(),
            "swept_level": short_level,
            "side": "buy_side",
        },
        "note": "Computed here, not by you: cite `reclaimed` and `swept_level` \
                 directly. Do not compare the price against a level yourself.",
    })
}

async fn detect_absorption_tool(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_absorption";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (state, candles, trades) = state_for(ctx, &symbol, timeframe, lookback).await?;

    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let footprints = recent_footprints(
        &candles,
        &trades,
        ctx.config.bucket_size,
        ctx.config.footprint_candles,
    );
    let events = detect_absorption(&footprints, ctx.config.absorption);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": true,
        "count": events.len(),
        "events": events.iter().rev().take(10).map(|e| json!({
            "price_level": e.price_level,
            "side": format!("{:?}", e.side),
            "volume": e.volume,
            "strength": e.strength,
            "delta_improvement": e.delta_improvement,
            "timestamp": e.timestamp,
        })).collect::<Vec<_>>(),
        "latest": state.latest_absorption().map(|e| json!({
            "price_level": e.price_level,
            "side": format!("{:?}", e.side),
            "strength": e.strength,
        })),
    }))
}

async fn detect_imbalance(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_imbalance";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (state, candles, trades) = state_for(ctx, &symbol, timeframe, lookback).await?;

    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let footprints = recent_footprints(
        &candles,
        &trades,
        ctx.config.bucket_size,
        ctx.config.footprint_candles,
    );
    let mut events = Vec::new();
    for footprint in &footprints {
        events.extend(detect_imbalances_with(footprint, &ctx.config.imbalance));
    }

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": true,
        "count": events.len(),
        "net_imbalance_volume": state.net_imbalance_volume(),
        "events": events.iter().rev().take(15).map(|e| json!({
            "price_level": e.price_level,
            "side": format!("{:?}", e.side),
            "ratio": e.ratio,
            "volume": e.volume,
            "stacked": e.stacked,
        })).collect::<Vec<_>>(),
    }))
}

async fn detect_market_structure_tool(
    ctx: &ToolContext<'_>,
    args: &Value,
) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_market_structure";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;
    let structure = detect_market_structure(&candles, ctx.config.structure);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "trend": format!("{:?}", structure.trend),
        "swing_highs": structure.swing_highs.iter().rev().take(8).copied().collect::<Vec<_>>(),
        "swing_lows": structure.swing_lows.iter().rev().take(8).copied().collect::<Vec<_>>(),
        "recent_breaks": structure.breaks.iter().rev().take(8).map(|b| json!({
            "kind": format!("{:?}", b.kind),
            "direction": format!("{:?}", b.direction),
            "level": b.level,
            "price": b.price,
        })).collect::<Vec<_>>(),
    }))
}

async fn backtest_strategy(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "backtest_strategy";
    let document = string_arg(args, "document", TOOL)?;
    let symbol = string_arg(args, "symbol", TOOL)?;
    let days = optional_u64(args, "days", TOOL)?
        .unwrap_or(180)
        .clamp(1, 3650);
    let runner = ctx.backtests.ok_or_else(|| AgentError::ToolFailed {
        tool: TOOL.into(),
        reason: "no backtest runner is attached to this agent".into(),
    })?;

    let (from, to) = trailing_days(days);
    let summary = runner.run(&document, &symbol, from, to).await?;
    Ok(serde_json::to_value(summary)?)
}

async fn backtest_similar_setups(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "backtest_similar_setups";
    let skill_ref = string_arg(args, "skill_ref", TOOL)?;
    let symbol = string_arg(args, "symbol", TOOL)?;
    let days = optional_u64(args, "days", TOOL)?
        .unwrap_or(180)
        .clamp(1, 3650);
    let runner = ctx.backtests.ok_or_else(|| AgentError::ToolFailed {
        tool: TOOL.into(),
        reason: "no backtest runner is attached to this agent".into(),
    })?;

    let (from, to) = trailing_days(days);
    let summary = runner.similar_setups(&skill_ref, &symbol, from, to).await?;
    Ok(serde_json::to_value(summary)?)
}

/// `get_user_drawings`: what the user marked on this symbol, read-only.
///
/// ## Why a tool and not only the viewport packet
///
/// The chart packet carries a flattened one-price-per-drawing summary of
/// whatever was on screen when the question was typed; it is a *hint* about a
/// viewport and is clamped hard. This tool is the grounded read: both anchors,
/// the label, and the current stored state — including drawings made before
/// this session, which the packet has never carried. It exists so the answer
/// to "is my level still holding?" can cite the user's own line instead of
/// rediscovering it and calling it new.
///
/// ## The two refusals say different things
///
/// "No drawings source" is about the *host* — nothing is attached, so the
/// honest answer is that the agent cannot see any drawing, not that the chart
/// is bare. "No drawings" is about the *chart* — storage answered, and this
/// user has marked nothing on this symbol. Reading the first as the second is
/// how an agent ends up confidently describing analysis it never saw.
async fn get_user_drawings(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_user_drawings";
    let symbol = string_arg(args, "symbol", TOOL)?;

    let (Some(source), Some(user_id)) = (ctx.drawings, ctx.user_id) else {
        return Err(AgentError::ToolFailed {
            tool: TOOL.into(),
            reason: "no drawings source is attached to this agent, so user \
                     drawings cannot be read. Say so; do not assume the chart \
                     is bare."
                .into(),
        });
    };

    let mut drawings = source.drawings(user_id, &symbol).await?;
    let total =
        crate::user_drawings::clamp_drawings(&mut drawings, crate::chart_context::MAX_DRAWINGS);
    let returned = drawings.len();

    // `price` on each entry is what folds into the grounding range
    // (`PriceRange` walks `PRICE_KEYS`, and `price` is one), so a level the
    // user drew is a level the thesis may be built against — which is the
    // point of the tool.
    let levels: Vec<Value> = drawings
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(serde_json::Value::Null))
        .collect();

    let mut out = json!({
        "symbol": symbol,
        "read_only": true,
        "count": returned,
        "drawings": levels,
    });
    if total > returned {
        out["total"] = json!(total);
        out["note"] = json!(format!(
            "showing the {returned} oldest of {total} drawings; the rest were \
             dropped to keep the answer readable"
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The write path (docs/21). Three tools, one reader, one writer, one rule:
// the host decides whether writes exist at all, and a host that said no gets
// told the truth rather than worked around.
// ---------------------------------------------------------------------------

/// The writer and the identity to write as, or the message a write tool
/// reports when the host attached none.
fn drawing_writer<'a>(
    ctx: &'a ToolContext<'_>,
    tool: &str,
) -> Result<(&'a dyn DrawingWriter, &'a str), AgentError> {
    let (Some(writer), Some(user_id)) = (ctx.drawing_writer, ctx.user_id) else {
        return Err(AgentError::ToolFailed {
            tool: tool.into(),
            reason: "no drawing writer is attached to this agent, so the chart \
                     cannot be changed from here. Present the analysis as text \
                     instead; do not pretend the object was drawn."
                .into(),
        });
    };
    Ok((writer, user_id))
}

/// A `NewAgentDrawing` from the model's arguments, or a correctable error.
///
/// The anchor checks mirror what storage will refuse, stated here because the
/// failure message is the model's only feedback: "a2 needs both time and
/// price" is actionable, a storage-layer `CHECK` violation is not.
fn new_drawing_args(args: &Value, tool: &str) -> Result<NewAgentDrawing, AgentError> {
    let kind = string_arg(args, "kind", tool)?;
    let symbol_independent_label = args
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let num = |key: &str| -> Result<f64, AgentError> {
        args.get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| AgentError::InvalidToolArgs {
                tool: tool.into(),
                reason: format!("`{key}` is required and must be a number"),
            })
    };
    let opt_num = |key: &str| -> Result<Option<f64>, AgentError> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .map(Some)
                .ok_or_else(|| AgentError::InvalidToolArgs {
                    tool: tool.into(),
                    reason: format!("`{key}` must be a number when given"),
                }),
        }
    };

    let time1_ms = num("time1_ms")?;
    let price1 = num("price1")?;
    let time2_ms = opt_num("time2_ms")?;
    let price2 = opt_num("price2")?;
    if (time2_ms.is_some()) != (price2.is_some()) {
        return Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "a second anchor needs both `time2_ms` and `price2`, or neither -- \
                     a half-drawn anchor has no meaning"
                .into(),
        });
    }
    let time3_ms = opt_num("time3_ms")?;
    let price3 = opt_num("price3")?;
    // The third anchor obeys the same whole-or-absent rule. The *which kinds*
    // half is the engine's `validate_anchors` one layer up; refusing a broken
    // pair here keeps the message about the shape rather than the rule.
    if (time3_ms.is_some()) != (price3.is_some()) {
        return Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "a third anchor needs both `time3_ms` and `price3`, or neither".into(),
        });
    }
    for (name, value) in [
        ("time1_ms", time1_ms),
        ("price1", price1),
        ("time2_ms", time2_ms.unwrap_or_default()),
        ("price2", price2.unwrap_or_default()),
        ("time3_ms", time3_ms.unwrap_or_default()),
        ("price3", price3.unwrap_or_default()),
    ] {
        if value.is_finite() {
            continue;
        }
        return Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: format!("`{name}` must be a finite number"),
        });
    }
    // A confidence the model invented at 1.4 is a claim nobody can read; clamp
    // the schema's own range here so the stored number is always 0..=1.
    let confidence = args
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|c| c.clamp(0.0, 1.0));
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let provenance = if confidence.is_some() || reason.is_some() {
        Some(crate::user_drawings::DrawingProvenance { confidence, reason })
    } else {
        None
    };

    Ok(NewAgentDrawing {
        kind,
        label: symbol_independent_label,
        time1_ms,
        price1,
        time2_ms,
        price2,
        time3_ms,
        price3,
        provenance,
    })
}

/// The answer a successful create returns, shaped for the model.
fn created_answer(stored: &crate::user_drawings::StoredDrawing) -> Value {
    json!({
        "created": true,
        "id": stored.id,
        "symbol": stored.symbol,
        "kind": stored.kind,
        "note": "the object is on the user's chart and recorded as drawn by you. \
                 Quote this id if you later move or remove it.",
    })
}

async fn create_drawing(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "create_drawing";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let drawing = new_drawing_args(args, TOOL)?;
    let (writer, user_id) = drawing_writer(ctx, TOOL)?;
    let stored = writer.create(user_id, &symbol, &drawing).await?;
    Ok(created_answer(&stored))
}

async fn update_drawing(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "update_drawing";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let id = string_arg(args, "id", TOOL)?;
    let drawing = new_drawing_args(args, TOOL)?;
    let (writer, user_id) = drawing_writer(ctx, TOOL)?;
    let updated = writer.update(user_id, &symbol, &id, &drawing).await?;
    if updated {
        Ok(json!({
            "updated": true,
            "id": id,
            "note": "the drawing now has the anchors you sent",
        }))
    } else {
        Ok(json!({
            "updated": false,
            "id": id,
            "note": "no drawing with that id on this symbol for this user. Call \
                     get_user_drawings for the current list -- the id may be stale.",
        }))
    }
}

async fn delete_drawing(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "delete_drawing";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let id = string_arg(args, "id", TOOL)?;
    let (writer, user_id) = drawing_writer(ctx, TOOL)?;
    let deleted = writer.delete(user_id, &symbol, &id).await?;
    if deleted {
        Ok(json!({"deleted": true, "id": id}))
    } else {
        Ok(json!({
            "deleted": false,
            "id": id,
            "note": "nothing to delete: there is no drawing with that id on this \
                     symbol for this user. If it was already gone, the requested \
                     state is already true."
        }))
    }
}

/// What the memory tools need from the context: a source, optionally a
/// writer, and whose memory it is.
type MemoryParts<'a> = (&'a dyn MemorySource, Option<&'a dyn MemoryWriter>, &'a str);

/// The memory tools' shared refusal. Same posture as the drawings tools: a
/// missing source is a *host* fact, reported so the model can say the feature
/// is unavailable here rather than reading silence as an empty memory.
fn memory_parts<'a>(ctx: &'a ToolContext<'_>, tool: &str) -> Result<MemoryParts<'a>, AgentError> {
    match (ctx.memory, ctx.memory_writer, ctx.memory_user_id) {
        (Some(source), writer, Some(user_id)) => Ok((source, writer, user_id)),
        _ => Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "no memory source is attached to this agent, so facts cannot \
                     be stored or read. Say so; do not pretend to remember."
                .into(),
        }),
    }
}

/// `remember`: store one fact under this user's name, newest-wins by key.
async fn remember(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "remember";
    let scope = string_arg(args, "scope", TOOL)?;
    let key = string_arg(args, "key", TOOL)?;
    let content = string_arg(args, "content", TOOL)?;
    let symbol = match args.get("symbol") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_uppercase()),
        Some(_) => {
            return Err(AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: "`symbol` must be a non-empty string when given".into(),
            })
        }
    };
    // The scope shape is checked *here*, in the model's vocabulary, before the
    // builder's identical check -- so a wrong shape returns a message about
    // scopes rather than one about symbols.
    if scope == "symbol" && symbol.is_none() {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "a symbol-scoped fact needs `symbol`; use scope `global` for \
                     preferences about this user"
                .into(),
        });
    }
    if scope == "global" && symbol.is_some() {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "a global fact does not carry a symbol; put the fact's market \
                     in the content if it has one"
                .into(),
        });
    }
    if scope != "symbol" && scope != "global" {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "`scope` must be `symbol` or `global`".into(),
        });
    }
    let (_source, Some(writer), user_id) = memory_parts(ctx, TOOL)? else {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "this agent can read memories but not store them: no memory \
                     writer is attached. Say so rather than implying the fact \
                     was kept."
                .into(),
        });
    };
    let fact = NewMemory::checked_for_tool(TOOL, &scope, symbol, key, content)?;
    writer.remember(user_id, &fact).await?;
    Ok(json!({
        "stored": true,
        "scope": fact.scope,
        "key": fact.key,
        "note": "the fact is stored under this user's name and will be recalled \
                 in future conversations on this symbol (and every symbol, for \
                 global facts)."
    }))
}

/// `recall_memories`: the user's newest facts, newest first.
async fn recall_memories(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "recall_memories";
    let symbol = string_arg(args, "symbol", TOOL)?.to_uppercase();
    let limit = optional_u64(args, "limit", TOOL)?
        .unwrap_or(crate::agent_memory::RECALL_LIMIT as u64) as usize;
    let (source, _writer, user_id) = memory_parts(ctx, TOOL)?;
    let rows = source.recall(user_id, &symbol, limit).await?;
    let facts: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "scope": row.scope,
                "symbol": row.symbol,
                "key": row.key,
                "content": row.content,
                "updated_at_ns": row.updated_at,
            })
        })
        .collect();
    Ok(json!({
        "count": facts.len(),
        "memories": facts,
        "note": "newest first. These are claims from earlier conversations -- \
                 re-derive levels from tools before acting on them."
    }))
}

/// `forget_memory`: drop one fact by key, honestly reporting a miss.
async fn forget_memory(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "forget_memory";
    let key = string_arg(args, "key", TOOL)?;
    let symbol = match args.get("symbol") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_uppercase()),
        Some(_) => {
            return Err(AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: "`symbol` must be a non-empty string when given".into(),
            })
        }
    };
    let (_source, Some(writer), user_id) = memory_parts(ctx, TOOL)? else {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "this agent can read memories but not drop them: no memory \
                     writer is attached."
                .into(),
        });
    };
    let deleted = writer.forget(user_id, symbol.as_deref(), &key).await?;
    if deleted {
        Ok(json!({"forgotten": true, "key": key}))
    } else {
        Ok(json!({
            "forgotten": false,
            "key": key,
            "note": "no fact under that key for this scope. If it was already \
                     gone, the requested state is already true."
        }))
    }
}

/// `[from, to)` covering the last `days` days, ending now.
fn trailing_days(days: u64) -> (i64, i64) {
    const NS_PER_DAY: i64 = 86_400 * 1_000_000_000;
    let to = now_ns();
    let span = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_mul(NS_PER_DAY);
    (to.saturating_sub(span), to)
}

/// Current wall-clock time in unix nanos.
fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn candle_json(candle: &Candle) -> Value {
    json!({
        "t": candle.open_time,
        "o": candle.open,
        "h": candle.high,
        "l": candle.low,
        "c": candle.close,
        "v": candle.volume,
        "bv": candle.buy_volume,
        "sv": candle.sell_volume,
    })
}

/// The `limit` highest-volume nodes, highest first.
fn top_nodes(histogram: &[VolumeNode], limit: usize) -> Vec<Value> {
    let mut nodes: Vec<&VolumeNode> = histogram.iter().collect();
    nodes.sort_by(|a, b| {
        b.volume
            .partial_cmp(&a.volume)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    nodes
        .into_iter()
        .take(limit)
        .map(|node| {
            json!({
                "price": node.price_level,
                "volume": node.volume,
                "buy_volume": node.buy_volume,
                "sell_volume": node.sell_volume,
            })
        })
        .collect()
}

/// Render a `MarketState` for the model.
///
/// The event lists are capped on purpose. A 300-bar window can surface hundreds
/// of imbalances; pasting them all in would bury the numbers that actually
/// matter and burn the context window doing it.
fn render_state(state: &MarketState, no_trades: bool) -> Value {
    let mut value = json!({
        "symbol": state.symbol,
        "timeframe": state.timeframe,
        "timestamp": state.timestamp,
        "price": state.price,
        "delta": state.delta,
        "cvd": state.cvd,
        "vwap": state.vwap,
        "above_vwap": state.above_vwap(),
        "poc": state.poc,
        "vah": state.vah,
        "val": state.val,
        "in_value_area": state.in_value_area(),
        "volume": state.volume,
        "buy_volume": state.buy_volume,
        "sell_volume": state.sell_volume,
        "divergence": format!("{:?}", state.divergence),
        "trend": format!("{:?}", state.trend),
        "net_imbalance_volume": state.net_imbalance_volume(),
        "swing_highs": state.swing_highs.iter().rev().take(6).copied().collect::<Vec<_>>(),
        "swing_lows": state.swing_lows.iter().rev().take(6).copied().collect::<Vec<_>>(),
        "liquidity": state.liquidity.iter().rev().take(10).map(|l| json!({
            "price": l.price,
            "kind": format!("{:?}", l.kind),
            "touches": l.touches,
            "swept": l.swept,
        })).collect::<Vec<_>>(),
        "nearest_liquidity_above": state.nearest_liquidity_above().map(|l| l.price),
        "nearest_liquidity_below": state.nearest_liquidity_below().map(|l| l.price),
        "absorption": state.absorption.iter().rev().take(6).map(|e| json!({
            "price_level": e.price_level,
            "side": format!("{:?}", e.side),
            "strength": e.strength,
            "volume": e.volume,
            "delta_improvement": e.delta_improvement,
            "timestamp": e.timestamp,
        })).collect::<Vec<_>>(),
        "imbalances": state.imbalances.iter().rev().take(10).map(|e| json!({
            "price_level": e.price_level,
            "side": format!("{:?}", e.side),
            "ratio": e.ratio,
            "stacked": e.stacked,
        })).collect::<Vec<_>>(),
    });

    if no_trades {
        value["data_note"] = json!(NO_TICK_DATA);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{ContentBlock, Message, Role};

    /// A fixed data source: rising candles, no trades.
    struct Fixture {
        candles: Vec<Candle>,
        latest: i64,
    }

    impl Fixture {
        fn rising(count: usize) -> Self {
            let candles: Vec<Candle> = (0..count)
                .map(|i| Candle {
                    symbol: "BTCUSDT".into(),
                    timeframe: Timeframe::M5,
                    open_time: i64::try_from(i).unwrap() * Timeframe::M5.nanos(),
                    open: 100.0 + i as f64,
                    high: 101.0 + i as f64,
                    low: 99.0 + i as f64,
                    close: 100.5 + i as f64,
                    volume: 10.0,
                    buy_volume: 6.0,
                    sell_volume: 4.0,
                })
                .collect();
            Self::from(candles)
        }

        /// A triangle wave: swings in both directions, unlike `rising`.
        ///
        /// Anything that needs swings to exist -- liquidity levels, market
        /// structure -- has to use this. A strictly monotonic series confirms
        /// no local extrema at all, so it yields nothing to assert against.
        /// The period is 8 bars with a 3-bar confirmation window, which is the
        /// default `LiquidityConfig::lookback`, so every peak and trough is
        /// confirmed with room to spare.
        fn oscillating(count: usize) -> Self {
            const PERIOD: usize = 8;
            const AMPLITUDE: f64 = 10.0;

            let candles: Vec<Candle> = (0..count)
                .map(|i| {
                    let phase = i % PERIOD;
                    let step = if phase <= PERIOD / 2 {
                        phase
                    } else {
                        PERIOD - phase
                    };
                    let price = 100.0 + f64::from(u8::try_from(step).unwrap()) * AMPLITUDE;
                    Candle {
                        symbol: "BTCUSDT".into(),
                        timeframe: Timeframe::M5,
                        open_time: i64::try_from(i).unwrap() * Timeframe::M5.nanos(),
                        open: price,
                        high: price + 1.0,
                        low: price - 1.0,
                        close: price + 0.5,
                        volume: 10.0,
                        buy_volume: 6.0,
                        sell_volume: 4.0,
                    }
                })
                .collect();
            Self::from(candles)
        }

        fn from(candles: Vec<Candle>) -> Self {
            let latest = candles.last().map_or(0, |c| c.open_time);
            Self { candles, latest }
        }
    }

    #[async_trait]
    impl MarketDataSource for Fixture {
        async fn candles(
            &self,
            _symbol: &str,
            _timeframe: Timeframe,
            from_ns: i64,
            to_ns: i64,
        ) -> Result<Vec<Candle>, AgentError> {
            Ok(self
                .candles
                .iter()
                .filter(|c| c.open_time >= from_ns && c.open_time < to_ns)
                .cloned()
                .collect())
        }

        async fn trades(
            &self,
            _symbol: &str,
            _from_ns: i64,
            _to_ns: i64,
        ) -> Result<Vec<Trade>, AgentError> {
            Ok(Vec::new())
        }

        async fn latest_candle_time(
            &self,
            _symbol: &str,
            _timeframe: Timeframe,
        ) -> Result<Option<i64>, AgentError> {
            Ok(Some(self.latest))
        }
    }

    /// A source with nothing stored at all.
    struct Empty;

    #[async_trait]
    impl MarketDataSource for Empty {
        async fn candles(
            &self,
            _symbol: &str,
            _timeframe: Timeframe,
            _from_ns: i64,
            _to_ns: i64,
        ) -> Result<Vec<Candle>, AgentError> {
            Ok(Vec::new())
        }
        async fn trades(
            &self,
            _symbol: &str,
            _from_ns: i64,
            _to_ns: i64,
        ) -> Result<Vec<Trade>, AgentError> {
            Ok(Vec::new())
        }
        async fn latest_candle_time(
            &self,
            _symbol: &str,
            _timeframe: Timeframe,
        ) -> Result<Option<i64>, AgentError> {
            Ok(None)
        }
    }

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: name.into(),
            input,
        }
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::market_analysis()
    }

    #[tokio::test]
    async fn the_registry_exposes_every_tool_documented_in_docs_09() {
        let registry = ToolRegistry::market_analysis();
        for name in [
            "get_candles",
            "get_footprint",
            "get_volume_profile",
            "get_delta",
            "get_cvd",
            "get_vwap",
            "detect_liquidity",
            "detect_absorption",
            "detect_imbalance",
            "detect_market_structure",
            "analyze_timeframe",
            "analyze_multi_timeframe",
            "backtest_strategy",
            "backtest_similar_setups",
            "get_user_drawings",
        ] {
            assert!(registry.contains(name), "missing tool {name}");
        }
    }

    /// Every advertised tool must have a dispatch arm. Without this, a tool
    /// could be visible to the model and unroutable at the same time.
    #[tokio::test]
    async fn every_registered_tool_is_dispatchable() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let registry = ToolRegistry::market_analysis();

        for name in registry.names() {
            let err = registry
                .execute(&call(name, json!({})), &ctx)
                .await
                .unwrap_err();
            assert!(
                !matches!(err, AgentError::UnknownTool(_)),
                "{name} is advertised but has no dispatch arm"
            );
        }
    }

    #[tokio::test]
    async fn every_schema_is_a_json_object_with_a_description() {
        for spec in ToolRegistry::market_analysis().specs() {
            assert_eq!(spec.input_schema["type"], "object", "{} schema", spec.name);
            assert!(
                spec.input_schema["required"].is_array(),
                "{} has no required list",
                spec.name
            );
            assert!(
                !spec.description.is_empty(),
                "{} has no description",
                spec.name
            );
        }
    }

    #[test]
    fn every_schema_advertises_every_timeframe_the_engine_accepts() {
        // The schema is the model's only idea of what exists. A hand-written
        // enum here once lagged the engine by a whole resolution: `1w` was
        // charted end to end while the model was still being told weekly did
        // not exist, and a model that obeys its schema never tries what the
        // schema forbids. So the enums are built from `Timeframe::all()` --
        // and this test pins the invariant directly by walking every schema's
        // JSON tree and comparing each timeframe enum against the engine's own
        // list. A timeframe enum is recognised by shape (it carries the `1m`
        // and `4h` rungs), not by tool name, so the check cannot go stale when
        // a tool is added.
        // Fine-to-coarse, the order a model reads a ladder in and the order the
        // schemas have always advertised. Derived from the type, not from the
        // schema helpers, so the comparison is not the function checking itself.
        let expected: Vec<Value> = Timeframe::all()
            .iter()
            .rev()
            .map(|tf| Value::from(tf.as_str()))
            .collect();

        fn collect_enums(value: &Value, out: &mut Vec<Vec<Value>>) {
            match value {
                Value::Object(map) => {
                    if let Some(Value::Array(items)) = map.get("enum") {
                        out.push(items.clone());
                    }
                    for v in map.values() {
                        collect_enums(v, out);
                    }
                }
                Value::Array(items) => {
                    for v in items {
                        collect_enums(v, out);
                    }
                }
                _ => {}
            }
        }

        let mut saw_timeframe_enum = false;
        for spec in ToolRegistry::market_analysis().specs() {
            let mut found = Vec::new();
            collect_enums(&spec.input_schema, &mut found);
            for items in &found {
                let is_timeframe_enum =
                    items.contains(&Value::from("1m")) && items.contains(&Value::from("4h"));
                if is_timeframe_enum {
                    saw_timeframe_enum = true;
                    assert_eq!(
                        items, &expected,
                        "{} schema's timeframe enum drifted from `Timeframe::all()`",
                        spec.name
                    );
                }
            }
        }
        assert!(
            saw_timeframe_enum,
            "no schema advertises timeframes any more; the check has gone blind"
        );
    }

    #[tokio::test]
    async fn analyze_timeframe_returns_real_numbers_from_the_core() {
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture);
        let out = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "analyze_timeframe",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["symbol"], "BTCUSDT");
        assert_eq!(out["timeframe"], "5m");
        // 60 candles, each +2 volume delta -> CVD 120.
        assert!(
            (out["cvd"].as_f64().unwrap() - 120.0).abs() < 1e-9,
            "cvd={}",
            out["cvd"]
        );
        assert!(out["poc"].as_f64().unwrap() > 0.0);
        assert!(out["price"].as_f64().unwrap() > 0.0);
        // No trades in the fixture, so the note must be present.
        assert!(out["data_note"].is_string());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_as_such() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = ToolRegistry::market_analysis()
            .execute(&call("make_me_money", json!({})), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::UnknownTool(_)), "got {err}");
    }

    #[tokio::test]
    async fn a_bad_timeframe_produces_a_recoverable_message() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "analyze_timeframe",
                    json!({"symbol": "BTCUSDT", "timeframe": "7m"}),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        match err {
            AgentError::InvalidToolArgs { reason, .. } => {
                assert!(
                    reason.contains("1m"),
                    "reason should list valid values: {reason}"
                );
            }
            other => panic!("expected InvalidToolArgs, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_missing_symbol_is_rejected_before_any_query() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = ToolRegistry::market_analysis()
            .execute(&call("analyze_timeframe", json!({"timeframe": "5m"})), &ctx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AgentError::InvalidToolArgs { .. }),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn no_data_is_an_error_the_model_can_act_on() {
        let empty = Empty;
        let ctx = ToolContext::new(&empty);
        let err = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "analyze_timeframe",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::NoData { .. }), "got {err}");
    }

    #[tokio::test]
    async fn get_candles_is_capped_and_ordered_oldest_first() {
        let fixture = Fixture::rising(500);
        let ctx = ToolContext::new(&fixture);
        let out = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "get_candles",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m", "limit": 25}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        let candles = out["candles"].as_array().unwrap();
        assert_eq!(candles.len(), 25);
        let times: Vec<i64> = candles.iter().map(|c| c["t"].as_i64().unwrap()).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]), "must be ascending");
    }

    #[test]
    fn an_absurd_limit_is_clamped_instead_of_being_sent_to_the_database() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        assert_eq!(ctx.clamp_lookback(Some(10_000_000)), ctx.max_lookback);
        assert_eq!(ctx.clamp_lookback(Some(0)), 1);
        assert_eq!(ctx.clamp_lookback(None), ctx.default_lookback);
    }

    #[tokio::test]
    async fn the_multi_timeframe_tool_returns_one_frame_per_request() {
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture);
        let out = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "analyze_multi_timeframe",
                    json!({"symbol": "BTCUSDT", "timeframes": ["1h", "5m"]}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["frames"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_multi_timeframe_tool_rejects_a_non_array() {
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture);
        let err = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "analyze_multi_timeframe",
                    json!({"symbol": "BTCUSDT", "timeframes": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, AgentError::InvalidToolArgs { .. }),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn backtest_tools_fail_clearly_when_no_runner_is_attached() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "backtest_similar_setups",
                    json!({"skill_ref": "liquidity-sweep-absorption-v2", "symbol": "BTCUSDT"}),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }), "got {err}");
    }

    #[tokio::test]
    async fn the_volume_profile_tool_reports_a_consistent_value_area() {
        let fixture = Fixture::rising(80);
        let ctx = ToolContext::new(&fixture);
        let out = ToolRegistry::market_analysis()
            .execute(
                &call(
                    "get_volume_profile",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out["val"].as_f64().unwrap() <= out["poc"].as_f64().unwrap());
        assert!(out["poc"].as_f64().unwrap() <= out["vah"].as_f64().unwrap());
        assert_eq!(out["source"], "candles");
    }

    #[tokio::test]
    async fn footprint_tools_say_unavailable_rather_than_empty() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        for tool in ["get_footprint", "detect_absorption", "detect_imbalance"] {
            let out = ToolRegistry::market_analysis()
                .execute(
                    &call(tool, json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                    &ctx,
                )
                .await
                .unwrap();
            assert_eq!(out["available"], false, "{tool} should report unavailable");
            assert!(out["note"].as_str().unwrap().contains("tick data"));
        }
    }

    #[tokio::test]
    async fn liquidity_levels_say_which_side_they_are_on() {
        // A live run had the model report a swept *high* as "sell-side
        // liquidity swept" -- the opposite of the truth, and of what the skill
        // asked for. Stating the side outright removes the inference that got
        // it wrong.
        // An oscillating fixture, because a monotonic one confirms no swings
        // and would make the assertions below vacuous.
        let fixture = Fixture::oscillating(200);
        let ctx = ToolContext::new(&fixture);
        let out = detect_liquidity(&ctx, &json!({"symbol": "BTCUSDT", "timeframe": "5m"}))
            .await
            .unwrap();

        let levels = out["levels"].as_array().unwrap();
        assert!(!levels.is_empty(), "the fixture should produce levels");

        let mut seen_above = false;
        let mut seen_below = false;
        for level in levels {
            let kind = level["kind"].as_str().unwrap();
            let side = level["side"]
                .as_str()
                .unwrap_or_else(|| panic!("level `{kind}` has no `side`"));
            let is_high = kind.contains("High");
            assert_eq!(
                side,
                if is_high { "buy_side" } else { "sell_side" },
                "`{kind}` mislabelled as {side}"
            );
            assert_eq!(level["above_market"].as_bool().unwrap(), is_high);
            seen_above |= is_high;
            seen_below |= !is_high;
        }
        // Both directions, or the test would pass by only checking one side.
        assert!(seen_above, "the fixture should produce highs");
        assert!(seen_below, "the fixture should produce lows");
    }

    #[tokio::test]
    async fn the_reclaim_verdict_is_computed_not_left_to_the_model() {
        // A live run asserted "price 77221.1 > swept level 77290.0" -- a
        // comparison it got backwards, on the one condition the sweep skill
        // rests on. The tool now answers it, so the model has nothing to
        // compare and nothing to get wrong.
        let fixture = Fixture::oscillating(200);
        let ctx = ToolContext::new(&fixture);
        let out = detect_liquidity(&ctx, &json!({"symbol": "BTCUSDT", "timeframe": "5m"}))
            .await
            .unwrap();

        let price = out["price"].as_f64().unwrap();
        let reclaim = &out["reclaim"];
        assert_eq!(reclaim["price"].as_f64().unwrap(), price);

        for (key, want_side) in [("long_reclaim", "sell_side"), ("short_reclaim", "buy_side")] {
            let side = reclaim[key]["side"].as_str().unwrap();
            assert_eq!(side, want_side);

            let reclaimed = reclaim[key]["reclaimed"].as_bool().unwrap_or_else(|| {
                panic!("{key} has no boolean `reclaimed` -- the model would have to infer it")
            });
            let level = reclaim[key]["swept_level"].as_f64();
            // The two must agree: a verdict without a level is not citable.
            assert_eq!(
                reclaimed,
                level.is_some(),
                "{key}: {reclaimed} vs {level:?}"
            );
            if let Some(level) = level {
                // The whole point: the comparison is done here, and it is true.
                if key == "long_reclaim" {
                    assert!(price > level, "long reclaim with price {price} <= {level}");
                } else {
                    assert!(price < level, "short reclaim with price {price} >= {level}");
                }
            }
        }
    }

    #[test]
    fn a_reclaim_verdict_never_claims_a_level_that_price_is_on_the_wrong_side_of() {
        // Unit-level, over hand-built levels: the verdict is a strict
        // inequality on the correct side, in both directions.
        let level =
            |price: f64, above: bool, swept: bool| analytics_core::liquidity::LiquidityLevel {
                price,
                kind: if above {
                    analytics_core::liquidity::LiquidityKind::SwingHigh
                } else {
                    analytics_core::liquidity::LiquidityKind::SwingLow
                },
                touches: 1,
                swept,
                formed_at: 0,
                last_index: 0,
            };

        let levels = vec![
            level(90.0, false, true),  // swept low, below price
            level(95.0, false, false), // unswept low
            level(110.0, true, true),  // swept high, above price
            level(98.0, true, true),   // swept high price has already run through
        ];
        let verdict = reclaim_verdict(&levels, 100.0);

        assert!(verdict["long_reclaim"]["reclaimed"].as_bool().unwrap());
        assert_eq!(verdict["long_reclaim"]["swept_level"].as_f64(), Some(90.0));
        assert!(verdict["short_reclaim"]["reclaimed"].as_bool().unwrap());
        assert_eq!(
            verdict["short_reclaim"]["swept_level"].as_f64(),
            Some(110.0)
        );

        // Nothing swept on either side: no reclaim, and no level to cite.
        let unswept = vec![level(90.0, false, false), level(110.0, true, false)];
        let verdict = reclaim_verdict(&unswept, 100.0);
        assert!(!verdict["long_reclaim"]["reclaimed"].as_bool().unwrap());
        assert!(!verdict["short_reclaim"]["reclaimed"].as_bool().unwrap());
        assert!(verdict["long_reclaim"]["swept_level"].is_null());
    }

    #[tokio::test]
    async fn market_structure_and_liquidity_are_reported_from_candles_alone() {
        let fixture = Fixture::rising(80);
        let ctx = ToolContext::new(&fixture);
        let registry = ToolRegistry::market_analysis();

        let structure = registry
            .execute(
                &call(
                    "detect_market_structure",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        // The trend comes straight from the core's structure detector; this
        // asserts the tool reports it, not what it should be for the fixture.
        let trend = structure["trend"].as_str().unwrap();
        assert!(
            matches!(trend, "Bullish" | "Bearish" | "Ranging"),
            "unexpected trend {trend}"
        );
        // A strictly monotonic fixture has no local extrema, so no swings are
        // confirmed -- which is correct, not a failure. Structure is still
        // reported, which is what this tool is for.
        assert!(structure["swing_highs"].is_array());
        assert!(structure["recent_breaks"].is_array());

        let liquidity = registry
            .execute(
                &call(
                    "detect_liquidity",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert!(liquidity["price"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn a_tool_call_extracted_from_a_message_is_what_the_registry_consumes() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "abc".into(),
                name: "get_vwap".into(),
                input: json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
            }],
        };
        let calls = message.tool_calls();
        assert_eq!(calls[0].name, "get_vwap");
        assert_eq!(calls[0].id, "abc");
    }

    // -----------------------------------------------------------------------
    // get_user_drawings (docs/19 row 19)
    // -----------------------------------------------------------------------

    /// A drawings source over a fixed list, per user.
    struct DrawingsFixture(Vec<crate::user_drawings::UserDrawing>);

    #[async_trait]
    impl crate::user_drawings::UserDrawingsSource for DrawingsFixture {
        async fn drawings(
            &self,
            user_id: &str,
            _symbol: &str,
        ) -> Result<Vec<crate::user_drawings::UserDrawing>, AgentError> {
            // Scoping is the host's job; the fixture only proves the tool
            // forwards the opaque id it was given.
            if user_id == "someone-else" {
                return Ok(Vec::new());
            }
            Ok(self.0.clone())
        }
    }

    fn hline(price: f64) -> crate::user_drawings::UserDrawing {
        crate::user_drawings::UserDrawing {
            kind: "hline".into(),
            label: Some("the range low".into()),
            time1_ms: 1_767_225_600_000.0,
            price1: price,
            time2_ms: None,
            price2: None,
            time3_ms: None,
            price3: None,
        }
    }

    fn drawings_ctx<'a>(
        fixture: &'a Fixture,
        source: &'a DrawingsFixture,
        user: &'a str,
    ) -> ToolContext<'a> {
        ToolContext::new(fixture).with_drawings(source, user)
    }

    #[tokio::test]
    async fn the_drawings_tool_reports_the_users_marks_with_both_anchors() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(vec![
            hline(45_000.0),
            crate::user_drawings::UserDrawing {
                kind: "trendline".into(),
                label: None,
                time1_ms: 1.0,
                price1: 44_000.0,
                time2_ms: Some(2.0),
                price2: Some(46_000.0),
                time3_ms: None,
                price3: None,
            },
        ]);
        let ctx = drawings_ctx(&fixture, &source, "user-1");

        let out = registry()
            .execute(
                &call("get_user_drawings", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["count"], 2);
        assert_eq!(out["drawings"][0]["kind"], "hline");
        assert_eq!(out["drawings"][0]["price1"], 45_000.0);
        // The second anchor survives: which end a trendline points at is the
        // information a one-price summary throws away.
        assert_eq!(out["drawings"][1]["price2"], 46_000.0);
        assert_eq!(out["drawings"][1]["time2_ms"], 2.0);
        assert_eq!(out["read_only"], true);
    }

    #[tokio::test]
    async fn a_missing_drawings_source_is_an_error_not_a_blank_chart() {
        // The failure mode the tool exists to prevent: a host with no source
        // read by the model as "the user drew nothing".
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);

        let error = registry()
            .execute(
                &call("get_user_drawings", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .expect_err("must refuse");

        let AgentError::ToolFailed { tool, reason } = &error else {
            panic!("expected ToolFailed, got {error:?}");
        };
        assert_eq!(tool, "get_user_drawings");
        assert!(reason.contains("no drawings source"), "{reason}");
        assert!(reason.contains("do not assume"), "{reason}");
    }

    #[tokio::test]
    async fn drawings_are_scoped_to_the_user_the_host_named() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(vec![hline(45_000.0)]);
        let ctx = drawings_ctx(&fixture, &source, "someone-else");

        let out = registry()
            .execute(
                &call("get_user_drawings", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["count"], 0, "another user's drawings are not readable");
        assert_eq!(out["drawings"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn an_oversize_drawing_list_is_trimmed_and_says_so() {
        let fixture = Fixture::rising(10);
        let many: Vec<crate::user_drawings::UserDrawing> =
            (0..30).map(|i| hline(f64::from(i) + 1.0)).collect();
        let source = DrawingsFixture(many);
        let ctx = drawings_ctx(&fixture, &source, "user-1");

        let out = registry()
            .execute(
                &call("get_user_drawings", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["count"], crate::chart_context::MAX_DRAWINGS);
        assert_eq!(out["total"], 30);
        assert!(out["note"].as_str().unwrap().contains("oldest of 30"));
    }

    // -----------------------------------------------------------------------
    // The write path (docs/21)
    // -----------------------------------------------------------------------

    /// A writer over an in-memory list, recording what it was asked to store.
    /// Proves the tool forwards the right arguments and the identity it was
    /// given; the *validation* is the host adapter's job and is tested there.
    struct WriterFixture {
        created: std::sync::Mutex<Vec<(String, String, crate::user_drawings::NewAgentDrawing)>>,
        deleted: std::sync::Mutex<Vec<(String, String, String)>>,
        next_id: std::sync::atomic::AtomicUsize,
    }

    impl WriterFixture {
        fn new() -> Self {
            Self {
                created: std::sync::Mutex::new(Vec::new()),
                deleted: std::sync::Mutex::new(Vec::new()),
                next_id: std::sync::atomic::AtomicUsize::new(1),
            }
        }

        fn created_count(&self) -> usize {
            self.created.lock().expect("lock").len()
        }
    }

    #[async_trait]
    impl crate::user_drawings::DrawingWriter for WriterFixture {
        async fn create(
            &self,
            user_id: &str,
            symbol: &str,
            drawing: &crate::user_drawings::NewAgentDrawing,
        ) -> Result<crate::user_drawings::StoredDrawing, AgentError> {
            self.created.lock().expect("lock").push((
                user_id.to_string(),
                symbol.to_string(),
                drawing.clone(),
            ));
            let n = self
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(crate::user_drawings::StoredDrawing {
                id: format!("stored-{n}"),
                symbol: symbol.to_uppercase(),
                kind: drawing.kind.clone(),
            })
        }

        async fn update(
            &self,
            _user_id: &str,
            _symbol: &str,
            _id: &str,
            _drawing: &crate::user_drawings::NewAgentDrawing,
        ) -> Result<bool, AgentError> {
            Ok(true)
        }

        async fn delete(&self, user_id: &str, _symbol: &str, id: &str) -> Result<bool, AgentError> {
            self.deleted.lock().expect("lock").push((
                user_id.to_string(),
                id.to_string(),
                String::new(),
            ));
            // "stored-*" exists in this fixture's memory; anything else is a
            // stale id, which storage reports as `false` -- the neutral fact
            // the tool is meant to relay.
            Ok(id.starts_with("stored-"))
        }
    }

    fn write_ctx<'a>(
        fixture: &'a Fixture,
        source: &'a DrawingsFixture,
        writer: &'a WriterFixture,
        user: &'a str,
    ) -> ToolContext<'a> {
        ToolContext::new(fixture)
            .with_drawings(source, user)
            .with_drawing_writer(writer, user)
    }

    #[tokio::test]
    async fn a_created_drawing_carries_the_provenance_the_model_stated() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let writer = WriterFixture::new();
        let ctx = write_ctx(&fixture, &source, &writer, "user-1");

        let out = registry()
            .execute(
                &call(
                    "create_drawing",
                    json!({
                        "symbol": "BTCUSDT",
                        "kind": "hline",
                        "time1_ms": 1_767_225_600_000.0_f64,
                        "price1": 45_000.0,
                        "label": "AI resistance",
                        "confidence": 0.87,
                        "reason": "three rejections in the last 80 bars",
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["created"], true);
        assert_eq!(out["id"], "stored-1");
        let (user, symbol, drawing) = &writer.created.lock().expect("lock")[0];
        assert_eq!(user, "user-1", "the write goes out as the asking user");
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(drawing.kind, "hline");
        let provenance = drawing.provenance.as_ref().expect("stated provenance");
        assert_eq!(provenance.confidence, Some(0.87));
        assert_eq!(
            provenance.reason.as_deref(),
            Some("three rejections in the last 80 bars")
        );
    }

    #[tokio::test]
    async fn a_drawing_without_stated_provenance_still_stores() {
        // The schema marks confidence and reason optional: forcing the model to
        // invent a number would be worse than an unstated one.
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let writer = WriterFixture::new();
        let ctx = write_ctx(&fixture, &source, &writer, "user-1");

        let out = registry()
            .execute(
                &call(
                    "create_drawing",
                    json!({
                        "symbol": "BTCUSDT",
                        "kind": "trendline",
                        "time1_ms": 1.0,
                        "price1": 44_000.0,
                        "time2_ms": 2.0,
                        "price2": 46_000.0,
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["created"], true);
        let (_, _, drawing) = &writer.created.lock().expect("lock")[0];
        assert!(drawing.provenance.is_none());
    }

    #[tokio::test]
    async fn a_half_second_anchor_is_refused_with_an_actionable_message() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let writer = WriterFixture::new();
        let ctx = write_ctx(&fixture, &source, &writer, "user-1");

        let err = registry()
            .execute(
                &call(
                    "create_drawing",
                    json!({
                        "symbol": "BTCUSDT",
                        "kind": "trendline",
                        "time1_ms": 1.0,
                        "price1": 44_000.0,
                        "time2_ms": 2.0,
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, AgentError::InvalidToolArgs { .. }),
            "got {err}"
        );
        assert_eq!(writer.created_count(), 0, "nothing stored on refusal");
    }

    #[tokio::test]
    async fn an_invented_confidence_is_clamped_into_the_stated_range() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let writer = WriterFixture::new();
        let ctx = write_ctx(&fixture, &source, &writer, "user-1");

        registry()
            .execute(
                &call(
                    "create_drawing",
                    json!({
                        "symbol": "BTCUSDT",
                        "kind": "hline",
                        "time1_ms": 1.0,
                        "price1": 45_000.0,
                        "confidence": 1.4,
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();
        let (_, _, drawing) = &writer.created.lock().expect("lock")[0];
        assert_eq!(
            drawing.provenance.as_ref().and_then(|p| p.confidence),
            Some(1.0),
            "confidence above 1 is clamped, not stored"
        );
    }

    #[tokio::test]
    async fn the_write_tools_say_so_when_no_writer_is_attached() {
        // A read-only deployment must stay read-only, and the model must be
        // able to tell the user that rather than claim a drawing happened.
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let ctx = ToolContext::new(&fixture).with_drawings(&source, "user-1");

        let err = registry()
            .execute(
                &call(
                    "create_drawing",
                    json!({"symbol": "BTCUSDT", "kind": "hline", "time1_ms": 1.0, "price1": 1.0}),
                ),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::ToolFailed { .. }), "got {err}");
        let message = err.to_string();
        assert!(
            message.contains("no drawing writer"),
            "the refusal names the missing capability: {message}"
        );
    }

    #[tokio::test]
    async fn a_delete_of_something_already_gone_reports_neutrally() {
        let fixture = Fixture::rising(10);
        let source = DrawingsFixture(Vec::new());
        let writer = WriterFixture::new();
        let ctx = write_ctx(&fixture, &source, &writer, "user-1");

        let out = registry()
            .execute(
                &call(
                    "delete_drawing",
                    json!({"symbol": "BTCUSDT", "id": "not-stored"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            out["deleted"], false,
            "nothing deleted is a fact, not an error"
        );
        assert!(out["note"].as_str().unwrap().contains("already gone"));
    }

    #[tokio::test]
    async fn the_registry_advertises_the_write_tools() {
        let reg = registry();
        let names = reg.names();
        for tool in ["create_drawing", "update_drawing", "delete_drawing"] {
            assert!(names.contains(&tool), "{tool} is registered");
        }
    }

    // -----------------------------------------------------------------------
    // Memory tools. The fixtures mirror the drawings ones: a source, a
    // writer, and the tool's own vocabulary exercised against both.
    // -----------------------------------------------------------------------

    struct MemoryFixture {
        rows: std::sync::Mutex<Vec<crate::agent_memory::MemoryRow>>,
        stored: std::sync::Mutex<Vec<(String, crate::agent_memory::NewMemory)>>,
        forgotten: std::sync::Mutex<Vec<(String, Option<String>, String)>>,
        next_ns: std::sync::atomic::AtomicI64,
    }

    impl MemoryFixture {
        fn new() -> Self {
            Self {
                rows: std::sync::Mutex::new(Vec::new()),
                stored: std::sync::Mutex::new(Vec::new()),
                forgotten: std::sync::Mutex::new(Vec::new()),
                next_ns: std::sync::atomic::AtomicI64::new(1),
            }
        }
    }

    #[async_trait]
    impl crate::agent_memory::MemorySource for MemoryFixture {
        async fn recall(
            &self,
            user_id: &str,
            symbol: &str,
            limit: usize,
        ) -> Result<Vec<crate::agent_memory::MemoryRow>, AgentError> {
            let rows = self.rows.lock().expect("lock").clone();
            Ok(rows
                .into_iter()
                .filter(|row| row.symbol.as_deref() == Some(symbol) || row.scope == "global")
                .filter(|_| user_id != "someone-else")
                .take(limit)
                .collect())
        }
    }

    #[async_trait]
    impl crate::agent_memory::MemoryWriter for MemoryFixture {
        async fn remember(
            &self,
            user_id: &str,
            memory: &crate::agent_memory::NewMemory,
        ) -> Result<(), AgentError> {
            self.stored
                .lock()
                .expect("lock")
                .push((user_id.to_string(), memory.clone()));
            let ns = self
                .next_ns
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.rows
                .lock()
                .expect("lock")
                .push(crate::agent_memory::MemoryRow {
                    scope: memory.scope.clone(),
                    symbol: memory.symbol.clone(),
                    key: memory.key.clone(),
                    content: memory.content.clone(),
                    updated_at: ns,
                });
            Ok(())
        }

        async fn forget(
            &self,
            user_id: &str,
            symbol: Option<&str>,
            key: &str,
        ) -> Result<bool, AgentError> {
            self.forgotten.lock().expect("lock").push((
                user_id.to_string(),
                symbol.map(str::to_string),
                key.to_string(),
            ));
            let mut rows = self.rows.lock().expect("lock");
            let before = rows.len();
            rows.retain(|row| !(row.key == key && row.symbol.as_deref() == symbol));
            Ok(rows.len() < before)
        }
    }

    fn memory_ctx<'a>(
        fixture: &'a Fixture,
        memory: &'a MemoryFixture,
        user: &'a str,
    ) -> ToolContext<'a> {
        ToolContext::new(fixture).with_memory(memory, Some(memory), user)
    }

    #[tokio::test]
    async fn a_remembered_fact_is_stored_under_the_asking_user() {
        let fixture = Fixture::rising(10);
        let memory = MemoryFixture::new();
        let ctx = memory_ctx(&fixture, &memory, "user-1");

        let out = registry()
            .execute(
                &call(
                    "remember",
                    json!({
                        "scope": "symbol",
                        "symbol": "btcusdt",
                        "key": "4h_resistance",
                        "content": "108,500 rejected price twice this week",
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["stored"], true);
        let (user, fact) = &memory.stored.lock().expect("lock")[0];
        assert_eq!(user, "user-1", "the write goes out as the asking user");
        assert_eq!(fact.symbol.as_deref(), Some("BTCUSDT"), "symbols normalise");
        assert_eq!(fact.key, "4h_resistance");
    }

    #[tokio::test]
    async fn a_global_fact_cannot_smuggle_a_symbol() {
        let fixture = Fixture::rising(10);
        let memory = MemoryFixture::new();
        let ctx = memory_ctx(&fixture, &memory, "user-1");

        let err = registry()
            .execute(
                &call(
                    "remember",
                    json!({
                        "scope": "global",
                        "symbol": "BTCUSDT",
                        "key": "risk_style",
                        "content": "risks 0.5R",
                    }),
                ),
                &ctx,
            )
            .await
            .expect_err("must refuse");
        assert!(matches!(err, AgentError::InvalidToolArgs { .. }));
        assert!(memory.stored.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn recall_reports_what_is_stored_with_the_scoping_kept() {
        let fixture = Fixture::rising(10);
        let memory = MemoryFixture::new();
        memory
            .rows
            .lock()
            .expect("lock")
            .push(crate::agent_memory::MemoryRow {
                scope: "global".into(),
                symbol: None,
                key: "risk_style".into(),
                content: "risks 0.5R".into(),
                updated_at: 1,
            });
        let ctx = memory_ctx(&fixture, &memory, "user-1");

        let out = registry()
            .execute(&call("recall_memories", json!({"symbol": "BTCUSDT"})), &ctx)
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["memories"][0]["key"], "risk_style");
        assert_eq!(
            out["read_only"],
            json!(null),
            "recall is read-only, and says nothing about being otherwise"
        );
    }

    #[tokio::test]
    async fn forgetting_reports_a_miss_honestly() {
        let fixture = Fixture::rising(10);
        let memory = MemoryFixture::new();
        let ctx = memory_ctx(&fixture, &memory, "user-1");

        let out = registry()
            .execute(
                &call(
                    "forget_memory",
                    json!({"symbol": "BTCUSDT", "key": "4h_resistance"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["forgotten"], false, "nothing was there to drop");
    }

    #[tokio::test]
    async fn a_missing_memory_source_is_an_error_not_an_empty_memory() {
        // The failure mode the tool exists to prevent, same as the drawings
        // one: a host with no source, read by the model as "nothing stored".
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);

        let err = registry()
            .execute(&call("recall_memories", json!({"symbol": "BTCUSDT"})), &ctx)
            .await
            .expect_err("must refuse");
        assert!(matches!(err, AgentError::InvalidToolArgs { .. }));
    }
}
