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
    bar_delta_stats_window, build_market_state, calculate_cvd_by_size, calculate_vpin_series,
    calculate_volume_profile_from_candles, delta_by_size_per_candle, detect_absorption,
    detect_imbalances_with, detect_liquidity_levels_with, detect_market_structure, MarketState,
    MarketStateConfig, SizeClass, SizeClassConfig, VpinConfig,
};

use crate::error::AgentError;
use crate::llm_client::ToolCall;
use crate::user_drawings::{DrawingWriter, NewAgentDrawing, UserDrawingsSource};

use crate::agent_memory::{MemorySource, MemoryWriter, NewMemory};
use crate::snapshots::{NewSnapshot, SnapshotStore};

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

impl std::fmt::Debug for dyn BacktestRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The runner is a callback into the host's market data; printing it
        // would print an address. Its presence is the fact worth logging.
        f.write_str("BacktestRunner(..)")
    }
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
    /// The capability registry scoped to this request's venue (`docs/39`).
    ///
    /// When attached, every capability-backed tool result gains a
    /// `provenance` block at dispatch, and the state render labels its
    /// sections available/derived/unavailable. `None` — tests, tools-only
    /// builds — means no claims are made, the same posture as the other
    /// sources.
    pub capabilities: Option<crate::capability_view::CapabilityView>,
    /// The asking user's snapshot store, when the host attached one
    /// (`docs/45`).
    ///
    /// `None` makes the snapshot tools report "no snapshot store is attached"
    /// rather than capture into the void -- a snapshot the model believes
    /// exists and nothing can retrieve is worse than the refusal.
    pub snapshots: Option<&'a dyn SnapshotStore>,
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
            capabilities: None,
            snapshots: None,
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

    /// Attach the capability view for this request's venue (`docs/39`).
    #[must_use]
    pub fn with_capabilities(mut self, view: crate::capability_view::CapabilityView) -> Self {
        self.capabilities = Some(view);
        self
    }

    /// Attach the asking user's snapshot store, and whose it is.
    ///
    /// The id takes the same argument the drawings and memory builders take:
    /// one identity per capability, and no path by which they diverge.
    #[must_use]
    pub fn with_snapshots(mut self, store: &'a dyn SnapshotStore, user_id: &'a str) -> Self {
        self.snapshots = Some(store);
        self.user_id = Some(user_id);
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
                    exposed_tool: None, // base tool: always exposed
                },
                ToolSpec {
                    name: "analyze_multi_timeframe".into(),
                    description: ANALYZE_MULTI_TIMEFRAME.into(),
                    input_schema: multi_timeframe_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "get_candles".into(),
                    description: GET_CANDLES.into(),
                    input_schema: symbol_timeframe_schema(Some("limit")),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "get_volume_profile".into(),
                    description: GET_VOLUME_PROFILE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("volume_profile"),
                },
                ToolSpec {
                    name: "get_footprint".into(),
                    description: GET_FOOTPRINT.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                    exposed_tool: Some("footprint"),
                },
                ToolSpec {
                    name: "get_delta_by_size".into(),
                    description: GET_DELTA_BY_SIZE.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "get_bar_delta_stats".into(),
                    description: GET_BAR_DELTA_STATS.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "get_vpin".into(),
                    description: GET_VPIN.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "detect_size_divergence".into(),
                    description: DETECT_SIZE_DIVERGENCE.into(),
                    input_schema: symbol_timeframe_schema(Some("count")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "get_delta".into(),
                    description: GET_DELTA.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "get_cvd".into(),
                    description: GET_CVD.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("delta"),
                },
                ToolSpec {
                    name: "get_vwap".into(),
                    description: GET_VWAP.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("vwap"),
                },
                ToolSpec {
                    name: "detect_liquidity".into(),
                    description: DETECT_LIQUIDITY.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("liquidity"),
                },
                ToolSpec {
                    name: "detect_absorption".into(),
                    description: DETECT_ABSORPTION.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("absorption"),
                },
                ToolSpec {
                    name: "detect_imbalance".into(),
                    description: DETECT_IMBALANCE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("imbalance"),
                },
                ToolSpec {
                    name: "detect_market_structure".into(),
                    description: DETECT_MARKET_STRUCTURE.into(),
                    input_schema: symbol_timeframe_schema(Some("lookback")),
                    exposed_tool: Some("market_structure"),
                },
                ToolSpec {
                    name: "get_rsi".into(),
                    description: GET_RSI.into(),
                    input_schema: indicator_schema(&[("period", period_prop(14)), ("limit", limit_prop())]),
                    exposed_tool: Some("classic_indicators"),
                },
                ToolSpec {
                    name: "get_macd".into(),
                    description: GET_MACD.into(),
                    input_schema: indicator_schema(&[
                        ("fast", period_prop(12)),
                        ("slow", period_prop(26)),
                        ("signal", period_prop(9)),
                        ("limit", limit_prop()),
                    ]),
                    exposed_tool: Some("classic_indicators"),
                },
                ToolSpec {
                    name: "get_bollinger_bands".into(),
                    description: GET_BOLLINGER_BANDS.into(),
                    input_schema: indicator_schema(&[
                        ("period", period_prop(20)),
                        ("mult", json!({"type": "number", "exclusiveMinimum": 0.0, "maximum": 5.0,
                                        "description": "Band width in standard deviations (default 2)"})),
                        ("limit", limit_prop()),
                    ]),
                    exposed_tool: Some("classic_indicators"),
                },
                ToolSpec {
                    name: "get_atr".into(),
                    description: GET_ATR.into(),
                    input_schema: indicator_schema(&[("period", period_prop(14))]),
                    exposed_tool: Some("classic_indicators"),
                },
                ToolSpec {
                    name: "get_moving_average".into(),
                    description: GET_MOVING_AVERAGE.into(),
                    input_schema: indicator_schema(&[
                        ("kind", json!({"type": "string", "enum": ["ema", "sma"],
                                        "description": "Average kind (default ema)"})),
                        ("period", period_prop(20)),
                        ("limit", limit_prop()),
                    ]),
                    exposed_tool: Some("classic_indicators"),
                },
                ToolSpec {
                    name: "detect_pattern".into(),
                    description: DETECT_PATTERN.into(),
                    input_schema: detect_pattern_schema(),
                    exposed_tool: Some("patterns"),
                },
                ToolSpec {
                    name: "detect_zones".into(),
                    description: DETECT_ZONES.into(),
                    input_schema: detect_zones_schema(),
                    exposed_tool: Some("market_structure"),
                },
                ToolSpec {
                    name: "open_chart".into(),
                    description: OPEN_CHART.into(),
                    input_schema: open_chart_schema(),
                    // The screen belongs to the asking user, not to a venue
                    // capability -- and a screen nobody is looking at is a
                    // no-op the note already owns up to.
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "set_chart".into(),
                    description: SET_CHART.into(),
                    input_schema: set_chart_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "compare_timeframes".into(),
                    description: COMPARE_TIMEFRAMES.into(),
                    input_schema: confluence_schema(false),
                    exposed_tool: Some("market_structure"),
                },
                ToolSpec {
                    name: "cross_timeframe_confluence".into(),
                    description: CROSS_TIMEFRAME_CONFLUENCE.into(),
                    input_schema: confluence_schema(true),
                    exposed_tool: Some("market_structure"),
                },
                ToolSpec {
                    name: "backtest_strategy".into(),
                    description: BACKTEST_STRATEGY.into(),
                    input_schema: backtest_strategy_schema(),
                    exposed_tool: None, // research tools always available
                },
                ToolSpec {
                    name: "backtest_similar_setups".into(),
                    description: BACKTEST_SIMILAR_SETUPS.into(),
                    input_schema: backtest_similar_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "get_user_drawings".into(),
                    description: GET_USER_DRAWINGS.into(),
                    input_schema: get_user_drawings_schema(),
                    exposed_tool: None, // drawing tools always available (if capacity)
                },
                ToolSpec {
                    name: "create_drawing".into(),
                    description: CREATE_DRAWING.into(),
                    input_schema: drawing_write_schema(false),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "update_drawing".into(),
                    description: UPDATE_DRAWING.into(),
                    input_schema: drawing_write_schema(true),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "delete_drawing".into(),
                    description: DELETE_DRAWING.into(),
                    input_schema: drawing_delete_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "remember".into(),
                    description: REMEMBER.into(),
                    input_schema: remember_schema(),
                    exposed_tool: None, // memory tools always available
                },
                ToolSpec {
                    name: "recall_memories".into(),
                    description: RECALL_MEMORIES.into(),
                    input_schema: recall_memories_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "forget_memory".into(),
                    description: FORGET_MEMORY.into(),
                    input_schema: forget_memory_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "take_snapshot".into(),
                    description: TAKE_SNAPSHOT.into(),
                    input_schema: take_snapshot_schema(),
                    exposed_tool: None, // snapshot tools always available (if capacity)
                },
                ToolSpec {
                    name: "get_snapshot".into(),
                    description: GET_SNAPSHOT.into(),
                    input_schema: get_snapshot_schema(),
                    exposed_tool: None,
                },
                ToolSpec {
                    name: "compare_snapshots".into(),
                    description: COMPARE_SNAPSHOTS.into(),
                    input_schema: compare_snapshots_schema(),
                    exposed_tool: None,
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

    /// The tool specs that should be exposed given the capability view
    /// (Phase 3, docs/41).
    ///
    /// Tools without an `exposed_tool` are always exposed (base tools,
    /// drawing, memory, backtests). Tools with an `exposed_tool` are exposed
    /// only when their capability resolves (or when no view is attached,
    /// which means the host has no registry to query).
    #[must_use]
    pub fn exposed_tools(
        &self,
        view: Option<&crate::capability_view::CapabilityView>,
    ) -> Vec<crate::llm_client::ToolSpec> {
        let mut exposed = Vec::new();
        for spec in &self.specs {
            match spec.exposed_tool {
                None => {
                    exposed.push(spec.clone());
                }
                Some(capability) => {
                    if let Some(v) = view {
                        let resolution = v.resolve(capability, "placeholder");
                        match resolution.availability {
                            capabilities::Availability::Available
                            | capabilities::Availability::Degraded
                            | capabilities::Availability::Derived => {
                                exposed.push(spec.clone());
                            }
                            _ => {}
                        }
                    } else {
                        // No registry attached: behave as before, expose all.
                        exposed.push(spec.clone());
                    }
                }
            }
        }
        exposed
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
            "get_delta_by_size" => get_delta_by_size(ctx, &call.input).await,
            "get_bar_delta_stats" => get_bar_delta_stats(ctx, &call.input).await,
            "get_vpin" => get_vpin(ctx, &call.input).await,
            "detect_size_divergence" => detect_size_divergence(ctx, &call.input).await,
            "get_delta" => get_delta(ctx, &call.input).await,
            "get_cvd" => get_cvd(ctx, &call.input).await,
            "get_vwap" => get_vwap(ctx, &call.input).await,
            "detect_liquidity" => detect_liquidity(ctx, &call.input).await,
            "detect_absorption" => detect_absorption_tool(ctx, &call.input).await,
            "detect_imbalance" => detect_imbalance(ctx, &call.input).await,
            "detect_market_structure" => detect_market_structure_tool(ctx, &call.input).await,
            "get_rsi" => get_rsi(ctx, &call.input).await,
            "get_macd" => get_macd(ctx, &call.input).await,
            "get_bollinger_bands" => get_bollinger_bands(ctx, &call.input).await,
            "get_atr" => get_atr_tool(ctx, &call.input).await,
            "get_moving_average" => get_moving_average(ctx, &call.input).await,
            "detect_pattern" => detect_pattern_tool(ctx, &call.input).await,
            "detect_zones" => detect_zones_tool(ctx, &call.input).await,
            "open_chart" => open_chart_tool(ctx, &call.input).await,
            "set_chart" => set_chart_tool(ctx, &call.input).await,
            "compare_timeframes" => compare_timeframes(ctx, &call.input).await,
            "cross_timeframe_confluence" => cross_timeframe_confluence(ctx, &call.input).await,
            "backtest_strategy" => backtest_strategy(ctx, &call.input).await,
            "backtest_similar_setups" => backtest_similar_setups(ctx, &call.input).await,
            "get_user_drawings" => get_user_drawings(ctx, &call.input).await,
            "create_drawing" => create_drawing(ctx, &call.input).await,
            "update_drawing" => update_drawing(ctx, &call.input).await,
            "delete_drawing" => delete_drawing(ctx, &call.input).await,
            "remember" => remember(ctx, &call.input).await,
            "recall_memories" => recall_memories(ctx, &call.input).await,
            "forget_memory" => forget_memory(ctx, &call.input).await,
            "take_snapshot" => take_snapshot(ctx, &call.input).await,
            "get_snapshot" => get_snapshot_tool(ctx, &call.input).await,
            "compare_snapshots" => compare_snapshots(ctx, &call.input).await,
            other => return Err(AgentError::UnknownTool(other.to_string())),
        };
        let elapsed = started.elapsed();

        // Provenance attaches here, once, at the seam every tool call passes
        // through (docs/39): the registry's answer for the capability this
        // tool exposes, named for the symbol the call was about. Plumbing
        // tools (memory, drawings, backtests) have no capability row and get
        // no block; a failed call gets none either — the error is already the
        // honest statement.
        let result = result.map(|mut value| {
            if let (Some(view), Some(symbol)) = (
                ctx.capabilities.as_ref(),
                call.input.get("symbol").and_then(Value::as_str),
            ) {
                if let (Some(block), Some(object)) =
                    (view.tool_provenance(&call.name, symbol), value.as_object_mut())
                {
                    object.insert("provenance".to_string(), block);
                }
            }
            value
        });

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

/// Indicator tool schema: symbol + timeframe + lookback, with per-tool knobs
/// merged in. Keeping the base shared means every indicator advertises the
/// same timeframe vocabulary the engine accepts.
fn indicator_schema(extra: &[(&str, Value)]) -> Value {
    let mut schema = symbol_timeframe_schema(Some("lookback"));
    let properties = schema["properties"]
        .as_object_mut()
        .expect("symbol_timeframe_schema is an object");
    for (name, value) in extra {
        properties.insert((*name).to_string(), value.clone());
    }
    schema
}

fn period_prop(default: u32) -> Value {
    json!({"type": "integer", "minimum": 2, "maximum": 500,
           "description": format!("Indicator period (default {default})")})
}

fn limit_prop() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": 50,
           "description": "How many recent readings to return (default 10)"})
}

fn detect_pattern_schema() -> Value {
    let patterns: Vec<Value> = analytics_core::PatternKind::ALL
        .iter()
        .map(|k| Value::from(k.name()))
        .collect();
    indicator_schema(&[
        ("pattern", json!({
            "type": "string",
            "enum": patterns,
            "description": "The pattern to look for. Omit to detect every kind.",
        })),
        ("tolerance_pct", json!({
            "type": "number", "exclusiveMinimum": 0.0, "maximum": 0.05,
            "description": "How far 'equal' extremes may differ, as a fraction of price (default 0.004 = 0.4%)",
        })),
        ("min_confidence", json!({
            "type": "number", "minimum": 0.0, "maximum": 1.0,
            "description": "Only report matches at or above this confidence (default 0.5)",
        })),
    ])
}

fn detect_zones_schema() -> Value {
    symbol_timeframe_schema(Some("lookback"))
}

fn open_chart_schema() -> Value {
    symbol_timeframe_schema(None)
}

fn set_chart_schema() -> Value {
    // Both fields optional at the schema level; the tool itself refuses a
    // call that changes nothing, because the refusal carries the reason and
    // an empty `required` list cannot.
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "The symbol to show, e.g. BTCUSDT."},
            "timeframe": {"type": "string", "description": "The timeframe to show, e.g. 5m."},
        },
        "required": [],
    })
}

fn confluence_schema(with_tolerance: bool) -> Value {
    let mut schema = multi_timeframe_schema();
    if with_tolerance {
        schema["properties"]["tolerance_pct"] = json!({
            "type": "number", "exclusiveMinimum": 0.0, "maximum": 0.02,
            "description": "Cluster radius as a fraction of price (default 0.0015 = 0.15%)",
        });
    }
    schema
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

fn take_snapshot_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "symbol": {"type": "string", "description": "Trading symbol, e.g. BTCUSDT"},
            "timeframe": {"type": "string", "enum": timeframe_enum(), "description": timeframe_description()},
            "note": {"type": "string", "description": "What this capture is for, in your own words -- 'before the FOMC print', 'the range as mapped'"},
            "tags": {"type": "array", "items": {"type": "string"}, "description": "Retrieval tags, e.g. [\"ny-open\", \"range\"]"},
        },
        "required": ["symbol", "timeframe"],
    })
}

fn get_snapshot_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "The snapshot's id, from take_snapshot or compare_snapshots"},
        },
        "required": ["id"],
    })
}

fn compare_snapshots_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id_a": {"type": "string", "description": "The earlier snapshot's id"},
            "id_b": {"type": "string", "description": "The later snapshot's id"},
            "symbol": {"type": "string", "description": "Or just the symbol: its two most recent snapshots are compared"},
        },
        // Either the two ids or the symbol; enforced in the tool, where the
        // error can say so in a sentence a model recovers from.
        "required": [],
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

const GET_CANDLES: &str = "Recent OHLCV candles with buy/sell split, oldest first. Each candle \
    carries `i` (its index in this window, 0 = oldest) and `time_ms`, so a skill's drawing \
    recipe can name a candle by position -- \"the third candle from the right\" is \
    `count - 3` -- and its `time_ms` feeds create_drawing's anchors directly. Use it to see \
    the actual price path, not just the aggregate order-flow read.";

const GET_VOLUME_PROFILE: &str = "Volume profile over the window: point of control (POC), \
    value area high/low (VAH/VAL), total volume, and the highest-volume nodes.";

const GET_FOOTPRINT: &str = "Per-price bid vs ask volume for the most recent candles. \
    Only available when tick data exists for the window; returns an explicit \
    note when it does not.";

const GET_DELTA_BY_SIZE: &str = "Delta and cumulative delta (CVD) split by order size \
    class (small/medium/large by notional). Use it to answer whether big players \
    and small players are on the same or opposite sides of a move.";

const GET_BAR_DELTA_STATS: &str = "Intra-bar delta extremes and intrabar VWAP per candle: \
    the highest and lowest running delta reached inside each bar and where the bar \
    closed within that range. A bar whose peak delta was strongly positive but \
    closed negative trapped late buyers. Unavailable without tick data.";

const GET_VPIN: &str = "Order-flow toxicity (VPIN) over volume buckets, 0 to 1. High \
    values historically precede volatility bursts; treat it as a risk input for \
    sizing and invalidation width, not as a direction.";

const DETECT_SIZE_DIVERGENCE: &str = "Score the divergence between large-order CVD and \
    small-order CVD over the window. Positive means large players accumulated \
    while small players distributed (or vice versa for negative). Unavailable \
    without tick data.";

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

const GET_RSI: &str = "Relative Strength Index over closes: the last `limit` defined \
    readings, oldest first. Values above 70 are overbought, below 30 oversold -- in a \
    trend they can stay there, so read RSI against the structure, never alone. The \
    numbers come from the analytics core; cite them, do not estimate your own.";

const GET_MACD: &str = "MACD (fast 12 / slow 26 / signal 9 by default): the last `limit` \
    readings of line, signal and histogram. A rising histogram is momentum building; a \
    line crossing the signal is the classic trigger. Warm-up is long -- short windows \
    may define nothing.";

const GET_BOLLINGER_BANDS: &str = "Bollinger Bands (period 20, 2 standard deviations by \
    default): middle/upper/lower, bandwidth and %b for the last `limit` bars. Falling \
    bandwidth is the squeeze; %b above 1 or below 0 is a close outside the bands.";

const GET_ATR: &str = "Average True Range: the latest ATR and ATR as a percent of price. \
    Use it to size stops and targets -- a stop inside one ATR of entry is noise, not \
    structure.";

const GET_MOVING_AVERAGE: &str = "A simple or exponential moving average over closes \
    (`kind`: sma or ema, default ema): the last `limit` defined readings, oldest first. \
    The EMA is SMA-seeded, matching charting-platform convention.";

const DETECT_PATTERN: &str = "Detect a classical chart pattern over confirmed swings: \
    head_and_shoulders, inverse_head_and_shoulders, double_top, double_bottom, triangle, \
    wedge, flag -- or every kind when `pattern` is omitted. Each match carries its \
    anchor swings (`time_ms` values feed create_drawing directly), the entry level whose \
    break activates it, the measured-move target, and the invalidation. A pattern that \
    is not reported did not form; do not describe near-misses as patterns.";

const DETECT_ZONES: &str = "Detect the chart's zones -- the bands a skill's DRAW rules name. \
    Order blocks: the supply/demand bands each structure-breaking impulse started from, with \
    how much of each band price has traded back through (fresh = untouched). Support and \
    resistance: the bands where confirmed swings keep clustering, with their touch counts. \
    Every zone carries absolute anchors, so draw what you cite: create_drawing as a rect with \
    time1/price1 = from_ms/bottom and time2/price2 = to_ms/top. A zone that is not reported \
    did not form; do not sketch one from memory.";

const COMPARE_TIMEFRAMES: &str = "Structural read of one symbol on several timeframes at \
    once: per-timeframe trend and latest swings, which pairs of timeframes agree, and \
    the aligned direction when they all do. Use it before trusting a fine-timeframe \
    signal: a 5m long against a bearish 4h is a counter-trend scalp, not a setup.";

const OPEN_CHART: &str = "Open a chart panel on the user's screen showing `symbol` on \
    `timeframe` (docs/47). Use it when the analysis moves somewhere the screen does not \
    show yet -- a multi-panel read, a different timeframe's zones. The command is issued \
    to the screen, not confirmed: if the analysis depends on what the panel shows, verify \
    with a later screenshot rather than assuming.";

const SET_CHART: &str = "Switch the chart the user is looking at to another `symbol` and/or \
    `timeframe` (docs/47). At least one is required. Use it when the analysis should keep \
    the same panel but look elsewhere. The command is issued, not confirmed -- verify with \
    a later screenshot when it matters.";

const CROSS_TIMEFRAME_CONFLUENCE: &str = "Price levels where several timeframes' swings \
    cluster within `tolerance_pct`: a level three timeframes share is confluence; one \
    timeframe alone is not. Each cluster names its level, the timeframes contributing, \
    and whether it sits above (resistance) or below (support) current price.";

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

const TAKE_SNAPSHOT: &str = "Record what the chart shows right now: the last close, the \
    structure digest (trend and swings), and the user's drawings on the symbol, frozen as one \
    snapshot with your note and tags. Use it before acting on a read you may want to revisit -- \
    'same chart tomorrow, what changed' is what snapshots answer. The capture is stored as yours \
    (created_by: ai), like a drawing.";

const GET_SNAPSHOT: &str = "Read back one snapshot by id, from take_snapshot or the user's own \
    captures. Returns the frozen price, structure digest and drawings exactly as recorded.";

const COMPARE_SNAPSHOTS: &str = "Diff two snapshots -- pass their ids, or just a symbol to \
    compare its two most recent. Reports the price move, whether the structure trend changed, \
    how the swings moved, and which drawings were added or removed between the two captures. \
    This is 'what changed since' answered from records, not from memory.";

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
    let (state, _, _) = state_for(ctx, &symbol, timeframe, lookback).await?;
    Ok(render_state(&state, ctx.capabilities.as_ref()))
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
            Ok((state, _, _)) => frames.push(render_state(&state, ctx.capabilities.as_ref())),
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
    // `i` and `time_ms` are the drawing bridge (docs/47): a skill's DRAW
    // recipe names candles by position, and `time_ms` is the unit
    // create_drawing's anchors take, so neither costs the model a conversion.
    let rendered: Vec<Value> = candles
        .iter()
        .enumerate()
        .map(|(i, candle)| {
            let mut entry = candle_json(candle);
            entry["i"] = json!(i);
            entry["time_ms"] = json!(candle.open_time / 1_000_000);
            entry
        })
        .collect();
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "count": candles.len(),
        "candles": rendered,
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

async fn get_delta_by_size(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_delta_by_size";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "count", TOOL)?);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;
    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let config = SizeClassConfig::default();
    let per_candle = delta_by_size_per_candle(&candles, &trades, &config);
    let cvd = calculate_cvd_by_size(&candles, &trades, &config, true);
    let last = cvd.last();

    let classes = SizeClass::all().iter().map(|class| {
        let index = class.index();
        json!({
            "class": class.as_str(),
            "window_delta": per_candle.iter().map(|b| b.classes[index].delta()).sum::<f64>(),
            "cvd_latest": last.map_or(0.0, |p| p.classes[index]),
            "trades": per_candle.iter().map(|b| b.classes[index].trades).sum::<usize>(),
        })
    }).collect::<Vec<_>>();

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": true,
        "small_below": config.small_below,
        "medium_below": config.medium_below,
        "classes": classes,
        "series": cvd.iter().map(|p| json!({
            "open_time": p.open_time,
            "small": p.classes[0],
            "medium": p.classes[1],
            "large": p.classes[2],
        })).collect::<Vec<_>>(),
    }))
}

async fn get_bar_delta_stats(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_bar_delta_stats";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "count", TOOL)?).min(50);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;
    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let stats = bar_delta_stats_window(&candles, &trades);
    let rendered: Vec<_> = stats
        .iter()
        .filter(|stat| !stat.is_empty())
        .map(|stat| {
            json!({
                "open_time": stat.open_time,
                "delta": stat.delta,
                "max_delta": stat.max_delta,
                "min_delta": stat.min_delta,
                "delta_close_position": stat.delta_close_position(),
                "intrabar_vwap": stat.intrabar_vwap,
                "trades": stat.trades,
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

async fn get_vpin(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_vpin";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "count", TOOL)?).min(200);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;
    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    // One bucket per ~0.5% of the window's volume, matching the route.
    let total: f64 = candles.iter().map(|c| c.volume).sum();
    let config = VpinConfig {
        bucket_volume: (total / 200.0).max(1.0),
        buckets: 50,
    };
    let series = calculate_vpin_series(&trades, &config);
    let latest = series.last();

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": !series.is_empty(),
        "note": series.is_empty()
            .then(|| "not enough volume for a full VPIN window yet".to_string()),
        "latest": latest.map(|p| serde_json::json!({
            "vpin": p.vpin,
            "timestamp": p.timestamp,
        })),
        "interpretation": latest.map(|p| match p.vpin {
            v if v >= 0.6 => "elevated: informed flow likely active, widen invalidation".to_string(),
            v if v >= 0.3 => "normal range".to_string(),
            _ => "calm: flow is balanced".to_string(),
        }),
    }))
}

async fn detect_size_divergence(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_size_divergence";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "count", TOOL)?);
    let (candles, trades) = load_window(ctx, &symbol, timeframe, lookback).await?;
    if trades.is_empty() {
        return Ok(json!({
            "symbol": symbol,
            "timeframe": timeframe.to_string(),
            "available": false,
            "note": NO_TICK_DATA,
        }));
    }

    let config = SizeClassConfig::default();
    let cvd = calculate_cvd_by_size(&candles, &trades, &config, true);
    let small: Vec<f64> = cvd.iter().map(|p| p.classes[SizeClass::Small.index()]).collect();
    let large: Vec<f64> = cvd.iter().map(|p| p.classes[SizeClass::Large.index()]).collect();

    // Normalized Pearson correlation between the small-order and large-order
    // CVD paths: -1 = opposite behavior, +1 = identical behavior.
    let correlation = correlation(&small, &large);
    let large_latest = large.last().copied().unwrap_or(0.0);
    let small_latest = small.last().copied().unwrap_or(0.0);

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "available": true,
        "correlation": correlation,
        "large_cvd": large_latest,
        "small_cvd": small_latest,
        "reading": match correlation {
            c if c < -0.3 => "diverged: large and small players are on opposite sides".to_string(),
            c if c > 0.7 => "aligned: both cohorts flow the same direction".to_string(),
            _ => "mixed".to_string(),
        },
    }))
}

/// Pearson correlation of two equal-length series, `None`-safe: a flat series
/// (zero variance) yields 0.0 rather than NaN.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let mean_a = a.iter().sum::<f64>() / n as f64;
    let mean_b = b.iter().sum::<f64>() / n as f64;
    let covariance: f64 = a.iter().zip(b).map(|(x, y)| (x - mean_a) * (y - mean_b)).sum();
    let var_a: f64 = a.iter().map(|x| (x - mean_a) * (x - mean_a)).sum();
    let var_b: f64 = b.iter().map(|y| (y - mean_b) * (y - mean_b)).sum();
    let denominator = (var_a * var_b).sqrt();
    if denominator.abs() < f64::EPSILON {
        0.0
    } else {
        covariance / denominator
    }
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

// ---------------------------------------------------------------------------
// Classic indicators and pattern tools (docs/45). Every value comes from the
// analytics core -- the model cites readings, it never computes them.
// ---------------------------------------------------------------------------

fn optional_f64(args: &Value, key: &str, tool: &str) -> Result<Option<f64>, AgentError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| AgentError::InvalidToolArgs {
                tool: tool.into(),
                reason: format!("`{key}` must be a number"),
            }),
    }
}

/// The closes of a loaded window, in order.
fn closes_of(candles: &[Candle]) -> Vec<f64> {
    candles.iter().map(|c| c.close).collect()
}

/// The last `limit` defined readings of a series, oldest first, each tagged
/// with its candle's open time (ns) and the millisecond form drawing anchors
/// take. Index `i` of the series is bar `i` of the window by construction.
fn series_tail<T: Copy>(
    series: &[Option<T>],
    candles: &[Candle],
    limit: usize,
    render: impl Fn(i64, i64, T) -> Value,
) -> Vec<Value> {
    let defined: Vec<(usize, T)> = series
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| (i, v)))
        .collect();
    defined
        .iter()
        .rev()
        .take(limit)
        .rev()
        .map(|(i, v)| {
            let t = candles[*i].open_time;
            render(t, t / 1_000_000, *v)
        })
        .collect()
}

async fn get_rsi(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_rsi";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let period = optional_u64(args, "period", TOOL)?.unwrap_or(14).clamp(2, 500) as usize;
    let limit = optional_u64(args, "limit", TOOL)?.unwrap_or(10).clamp(1, 50) as usize;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let series = analytics_core::indicators::rsi(&closes_of(&candles), period);
    let readings = series_tail(&series, &candles, limit, |t, time_ms, v| {
        json!({"t": t, "time_ms": time_ms, "value": v})
    });
    let latest = readings.last().cloned();
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "period": period,
        "latest": latest,
        "zone": readings.last().and_then(|r| r["value"].as_f64()).map(|v| {
            if v >= 70.0 { "overbought" } else if v <= 30.0 { "oversold" } else { "neutral" }
        }),
        "readings": readings,
    }))
}

async fn get_macd(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_macd";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let fast = optional_u64(args, "fast", TOOL)?.unwrap_or(12).clamp(2, 500) as usize;
    let slow = optional_u64(args, "slow", TOOL)?.unwrap_or(26).clamp(2, 500) as usize;
    let signal = optional_u64(args, "signal", TOOL)?.unwrap_or(9).clamp(2, 500) as usize;
    let limit = optional_u64(args, "limit", TOOL)?.unwrap_or(10).clamp(1, 50) as usize;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let series = analytics_core::indicators::macd(&closes_of(&candles), fast, slow, signal);
    let readings = series_tail(&series, &candles, limit, |t, time_ms, p| {
        json!({"t": t, "time_ms": time_ms, "line": p.line, "signal": p.signal,
               "histogram": p.histogram})
    });
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "fast": fast,
        "slow": slow,
        "signal_period": signal,
        "latest": readings.last().cloned(),
        "readings": readings,
    }))
}

async fn get_bollinger_bands(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_bollinger_bands";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let period = optional_u64(args, "period", TOOL)?.unwrap_or(20).clamp(2, 500) as usize;
    let mult = optional_f64(args, "mult", TOOL)?.unwrap_or(2.0).clamp(0.1, 5.0);
    let limit = optional_u64(args, "limit", TOOL)?.unwrap_or(10).clamp(1, 50) as usize;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let series = analytics_core::indicators::bollinger(&closes_of(&candles), period, mult);
    let readings = series_tail(&series, &candles, limit, |t, time_ms, p| {
        json!({"t": t, "time_ms": time_ms, "middle": p.middle, "upper": p.upper,
               "lower": p.lower, "bandwidth": p.bandwidth, "percent_b": p.percent_b})
    });
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "period": period,
        "mult": mult,
        "latest": readings.last().cloned(),
        "readings": readings,
    }))
}

async fn get_atr_tool(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_atr";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let period = optional_u64(args, "period", TOOL)?.unwrap_or(14).clamp(2, 500) as usize;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let atr_series = analytics_core::indicators::atr(&candles, period);
    let pct_series = analytics_core::indicators::atr_percent(&candles, period);
    let latest_atr = atr_series.iter().rev().flatten().next().copied();
    let latest_pct = pct_series.iter().rev().flatten().next().copied();
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "period": period,
        "atr": latest_atr,
        "atr_percent": latest_pct,
    }))
}

async fn get_moving_average(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_moving_average";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("ema")
        .to_string();
    if kind != "ema" && kind != "sma" {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: format!("`kind` must be ema or sma; got `{kind}`"),
        });
    }
    let period = optional_u64(args, "period", TOOL)?.unwrap_or(20).clamp(2, 500) as usize;
    let limit = optional_u64(args, "limit", TOOL)?.unwrap_or(10).clamp(1, 50) as usize;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let closes = closes_of(&candles);
    let series = if kind == "ema" {
        analytics_core::indicators::ema(&closes, period)
    } else {
        analytics_core::indicators::sma(&closes, period)
    };
    let readings = series_tail(&series, &candles, limit, |t, time_ms, v| {
        json!({"t": t, "time_ms": time_ms, "value": v})
    });
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "kind": kind,
        "period": period,
        "latest": readings.last().cloned(),
        "readings": readings,
    }))
}

async fn detect_pattern_tool(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_pattern";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let pattern = match args.get("pattern").and_then(Value::as_str) {
        None => None,
        Some(raw) => Some(analytics_core::PatternKind::from_name(raw).ok_or_else(|| {
            AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: format!(
                    "`pattern` must be one of {}; got `{raw}`",
                    analytics_core::PatternKind::ALL
                        .iter()
                        .map(|k| k.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        })?),
    };
    let tolerance_pct = optional_f64(args, "tolerance_pct", TOOL)?
        .unwrap_or(0.004)
        .clamp(0.0005, 0.05);
    let min_confidence = optional_f64(args, "min_confidence", TOOL)?
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let structure = detect_market_structure(&candles, ctx.config.structure);
    let config = analytics_core::PatternConfig {
        tolerance_pct,
        ..analytics_core::PatternConfig::default()
    };
    let mut matches = analytics_core::detect_patterns(&candles, &structure, pattern, &config);
    matches.retain(|m| m.confidence >= min_confidence);

    // Anchor times arrive in the candles' own unit (ns from the store);
    // `time_ms` is the form create_drawing's anchors take, so a detected
    // pattern can be drawn without the model converting anything.
    let render = |m: &analytics_core::PatternMatch| {
        json!({
            "kind": m.kind.name(),
            "direction": serde_json::to_value(m.direction).unwrap_or_default(),
            "confidence": m.confidence,
            "anchors": m.anchors.iter().map(|p| json!({
                "time_ms": p.timestamp / 1_000_000,
                "price": p.price,
                "kind": match p.kind {
                    analytics_core::SwingKind::High => "high",
                    analytics_core::SwingKind::Low => "low",
                },
            })).collect::<Vec<_>>(),
            "entry_level": m.entry_level,
            "target": m.target,
            "invalidation": m.invalidation,
            "summary": m.summary,
        })
    };
    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "count": matches.len(),
        "patterns": matches.iter().map(render).collect::<Vec<_>>(),
        "note": "an empty list means no pattern formed on the confirmed swings; a near-miss is a miss",
    }))
}

/// The zones a chart is drawn in (`docs/47`): supply/demand bands from
/// [`analytics_core::detect_zones`] — the order-block concept, the origin of
/// each structure-breaking impulse — plus support/resistance bands from
/// [`analytics_core::detect_sr_zones`], where confirmed swings keep
/// clustering.
///
/// Every zone carries absolute anchors in both units, because the point of
/// the tool is drawing: a rect from `from_ms`/`bottom` to `to_ms`/`top` is
/// the zone on the user's chart, and the model should not have to convert
/// anything to place it there.
async fn detect_zones_tool(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "detect_zones";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;

    let regions = analytics_core::detect_zones(&candles, Default::default());
    let structure = detect_market_structure(&candles, ctx.config.structure);
    let last_close = candles.last().map_or(0.0, |c| c.close);
    let sr = analytics_core::detect_sr_zones(&structure, last_close, Default::default());

    let order_blocks: Vec<Value> = regions
        .iter()
        .rev() // newest first: the fresh zone is the one worth trading
        .map(|r| {
            json!({
                "name": r.name,
                "direction": if r.side == analytics_core::Side::Buy { "bullish" } else { "bearish" },
                "top": r.price_high,
                "bottom": r.price_low,
                "from_ms": r.from / 1_000_000,
                "to_ms": r.to / 1_000_000,
                "mitigated": r.mitigated,
                "fresh": r.is_fresh(),
                "broken_level": r.origin.broken_level(),
            })
        })
        .collect();
    let sr_zones: Vec<Value> = sr
        .iter()
        .map(|z| {
            json!({
                "kind": z.kind.name(),
                "top": z.top,
                "bottom": z.bottom,
                "touches": z.touches,
                "first_time_ms": z.first_time / 1_000_000,
                "last_time_ms": z.last_time / 1_000_000,
            })
        })
        .collect();

    Ok(json!({
        "symbol": symbol,
        "timeframe": timeframe.to_string(),
        "price": last_close,
        "order_blocks": order_blocks,
        "sr_zones": sr_zones,
        "note": "draw what you cite: a rect with time1/price1 = from_ms/bottom and \
                 time2/price2 = to_ms/top puts the zone on the chart. An empty list \
                 means no zone formed; do not sketch one from memory.",
    }))
}

/// `open_chart` / `set_chart` (`docs/47`): the tools do not touch a screen --
/// they return the command, and the orchestrator relays it to the client over
/// the progress channel. Confirmed-not-seen: the note the model gets back
/// says the command was *issued*, and a model that needs to know what the
/// screen did must look at a later screenshot rather than trust the relay.
fn ui_command(action: &str, symbol: Option<String>, timeframe: Option<String>) -> Value {
    json!({
        "ok": true,
        "ui_command": { "action": action, "symbol": symbol, "timeframe": timeframe },
        "note": "the command was issued to the user's screen, not confirmed. If the analysis \
                 depends on what the screen now shows, verify with a screenshot rather than \
                 assuming the panel opened.",
    })
}

async fn open_chart_tool(_ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "open_chart";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframe = timeframe_arg(args, TOOL)?;
    Ok(ui_command(
        "open_chart",
        Some(symbol.to_uppercase()),
        Some(timeframe.to_string()),
    ))
}

async fn set_chart_tool(_ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "set_chart";
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase());
    let timeframe = match args.get("timeframe").and_then(Value::as_str) {
        None => None,
        Some(raw) => Some(
            raw.parse::<Timeframe>()
                .map_err(|_| AgentError::InvalidToolArgs {
                    tool: TOOL.into(),
                    reason: format!("`timeframe` is not a resolution the engine accepts: `{raw}`"),
                })?
                .to_string(),
        ),
    };
    if symbol.is_none() && timeframe.is_none() {
        return Err(AgentError::InvalidToolArgs {
            tool: TOOL.into(),
            reason: "at least one of `symbol` or `timeframe` is required -- a command that \
                     changes nothing is not a command"
                .into(),
        });
    }
    Ok(ui_command("set_chart", symbol, timeframe))
}

/// The `timeframes` array both multi-timeframe tools take: 2 to 6 entries,
/// each a resolution the engine accepts.
fn timeframes_arg(args: &Value, tool: &str) -> Result<Vec<Timeframe>, AgentError> {
    let raw = args
        .get("timeframes")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "`timeframes` is required and must be an array".into(),
        })?;
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let s = entry.as_str().ok_or_else(|| AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "every `timeframes` entry must be a string".into(),
        })?;
        out.push(s.parse::<Timeframe>().map_err(|_| AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: format!(
                "`timeframes` entries must be one of {}; got `{s}`",
                Timeframe::all()
                    .iter()
                    .rev()
                    .map(|tf| tf.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?);
    }
    if !(2..=6).contains(&out.len()) {
        return Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: format!("`timeframes` needs 2 to 6 entries; got {}", out.len()),
        });
    }
    Ok(out)
}

async fn compare_timeframes(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "compare_timeframes";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframes = timeframes_arg(args, TOOL)?;
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);

    let mut frames = Vec::with_capacity(timeframes.len());
    for tf in &timeframes {
        let (candles, _) = load_window(ctx, &symbol, *tf, lookback).await?;
        let structure = detect_market_structure(&candles, ctx.config.structure);
        frames.push(json!({
            "timeframe": tf.to_string(),
            "trend": format!("{:?}", structure.trend),
            "latest_swing_high": structure.latest_swing_high(),
            "latest_swing_low": structure.latest_swing_low(),
            "last_close": candles.last().map(|c| c.close),
        }));
    }

    // Pairwise agreement: which pairs of timeframes read the same trend.
    let mut agreements = Vec::new();
    for i in 0..frames.len() {
        for j in (i + 1)..frames.len() {
            agreements.push(json!({
                "pair": [frames[i]["timeframe"], frames[j]["timeframe"]],
                "agree": frames[i]["trend"] == frames[j]["trend"],
            }));
        }
    }
    let first_trend = frames[0]["trend"].clone();
    let aligned = frames.iter().all(|f| f["trend"] == first_trend)
        && first_trend != "Ranging";
    Ok(json!({
        "symbol": symbol,
        "frames": frames,
        "agreements": agreements,
        "aligned_direction": if aligned { first_trend } else { Value::Null },
    }))
}

async fn cross_timeframe_confluence(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "cross_timeframe_confluence";
    let symbol = string_arg(args, "symbol", TOOL)?;
    let timeframes = timeframes_arg(args, TOOL)?;
    let tolerance_pct = optional_f64(args, "tolerance_pct", TOOL)?
        .unwrap_or(0.0015)
        .clamp(0.0002, 0.02);
    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);

    // Every confirmed swing on every timeframe, tagged with its timeframe.
    let mut levels: Vec<(f64, String)> = Vec::new();
    let mut current_price: Option<f64> = None;
    for tf in &timeframes {
        let (candles, _) = load_window(ctx, &symbol, *tf, lookback).await?;
        let structure = detect_market_structure(&candles, ctx.config.structure);
        for price in structure.swing_highs.iter().chain(&structure.swing_lows) {
            levels.push((*price, tf.to_string()));
        }
        // The last timeframe listed is the finest the caller named; its last
        // close is "current price" for the support/resistance label.
        current_price = candles.last().map(|c| c.close);
    }

    // Greedy clustering over sorted prices: a new level joins the open cluster
    // while it sits within tolerance of the cluster's running mean.
    levels.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut clusters: Vec<(Vec<f64>, Vec<String>)> = Vec::new();
    for (price, tf) in levels {
        let joins = clusters.last().is_some_and(|(prices, _)| {
            let mean = prices.iter().sum::<f64>() / prices.len() as f64;
            price <= mean * (1.0 + tolerance_pct)
        });
        if joins {
            let (prices, tfs) = clusters.last_mut().expect("checked above");
            prices.push(price);
            if !tfs.contains(&tf) {
                tfs.push(tf);
            }
        } else {
            clusters.push((vec![price], vec![tf]));
        }
    }

    let confluent: Vec<Value> = clusters
        .iter()
        .filter(|(_, tfs)| tfs.len() >= 2)
        .map(|(prices, tfs)| {
            let level = prices.iter().sum::<f64>() / prices.len() as f64;
            json!({
                "level": level,
                "timeframes": tfs,
                "touches": prices.len(),
                "side": current_price.map(|p| if level > p { "resistance" } else { "support" }),
            })
        })
        .collect();
    Ok(json!({
        "symbol": symbol,
        "tolerance_pct": tolerance_pct,
        "current_price": current_price,
        "count": confluent.len(),
        "clusters": confluent,
        "note": "a level only one timeframe has is not listed; confluence means several timeframes share it",
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

// ---------------------------------------------------------------------------
// Snapshot tools (docs/45). A snapshot freezes what the chart shows -- last
// close, structure digest, the user's drawings -- so "what changed since" is
// answered from records rather than from the model's memory of the last run.
// ---------------------------------------------------------------------------

/// The store and the identity it is scoped to, or the honest absence.
fn snapshot_parts<'a>(
    ctx: &'a ToolContext<'_>,
    tool: &str,
) -> Result<(&'a dyn SnapshotStore, &'a str), AgentError> {
    match (ctx.snapshots, ctx.user_id) {
        (Some(store), Some(user_id)) => Ok((store, user_id)),
        _ => Err(AgentError::InvalidToolArgs {
            tool: tool.into(),
            reason: "no snapshot store is attached, so nothing can be captured \
                     or read back. Say so rather than implying a snapshot exists."
                .into(),
        }),
    }
}

/// The digest a snapshot freezes: the same shape `detect_market_structure`
/// reports, plus the close. Built from the candles at capture time -- never
/// from a previous run's memory of them.
///
/// Public because the gateway's `POST /chart-snapshots` freezes the user's
/// own captures with it: two writers, one digest shape, or the compare tool
/// would read the two kinds of snapshot differently.
#[must_use]
pub fn structure_digest(candles: &[Candle], config: MarketStateConfig) -> Value {
    let structure = detect_market_structure(candles, config.structure);
    json!({
        "trend": format!("{:?}", structure.trend),
        "swing_highs": structure.swing_highs.iter().rev().take(8).copied().collect::<Vec<_>>(),
        "swing_lows": structure.swing_lows.iter().rev().take(8).copied().collect::<Vec<_>>(),
        "recent_breaks": structure.breaks.iter().rev().take(8).map(|b| json!({
            "kind": format!("{:?}", b.kind),
            "direction": format!("{:?}", b.direction),
            "level": b.level,
            "price": b.price,
        })).collect::<Vec<_>>(),
        "bars": candles.len(),
    })
}

/// `take_snapshot`: freeze the chart state and store it as the agent's own.
async fn take_snapshot(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "take_snapshot";
    let symbol = string_arg(args, "symbol", TOOL)?.to_uppercase();
    let timeframe = timeframe_arg(args, TOOL)?;
    let note = match args.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => {
            return Err(AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: "`note` must be a non-empty string when given".into(),
            })
        }
    };
    let tags: Vec<String> = match args.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter_map(|e| e.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
            .collect(),
        Some(_) => {
            return Err(AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: "`tags` must be an array of strings when given".into(),
            })
        }
    };
    let (store, user_id) = snapshot_parts(ctx, TOOL)?;

    let lookback = ctx.clamp_lookback(optional_u64(args, "lookback", TOOL)?);
    let (candles, _) = load_window(ctx, &symbol, timeframe, lookback).await?;
    let price = candles.last().map(|c| c.close).ok_or_else(|| AgentError::ToolFailed {
        tool: TOOL.into(),
        reason: format!("no candles for {symbol} {timeframe}: a snapshot of nothing is not a snapshot"),
    })?;
    let structure = structure_digest(&candles, ctx.config.clone());

    // The drawings are frozen as the read tool reports them, ids included --
    // the compare tool's added/removed diff is by identity, and that identity
    // is only there if the capture carried it.
    let drawings: Vec<Value> = match ctx.drawings {
        Some(source) => source
            .drawings(user_id, &symbol)
            .await?
            .iter()
            .map(|d| serde_json::to_value(d).unwrap_or_default())
            .collect(),
        // No drawings source: the snapshot still stands, and the frozen list
        // is empty -- the same honest posture the read tool takes.
        None => Vec::new(),
    };
    let drawing_count = drawings.len();

    let captured = store
        .capture(
            user_id,
            &NewSnapshot {
                symbol: symbol.clone(),
                timeframe: timeframe.to_string(),
                price,
                drawings: Value::Array(drawings),
                structure: structure.clone(),
                note: note.clone(),
                tags,
                created_by: "ai".into(),
            },
        )
        .await?;

    Ok(json!({
        "captured": true,
        "id": captured.id,
        "symbol": captured.symbol,
        "timeframe": captured.timeframe,
        "price": price,
        "trend": structure["trend"],
        "drawings": drawing_count,
        "note": note,
        "created_at_ms": captured.created_at / 1_000_000,
    }))
}

/// `get_snapshot`: read one capture back.
async fn get_snapshot_tool(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "get_snapshot";
    let id = string_arg(args, "id", TOOL)?;
    let (store, user_id) = snapshot_parts(ctx, TOOL)?;
    match store.get(user_id, &id).await? {
        Some(snapshot) => Ok(serde_json::to_value(snapshot).unwrap_or_default()),
        None => Ok(json!({
            "found": false,
            "id": id,
            "note": "no snapshot under that id for this user. If it was never captured, that is the answer.",
        })),
    }
}

/// How long between two captures, in the words a summary reads well in.
fn gap_words(ns: i64) -> String {
    let minutes = ns / 60_000_000_000;
    if minutes < 90 {
        format!("{minutes}m")
    } else if minutes < 60 * 36 {
        format!("{:.1}h", minutes as f64 / 60.0)
    } else {
        format!("{:.1}d", minutes as f64 / 1440.0)
    }
}

/// One drawing's identity for the diff: its id when it has one, else the
/// kind-and-anchors signature, which is what "the same shape" means for a
/// source without ids.
fn drawing_identity(drawing: &Value) -> String {
    if let Some(id) = drawing.get("id").and_then(Value::as_str) {
        return format!("id:{id}");
    }
    format!(
        "sig:{}|{}|{}|{}|{}",
        drawing.get("kind").and_then(Value::as_str).unwrap_or("?"),
        drawing["time1_ms"],
        drawing["price1"],
        drawing["time2_ms"],
        drawing["price2"],
    )
}

/// A drawing's one-line description for the diff's added/removed lists.
fn drawing_line(drawing: &Value) -> Value {
    json!({
        "kind": drawing.get("kind").and_then(Value::as_str).unwrap_or("?"),
        "label": drawing.get("label").and_then(Value::as_str),
        "price": drawing.get("price2").and_then(Value::as_f64)
            .or_else(|| drawing.get("price1").and_then(Value::as_f64)),
    })
}

/// `compare_snapshots`: two captures in, "what changed" out.
async fn compare_snapshots(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, AgentError> {
    const TOOL: &str = "compare_snapshots";
    let (store, user_id) = snapshot_parts(ctx, TOOL)?;

    // Two ids, or a symbol whose two most recent captures are compared.
    let (a, b) = match (
        args.get("id_a").and_then(Value::as_str),
        args.get("id_b").and_then(Value::as_str),
        args.get("symbol").and_then(Value::as_str),
    ) {
        (Some(id_a), Some(id_b), _) => {
            let a = store.get(user_id, id_a).await?;
            let b = store.get(user_id, id_b).await?;
            match (a, b) {
                (Some(a), Some(b)) => (a, b),
                (missing_a, missing_b) => {
                    return Ok(json!({
                        "compared": false,
                        "note": format!(
                            "one or both snapshots were not found for this user: \
                             id_a {}, id_b {}",
                            if missing_a.is_none() { "missing" } else { "found" },
                            if missing_b.is_none() { "missing" } else { "found" },
                        ),
                    }));
                }
            }
        }
        (None, None, Some(symbol)) => {
            let recent = store.list(user_id, &symbol.to_uppercase(), 2).await?;
            if recent.len() < 2 {
                return Ok(json!({
                    "compared": false,
                    "note": format!(
                        "{} has {} snapshot(s); a compare needs two. Take one now \
                         and another later, and this tool answers what changed.",
                        symbol.to_uppercase(),
                        recent.len()
                    ),
                }));
            }
            // Newest first: `b` is the later capture.
            (recent[1].clone(), recent[0].clone())
        }
        _ => {
            return Err(AgentError::InvalidToolArgs {
                tool: TOOL.into(),
                reason: "pass `id_a` and `id_b`, or just `symbol` for its two \
                         most recent snapshots"
                    .into(),
            })
        }
    };

    let price_change_pct = if a.price > 0.0 {
        (b.price - a.price) / a.price * 100.0
    } else {
        0.0
    };
    let trend_a = a.structure.get("trend").and_then(Value::as_str).unwrap_or("?");
    let trend_b = b.structure.get("trend").and_then(Value::as_str).unwrap_or("?");
    let trend_changed = trend_a != trend_b;

    let highs_a = a.structure.get("swing_highs").and_then(Value::as_array);
    let highs_b = b.structure.get("swing_highs").and_then(Value::as_array);
    let swing_high_moved = highs_a.and_then(|h| h.first())
        != highs_b.and_then(|h| h.first());
    let lows_a = a.structure.get("swing_lows").and_then(Value::as_array);
    let lows_b = b.structure.get("swing_lows").and_then(Value::as_array);
    let swing_low_moved = lows_a.and_then(|l| l.first())
        != lows_b.and_then(|l| l.first());

    let drawings_a = a.drawings.as_array().cloned().unwrap_or_default();
    let drawings_b = b.drawings.as_array().cloned().unwrap_or_default();
    let ids_a: std::collections::HashMap<String, &Value> = drawings_a
        .iter()
        .map(|d| (drawing_identity(d), d))
        .collect();
    let ids_b: std::collections::HashMap<String, &Value> = drawings_b
        .iter()
        .map(|d| (drawing_identity(d), d))
        .collect();
    let added: Vec<Value> = drawings_b
        .iter()
        .filter(|d| !ids_a.contains_key(&drawing_identity(d)))
        .map(drawing_line)
        .collect();
    let removed: Vec<Value> = drawings_a
        .iter()
        .filter(|d| !ids_b.contains_key(&drawing_identity(d)))
        .map(drawing_line)
        .collect();

    let gap = gap_words(b.created_at.saturating_sub(a.created_at));
    let summary = format!(
        "{} moved {:+.2}% between the captures ({} apart); trend {} -> {}{}; {} drawing(s) added, {} removed.",
        b.symbol,
        price_change_pct,
        gap,
        trend_a,
        trend_b,
        if trend_changed { " (changed)" } else { "" },
        added.len(),
        removed.len(),
    );

    Ok(json!({
        "compared": true,
        "symbol": b.symbol,
        "timeframe": b.timeframe,
        "from": {"id": a.id, "created_at_ms": a.created_at / 1_000_000},
        "to": {"id": b.id, "created_at_ms": b.created_at / 1_000_000},
        "gap": gap,
        "price_change_pct": price_change_pct,
        "trend": {"from": trend_a, "to": trend_b, "changed": trend_changed},
        "swings": {"high_moved": swing_high_moved, "low_moved": swing_low_moved},
        "drawings": {"added": added, "removed": removed},
        "summary": summary,
        "different_symbols": a.symbol != b.symbol,
    }))
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
///
/// Every render carries a `provenance` block (docs/39): the observed facts
/// from the state's own [`analytics_core::StateProvenance`], plus the
/// registry's declared labels when the host attached a `CapabilityView`. This
/// replaced the ad-hoc `data_note` patch: "absorption is empty because no
/// trades exist" is now a labelled section, not a footnote.
fn render_state(state: &MarketState, view: Option<&crate::capability_view::CapabilityView>) -> Value {
    let provenance = match view {
        Some(view) => view.state_provenance(state),
        None => crate::capability_view::observed_provenance(state),
    };
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

    if let Some(object) = value.as_object_mut() {
        object.insert("provenance".to_string(), provenance);
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
            "get_rsi",
            "get_macd",
            "get_bollinger_bands",
            "get_atr",
            "get_moving_average",
            "detect_pattern",
            "detect_zones",
            "open_chart",
            "set_chart",
            "compare_timeframes",
            "cross_timeframe_confluence",
            "analyze_timeframe",
            "analyze_multi_timeframe",
            "backtest_strategy",
            "backtest_similar_setups",
            "get_user_drawings",
            "take_snapshot",
            "get_snapshot",
            "compare_snapshots",
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
        // No trades in the fixture: the structured provenance block says so,
        // per section (docs/39) — the `data_note` footnote is gone.
        assert_eq!(out["provenance"]["trades"], 0);
        assert_eq!(out["provenance"]["sections"]["absorption"], "unavailable");
        assert!(
            out["provenance"]["why"]
                .as_array()
                .expect("a why list")
                .iter()
                .any(|w| w.as_str().is_some_and(|s| s.contains("no trades"))),
            "{}",
            out["provenance"]
        );
    }

    #[tokio::test]
    async fn a_capability_view_attaches_the_registrys_answer_to_tool_results() {
        use capabilities::profile::{Channel, ClassProfile, KindSupport, ProviderDataProfile};
        use capabilities::{DataKind, Provider, SplitQuality, SymbolClass};

        // A Bybit-shaped registry: candles real live, attributed over REST,
        // trades live-only. The fixture holds no trades, so the state's split
        // sections must render *derived* — the audit's provenance-leak finding,
        // pinned at the tool boundary.
        let bybit = ProviderDataProfile::new(Provider::Bybit).with_class(
            SymbolClass::Spot,
            ClassProfile::new()
                .with(
                    DataKind::Candles,
                    KindSupport::both(
                        Channel::candles(SplitQuality::Real),
                        Channel::candles(SplitQuality::Attributed)
                            .with_note("attributed, not a measurement"),
                    ),
                )
                .with(DataKind::Trades, KindSupport::live_only(Channel::plain())),
        );
        let registry = std::sync::Arc::new(capabilities::Registry::new(
            capabilities::descriptor::STANDARD,
            vec![bybit],
        ));
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture).with_capabilities(
            crate::capability_view::CapabilityView::new(registry, Provider::Bybit, SymbolClass::Spot),
        );
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

        // The render labels its sections...
        assert_eq!(out["provenance"]["sections"]["delta"], "derived");
        assert_eq!(out["provenance"]["sections"]["absorption"], "unavailable");
        // ...and dispatch attached the registry's per-tool block. There is no
        // catalogued `analyze_timeframe` capability (it is the aggregate), so
        // the block comes from a tool that has one.
        let out = ToolRegistry::market_analysis()
            .execute(
                &call("get_delta", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["provenance"]["capability"], "delta");
        assert_eq!(out["provenance"]["availability"], "available");
        assert!(
            out["provenance"]["caveats"]
                .as_array()
                .expect("caveats")
                .iter()
                .any(|c| c.as_str().is_some_and(|s| s.contains("live-window only"))),
            "{}",
            out["provenance"]
        );
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

    #[tokio::test]
    async fn get_candles_indexes_each_candle_and_carries_drawing_ready_millis() {
        // The drawing bridge (docs/47): a skill's DRAW recipe names a candle by
        // position, and `time_ms` is the unit create_drawing's anchors take.
        // Both are the tool's to compute -- a model counting a raw array
        // miscounts, and a model converting nanoseconds drops zeros.
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let out = ToolRegistry::market_analysis()
            .execute(
                &call("get_candles", json!({"symbol": "BTCUSDT", "timeframe": "5m", "limit": 10})),
                &ctx,
            )
            .await
            .unwrap();
        let candles = out["candles"].as_array().unwrap();
        assert_eq!(candles.len(), 10);
        for (i, candle) in candles.iter().enumerate() {
            assert_eq!(candle["i"].as_u64().unwrap(), i as u64, "index of candle {i}");
            assert_eq!(
                candle["time_ms"].as_i64().unwrap(),
                candle["t"].as_i64().unwrap() / 1_000_000,
                "time_ms is t in milliseconds"
            );
        }
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
            id: None,
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
                id: None,
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

    // ------------------------------------------------------------------
    // Classic indicators and pattern tools (docs/45)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn get_rsi_returns_defined_readings_once_warmed_up() {
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("get_rsi", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["period"], 14);
        let readings = out["readings"].as_array().expect("a readings list");
        assert_eq!(readings.len(), 10, "the default limit");
        assert!(
            readings.iter().all(|r| r["value"].is_f64() && r["time_ms"].is_i64()),
            "every reading carries its value and its anchor-ready time: {readings:?}"
        );
        // A strictly rising series pins RSI at 100.
        assert_eq!(out["zone"], "overbought");
    }

    #[tokio::test]
    async fn get_macd_reports_line_signal_and_histogram() {
        let fixture = Fixture::rising(60);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("get_macd", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        let latest = out["latest"].as_object().expect("60 bars warm the default MACD up");
        assert!(latest["line"].is_f64() && latest["signal"].is_f64() && latest["histogram"].is_f64());
    }

    #[tokio::test]
    async fn get_bollinger_bands_reports_ordered_bands() {
        let fixture = Fixture::oscillating(60);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("get_bollinger_bands", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        let latest = &out["latest"];
        assert!(latest["upper"].as_f64().unwrap() >= latest["middle"].as_f64().unwrap());
        assert!(latest["middle"].as_f64().unwrap() >= latest["lower"].as_f64().unwrap());
        assert!(latest["bandwidth"].as_f64().unwrap() > 0.0);
    }

    #[tokio::test]
    async fn get_atr_reports_price_and_percent() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("get_atr", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out["atr"].as_f64().expect("atr") > 0.0);
        assert!(out["atr_percent"].as_f64().expect("atr percent") > 0.0);
    }

    #[tokio::test]
    async fn detect_zones_reports_drawing_ready_bands() {
        // The zones tool is the drawing bridge (docs/47): every band must
        // carry the anchors a rect takes, in milliseconds, so the model can
        // draw what it cites without converting anything.
        let fixture = Fixture::oscillating(120);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("detect_zones", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out["price"].as_f64().unwrap() > 0.0);
        for zone in out["sr_zones"].as_array().unwrap() {
            for key in ["kind", "top", "bottom", "touches", "first_time_ms", "last_time_ms"] {
                assert!(zone.get(key).is_some(), "an sr zone lost `{key}`: {zone}");
            }
            assert!(zone["top"].as_f64().unwrap() >= zone["bottom"].as_f64().unwrap());
        }
        for block in out["order_blocks"].as_array().unwrap() {
            for key in ["name", "direction", "top", "bottom", "from_ms", "to_ms", "mitigated", "fresh"] {
                assert!(block.get(key).is_some(), "an order block lost `{key}`: {block}");
            }
        }
    }

    #[tokio::test]
    async fn the_screen_tools_return_the_command_the_orchestrator_relays() {
        // docs/47: the tools issue, the orchestrator relays, the screen
        // executes -- and the note owns that issuance is not confirmation.
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("open_chart", json!({"symbol": "ethusdt", "timeframe": "1h"})),
                &ctx,
            )
            .await
            .unwrap();
        let command = &out["ui_command"];
        assert_eq!(command["action"], "open_chart");
        assert_eq!(command["symbol"], "ETHUSDT", "symbols render as the venue spells them");
        assert_eq!(command["timeframe"], "1h");
        assert!(out["note"].as_str().unwrap().contains("not confirmed"));
    }

    #[tokio::test]
    async fn set_chart_refuses_a_command_that_changes_nothing() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = registry()
            .execute(&call("set_chart", json!({})), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("at least one"), "{err}");
        // ...and a partial command is fine: timeframe alone retargets the pane.
        let out = registry()
            .execute(&call("set_chart", json!({"timeframe": "4h"})), &ctx)
            .await
            .unwrap();
        assert_eq!(out["ui_command"]["action"], "set_chart");
        assert_eq!(out["ui_command"]["timeframe"], "4h");
        assert!(out["ui_command"]["symbol"].is_null());
    }

    #[tokio::test]
    async fn get_moving_average_rejects_an_unknown_kind() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let err = registry()
            .execute(
                &call(
                    "get_moving_average",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m", "kind": "wma"}),
                ),
                &ctx,
            )
            .await
            .expect_err("wma is not a kind this tool serves");
        assert!(matches!(err, AgentError::InvalidToolArgs { .. }));

        let out = registry()
            .execute(
                &call(
                    "get_moving_average",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m", "kind": "sma"}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["kind"], "sma");
    }

    #[tokio::test]
    async fn detect_pattern_rejects_an_unknown_pattern_by_name() {
        let fixture = Fixture::oscillating(80);
        let ctx = ToolContext::new(&fixture);
        let err = registry()
            .execute(
                &call(
                    "detect_pattern",
                    json!({"symbol": "BTCUSDT", "timeframe": "5m", "pattern": "cup_and_handle"}),
                ),
                &ctx,
            )
            .await
            .expect_err("must refuse");
        let AgentError::InvalidToolArgs { reason, .. } = err else {
            panic!("got {err}")
        };
        assert!(
            reason.contains("head_and_shoulders") && reason.contains("double_top"),
            "the refusal names the vocabulary: {reason}"
        );
    }

    #[tokio::test]
    async fn detect_pattern_reports_anchor_ready_matches_or_none() {
        let fixture = Fixture::oscillating(80);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call("detect_pattern", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .unwrap();
        let patterns = out["patterns"].as_array().expect("a patterns list");
        for m in patterns {
            assert!(
                m["entry_level"].is_f64() && m["target"].is_f64() && m["invalidation"].is_f64(),
                "a match without its trade geometry is a label, not a signal: {m}"
            );
            for anchor in m["anchors"].as_array().expect("anchors") {
                assert!(
                    anchor["time_ms"].is_i64() && anchor["price"].is_f64(),
                    "anchors must drop straight into create_drawing: {anchor}"
                );
            }
        }
    }

    #[tokio::test]
    async fn compare_timeframes_reports_every_frame_and_the_pairwise_agreements() {
        let fixture = Fixture::oscillating(60);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call(
                    "compare_timeframes",
                    json!({"symbol": "BTCUSDT", "timeframes": ["5m", "15m", "1h"]}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out["frames"].as_array().expect("frames").len(), 3);
        // 3 frames make 3 pairs.
        assert_eq!(out["agreements"].as_array().expect("agreements").len(), 3);
    }

    #[tokio::test]
    async fn cross_timeframe_confluence_lists_only_levels_several_timeframes_share() {
        // The fixture serves the same series on every timeframe, so every
        // swing is shared by construction -- the clusters must name both.
        let fixture = Fixture::oscillating(60);
        let ctx = ToolContext::new(&fixture);
        let out = registry()
            .execute(
                &call(
                    "cross_timeframe_confluence",
                    json!({"symbol": "BTCUSDT", "timeframes": ["5m", "1h"]}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        let clusters = out["clusters"].as_array().expect("clusters");
        assert!(!clusters.is_empty(), "identical series must conflate");
        for cluster in clusters {
            let tfs = cluster["timeframes"].as_array().expect("timeframes");
            assert!(tfs.len() >= 2, "confluence means shared: {cluster}");
            assert!(cluster["level"].is_f64());
            assert!(cluster["side"].is_string(), "support or resistance, named");
        }
    }

    #[tokio::test]
    async fn a_single_timeframe_is_not_a_comparison() {
        let fixture = Fixture::rising(40);
        let ctx = ToolContext::new(&fixture);
        let err = registry()
            .execute(
                &call(
                    "compare_timeframes",
                    json!({"symbol": "BTCUSDT", "timeframes": ["5m"]}),
                ),
                &ctx,
            )
            .await
            .expect_err("one timeframe is not a comparison");
        assert!(matches!(err, AgentError::InvalidToolArgs { .. }));
    }

    // -----------------------------------------------------------------------
    // Snapshot tools (docs/45)
    // -----------------------------------------------------------------------

    /// A store over an in-memory list, recording what it was asked to keep.
    struct SnapshotFixture {
        rows: std::sync::Mutex<Vec<crate::snapshots::ChartSnapshot>>,
    }

    impl SnapshotFixture {
        fn new() -> Self {
            Self {
                rows: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// Seed a capture directly, for compare tests that should not depend
        /// on what a fixture's candles happen to digest into.
        fn seed(&self, id: &str, price: f64, trend: &str, drawing_ids: &[&str], at_ns: i64) {
            let drawings: Vec<Value> = drawing_ids
                .iter()
                .map(|id| json!({"id": id, "kind": "hline", "price1": price}))
                .collect();
            self.rows.lock().expect("lock").push(crate::snapshots::ChartSnapshot {
                id: id.into(),
                symbol: "BTCUSDT".into(),
                timeframe: "1h".into(),
                price,
                drawings: Value::Array(drawings),
                structure: json!({"trend": trend, "swing_highs": [price + 100.0], "swing_lows": [price - 100.0]}),
                note: None,
                tags: vec![],
                created_by: "ai".into(),
                created_at: at_ns,
            });
        }

        fn len(&self) -> usize {
            self.rows.lock().expect("lock").len()
        }
    }

    #[async_trait]
    impl crate::snapshots::SnapshotStore for SnapshotFixture {
        async fn capture(
            &self,
            _user_id: &str,
            snapshot: &crate::snapshots::NewSnapshot,
        ) -> Result<crate::snapshots::ChartSnapshot, AgentError> {
            let n = self.rows.lock().expect("lock").len() + 1;
            let row = crate::snapshots::ChartSnapshot {
                id: format!("s{n}"),
                symbol: snapshot.symbol.clone(),
                timeframe: snapshot.timeframe.clone(),
                price: snapshot.price,
                drawings: snapshot.drawings.clone(),
                structure: snapshot.structure.clone(),
                note: snapshot.note.clone(),
                tags: snapshot.tags.clone(),
                created_by: snapshot.created_by.clone(),
                created_at: 1_700_000_000_000_000_000 + i64::try_from(n).unwrap() * 3_600_000_000_000,
            };
            self.rows.lock().expect("lock").push(row.clone());
            Ok(row)
        }

        async fn get(
            &self,
            _user_id: &str,
            id: &str,
        ) -> Result<Option<crate::snapshots::ChartSnapshot>, AgentError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|r| r.id == id)
                .cloned())
        }

        async fn list(
            &self,
            _user_id: &str,
            symbol: &str,
            limit: usize,
        ) -> Result<Vec<crate::snapshots::ChartSnapshot>, AgentError> {
            let rows = self.rows.lock().expect("lock");
            let mut matched: Vec<_> = rows.iter().filter(|r| r.symbol == symbol).cloned().collect();
            matched.sort_by_key(|r| std::cmp::Reverse(r.created_at));
            matched.truncate(limit);
            Ok(matched)
        }
    }

    #[tokio::test]
    async fn take_snapshot_freezes_price_structure_and_the_users_drawings() {
        let fixture = Fixture::rising(30);
        let drawings = DrawingsFixture(vec![hline(45_000.0)]);
        let store = SnapshotFixture::new();
        let ctx = ToolContext::new(&fixture)
            .with_drawings(&drawings, "user-1")
            .with_snapshots(&store, "user-1");

        let out = registry()
            .execute(
                &call(
                    "take_snapshot",
                    json!({
                        "symbol": "BTCUSDT",
                        "timeframe": "5m",
                        "note": "the range as mapped",
                        "tags": ["range"],
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["captured"], true);
        assert_eq!(out["drawings"], 1, "the user's marks ride the capture");
        assert_eq!(out["note"], "the range as mapped");
        let stored = store.rows.lock().expect("lock");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].created_by, "ai", "the agent's capture is marked as its own");
        assert_eq!(stored[0].price, 129.5, "the last close of the fixture");
        assert_eq!(stored[0].drawings.as_array().expect("drawings").len(), 1);
        assert!(stored[0].structure.get("trend").is_some());
    }

    #[tokio::test]
    async fn take_snapshot_without_a_store_says_so_rather_than_pretending() {
        let fixture = Fixture::rising(10);
        let ctx = ToolContext::new(&fixture);
        let err = registry()
            .execute(
                &call("take_snapshot", json!({"symbol": "BTCUSDT", "timeframe": "5m"})),
                &ctx,
            )
            .await
            .expect_err("no store must refuse, honestly");
        match err {
            AgentError::InvalidToolArgs { reason, .. } => {
                assert!(reason.contains("no snapshot store"), "{reason}");
            }
            other => panic!("expected InvalidToolArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_snapshot_reads_back_what_was_captured_and_misses_honestly() {
        let fixture = Fixture::rising(10);
        let store = SnapshotFixture::new();
        store.seed("s-old", 100.0, "Ranging", &[], 1_700_000_000_000_000_000);
        let ctx = ToolContext::new(&fixture).with_snapshots(&store, "user-1");

        let found = registry()
            .execute(&call("get_snapshot", json!({"id": "s-old"})), &ctx)
            .await
            .unwrap();
        assert_eq!(found["id"], "s-old");
        assert_eq!(found["price"], 100.0);

        let missed = registry()
            .execute(&call("get_snapshot", json!({"id": "nope"})), &ctx)
            .await
            .unwrap();
        assert_eq!(missed["found"], false, "a miss is a fact, not an error");
    }

    #[tokio::test]
    async fn compare_snapshots_reports_price_trend_and_drawing_changes() {
        let fixture = Fixture::rising(10);
        let store = SnapshotFixture::new();
        store.seed("s1", 100.0, "Ranging", &["a", "b"], 1_700_000_000_000_000_000);
        store.seed("s2", 102.0, "Up", &["b", "c"], 1_700_000_003_600_000_000);
        let ctx = ToolContext::new(&fixture).with_snapshots(&store, "user-1");

        let out = registry()
            .execute(
                &call("compare_snapshots", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out["compared"], true);
        assert_eq!(out["from"]["id"], "s1");
        assert_eq!(out["to"]["id"], "s2");
        assert!((out["price_change_pct"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(out["trend"]["from"], "Ranging");
        assert_eq!(out["trend"]["to"], "Up");
        assert_eq!(out["trend"]["changed"], true);
        // Drawing "b" is in both; "a" left and "c" arrived.
        let added = out["drawings"]["added"].as_array().expect("added");
        let removed = out["drawings"]["removed"].as_array().expect("removed");
        assert_eq!(added.len(), 1);
        assert_eq!(removed.len(), 1);
        assert!(out["summary"].as_str().unwrap().contains("+2.00%"));
    }

    #[tokio::test]
    async fn compare_snapshots_by_id_and_the_missing_pair_case() {
        let fixture = Fixture::rising(10);
        let store = SnapshotFixture::new();
        store.seed("only", 100.0, "Up", &[], 1_700_000_000_000_000_000);
        let ctx = ToolContext::new(&fixture).with_snapshots(&store, "user-1");

        // One snapshot is not a comparison.
        let lonely = registry()
            .execute(
                &call("compare_snapshots", json!({"symbol": "BTCUSDT"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(lonely["compared"], false);
        assert!(lonely["note"].as_str().unwrap().contains("needs two"));

        // By id with one missing: also a neutral answer, not an error.
        let missing = registry()
            .execute(
                &call("compare_snapshots", json!({"id_a": "only", "id_b": "nope"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(missing["compared"], false);

        // No ids and no symbol: the model hears what the tool takes.
        let err = registry()
            .execute(&call("compare_snapshots", json!({})), &ctx)
            .await
            .expect_err("no selector must be refused");
        assert!(matches!(err, AgentError::InvalidToolArgs { .. }));
    }
}
