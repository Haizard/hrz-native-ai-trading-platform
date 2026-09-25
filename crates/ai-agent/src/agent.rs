//! Orchestration: the loop that turns a question into a thesis, and a
//! description into a validated strategy document.
//!
//! ## Why the ladder is read here rather than by the model
//!
//! `docs/09` step 2 says "call `analyze_timeframe` for each [timeframe],
//! top-down". Read literally, that is a tool the model chooses to call. Left
//! that way, a lazy or distracted model can produce a complete-looking thesis
//! without ever reading a single timeframe -- and the exit criterion is that
//! every number traces to a tool call.
//!
//! So the orchestrator reads the ladder itself, before the first LLM call:
//!
//! * the digest goes into the system prompt, so the context is guaranteed;
//! * every ladder result seeds the [`PriceRange`], so grounding is guaranteed;
//! * the common case costs one LLM turn instead of five.
//!
//! The model keeps all fifteen analysis tools and can still ask for anything
//! deeper (footprint, VWAP, a base rate). What it can no longer do is skip the
//! reading and still hand back a thesis.
//!
//! ## Why tool failures go back to the model instead of aborting
//!
//! A tool error is information the model can act on: "no tick data in this
//! window" is the difference between *absorption did not occur* and
//! *absorption cannot be detected here* (see [`CheckStatus::Unknown`]). Raising
//! would end the turn and leave the user with nothing. So failures come back as
//! `is_error` results and the loop continues.

use std::sync::Arc;

use analytics_core::MarketStateConfig;
use serde_json::json;
use strategy_dsl::expr::ConceptPart;

use crate::error::AgentError;
use crate::llm_client::{
    ContentBlock, LlmClient, LlmRequest, Message, ToolCall, ToolChoice, ToolResult, ToolSpec, Usage,
};
use crate::multi_timeframe::{analyze_ladder, LadderView, TimeframeLadder};
use crate::progress::{NoProgress, Progress, ProgressSink};
use crate::skills::{Skill, SkillLibrary, SkillQuery};
use crate::thesis::{
    parse_thesis, submit_thesis_spec, PriceRange, ToolTrace, TradeThesis, SUBMIT_THESIS,
};
use crate::tools::{ToolContext, ToolRegistry};

/// Default cap on round trips the model may spend calling analysis tools.
pub const DEFAULT_MAX_TURNS: usize = 6;

/// Turns reserved for the answer once the analysis budget is spent.
///
/// These are the turns that matter: [`DEFAULT_MAX_TURNS`] is headroom, this is
/// the difference between a thesis and an error. Two, because the first is
/// sometimes spent narrating or re-calling a tool from habit, and one nudge
/// with an explicit refusal is usually enough to land it.
pub const ANSWER_TURNS: usize = 2;

/// Default cap on validation-retry attempts when generating a strategy.
pub const DEFAULT_MAX_ATTEMPTS: usize = 3;

/// The asking user's drawings, attached by the host after authenticating them.
///
/// A trait object on a request is unusual, and the reason is ownership: the
/// *gateway* knows which user is asking, and the user is exactly the fact that
/// scopes [`crate::user_drawings::UserDrawingsSource`]. The wire types
/// (`AskBody`, `AgentWsRequest`) never carry this -- it is constructed
/// server-side from the authenticated identity, so a client cannot name
/// another user's drawings any more than it could name their session.
pub struct DrawingsContext {
    source: Arc<dyn crate::user_drawings::UserDrawingsSource>,
    /// Where the agent's own objects go, when the host allows writes.
    ///
    /// Inside the same context as the reader, not a parallel one, because the
    /// two are one capability granted to one identity: a host that wires the
    /// reader without the writer gets a read-only agent, and there is no path
    /// by which the write identity could diverge from the read identity.
    writer: Option<Arc<dyn crate::user_drawings::DrawingWriter>>,
    user_id: String,
}

impl DrawingsContext {
    /// Bind a drawings source to one authenticated user.
    #[must_use]
    pub fn new(
        source: Arc<dyn crate::user_drawings::UserDrawingsSource>,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            source,
            writer: None,
            user_id: user_id.into(),
        }
    }

    /// Also allow the agent to write chart objects, as the same user.
    ///
    /// The write identity **is** the read identity by construction: this
    /// method takes no id of its own, so the two cannot be pointed at
    /// different users even by a confused host.
    #[must_use]
    pub fn with_writer(mut self, writer: Arc<dyn crate::user_drawings::DrawingWriter>) -> Self {
        self.writer = Some(writer);
        self
    }

    /// The writer, for the tools, when the host granted writes.
    #[must_use]
    pub fn writer(&self) -> Option<&Arc<dyn crate::user_drawings::DrawingWriter>> {
        self.writer.as_ref()
    }

    /// The user whose drawings this is, opaque to the agent.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

impl Clone for DrawingsContext {
    fn clone(&self) -> Self {
        Self {
            source: Arc::clone(&self.source),
            writer: self.writer.clone(),
            user_id: self.user_id.clone(),
        }
    }
}

/// The asking user's memory, attached by the host after authenticating them.
///
/// The same ownership argument as [`DrawingsContext`], and the same shape:
/// reader and writer ride together because a memory the agent cannot recall
/// and one it cannot store are the same capability seen from two sides, and
/// the user id never crosses the wire — it is resolved from the authenticated
/// identity one layer up.
pub struct MemoryContext {
    source: Arc<dyn crate::agent_memory::MemorySource>,
    writer: Option<Arc<dyn crate::agent_memory::MemoryWriter>>,
    user_id: String,
}

impl MemoryContext {
    /// Bind a memory source to one authenticated user.
    #[must_use]
    pub fn new(
        source: Arc<dyn crate::agent_memory::MemorySource>,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            source,
            writer: None,
            user_id: user_id.into(),
        }
    }

    /// Also allow the agent to store facts, as the same user.
    #[must_use]
    pub fn with_writer(mut self, writer: Arc<dyn crate::agent_memory::MemoryWriter>) -> Self {
        self.writer = Some(writer);
        self
    }

    /// The user whose memory this is, opaque to the agent.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

impl Clone for MemoryContext {
    fn clone(&self) -> Self {
        Self {
            source: Arc::clone(&self.source),
            writer: self.writer.clone(),
            user_id: self.user_id.clone(),
        }
    }
}

impl std::fmt::Debug for MemoryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same rule as `DrawingsContext`: log the identity, never the source.
        f.debug_struct("MemoryContext")
            .field("user_id", &self.user_id)
            .finish()
    }
}

impl std::fmt::Debug for DrawingsContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The source is a callback into storage; printing it would print an
        // address. The user id is the fact worth logging -- and no more than
        // that, because request logs outlive the request.
        f.debug_struct("DrawingsContext")
            .field("user_id", &self.user_id)
            .finish()
    }
}

/// A question for the agent.
#[derive(Debug, Clone)]
pub struct AskRequest {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// The user's question, in plain language.
    pub question: String,
    /// Explicit timeframe ladder, overriding the skill and the default.
    pub timeframes: Option<Vec<String>>,
    /// Pin a specific skill instead of retrieving by relevance.
    pub skill_id: Option<String>,
    /// Candles per timeframe; `None` uses the context default.
    pub lookback: Option<usize>,
    /// The chart the user is looking at, when the shell told us.
    ///
    /// See [`crate::chart_context`] for why this is a *viewport* and not data:
    /// it says where to look, and every number in it is subject to the same
    /// grounding rule as a tool result.
    pub chart: Option<crate::chart_context::ChartContext>,
    /// The asking user's drawings, when the host attached them.
    ///
    /// `None` makes `get_user_drawings` report "no drawings source" rather
    /// than a bare chart -- see the tool for why those must not be confused.
    pub drawings: Option<DrawingsContext>,
    /// The asking user's memory, when the host attached it.
    ///
    /// `None` is a deployment without memory, which the prompt simply omits —
    /// the model is never told "you have no memory", because that reads as
    /// an apology rather than a configuration fact.
    pub memory: Option<MemoryContext>,
}

impl AskRequest {
    /// A question with everything else left to defaults.
    #[must_use]
    pub fn new(symbol: impl Into<String>, question: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            question: question.into(),
            timeframes: None,
            skill_id: None,
            lookback: None,
            chart: None,
            drawings: None,
            memory: None,
        }
    }

    /// Pin a skill by id.
    #[must_use]
    pub fn with_skill(mut self, skill_id: impl Into<String>) -> Self {
        self.skill_id = Some(skill_id.into());
        self
    }

    /// Override the ladder.
    #[must_use]
    pub fn with_timeframes(mut self, timeframes: Vec<String>) -> Self {
        self.timeframes = Some(timeframes);
        self
    }

    /// Attach the viewport the user is looking at.
    #[must_use]
    pub fn with_chart(mut self, chart: crate::chart_context::ChartContext) -> Self {
        self.chart = Some(chart);
        self
    }

    /// Attach the asking user's drawings, resolved by the host.
    ///
    /// Host-only, by construction: [`DrawingsContext`] is built from the
    /// authenticated identity one layer up and never parsed from a request
    /// body.
    #[must_use]
    pub fn with_drawings(mut self, drawings: DrawingsContext) -> Self {
        self.drawings = Some(drawings);
        self
    }

    /// Attach the asking user's memory, resolved by the host.
    ///
    /// Host-only for the same reason the drawings are: the id is a fact about
    /// *who is asking*, which no request body may state.
    #[must_use]
    pub fn with_memory(mut self, memory: MemoryContext) -> Self {
        self.memory = Some(memory);
        self
    }
}

/// What the agent hands back for a question.
#[derive(Debug, Clone)]
pub struct AgentAnswer {
    /// The explainable thesis. Every numeric field traces to [`Self::trace`].
    pub thesis: TradeThesis,
    /// The skill that supplied the methodology, if one was retrieved.
    pub skill: Option<String>,
    /// The ladder as read, coarse to fine.
    pub ladder: LadderView,
    /// Every tool call made during the turn, with its raw result.
    pub trace: Vec<ToolTrace>,
    /// LLM round trips used.
    pub turns: usize,
    /// Token usage across the whole turn.
    pub usage: Usage,
}

/// A natural-language request for a strategy document.
#[derive(Debug, Clone)]
pub struct StrategyRequest {
    /// What the user wants the strategy to do.
    pub description: String,
    /// Instrument the strategy trades.
    pub market: String,
    /// Timeframe the entry condition fires on.
    pub entry_timeframe: String,
    /// Pin a skill whose methodology the document should encode.
    pub skill_id: Option<String>,
    /// Validation-retry cap; `None` uses [`DEFAULT_MAX_ATTEMPTS`].
    pub max_attempts: Option<usize>,
}

impl StrategyRequest {
    /// A description, market and entry timeframe; everything else default.
    #[must_use]
    pub fn new(
        description: impl Into<String>,
        market: impl Into<String>,
        entry_timeframe: impl Into<String>,
    ) -> Self {
        Self {
            description: description.into(),
            market: market.into(),
            entry_timeframe: entry_timeframe.into(),
            skill_id: None,
            max_attempts: None,
        }
    }
}

/// A strategy document that has passed validation, plus how it got there.
#[derive(Debug, Clone)]
pub struct GeneratedStrategy {
    /// The document, wrapped in the validating type so it cannot be handed to
    /// the runtime without having passed (`docs/09`: never a silently-invalid
    /// document).
    validated: strategy_dsl::ValidatedStrategy,
    /// The YAML as the model produced it, for storage and diffing.
    pub yaml: String,
    /// How many draft/validate round trips it took.
    pub attempts: usize,
    /// Every validation error that was fed back and fixed. Empty means the
    /// first draft was already valid.
    pub repaired_errors: Vec<String>,
}

impl GeneratedStrategy {
    /// The validated document.
    #[must_use]
    pub fn document(&self) -> &strategy_dsl::StrategyDocument {
        self.validated.document()
    }

    /// Consume into the validated wrapper, for handing to `strategy-runtime`.
    #[must_use]
    pub fn into_validated(self) -> strategy_dsl::ValidatedStrategy {
        self.validated
    }
}

/// Tunables for the orchestrator.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Cap on round trips the model may spend calling analysis tools.
    ///
    /// One further turn is always allowed after this, reserved for the
    /// answer: see [`Agent::ask`].
    pub max_turns: usize,
    /// Cap on validation retries per strategy.
    pub max_attempts: usize,
    /// Output token cap per request.
    pub max_tokens: u32,
    /// Sampling temperature. Zero by default: this is analysis, not prose.
    pub temperature: f32,
    /// Candles per timeframe when reading the ladder.
    pub lookback: usize,
    /// Passed through to `analytics-core` when building each `MarketState`.
    pub market_state: MarketStateConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_tokens: 4096,
            temperature: 0.0,
            lookback: 300,
            market_state: MarketStateConfig::default(),
        }
    }
}

/// The agent: LLM + tools + skills, in one loop.
pub struct Agent {
    llm: Arc<dyn LlmClient>,
    registry: ToolRegistry,
    skills: SkillLibrary,
    config: AgentConfig,
}

impl Agent {
    /// Assemble an agent from its parts.
    #[must_use]
    pub fn new(llm: Arc<dyn LlmClient>, skills: SkillLibrary, config: AgentConfig) -> Self {
        Self {
            llm,
            registry: ToolRegistry::market_analysis(),
            skills,
            config,
        }
    }

    /// Answer a question with a thesis.
    ///
    /// # Errors
    /// [`AgentError::NoData`] when the ladder has no candles at all;
    /// [`AgentError::NoThesis`] when the turn budget runs out without a
    /// `submit_thesis` call; [`AgentError::Ungrounded`] when the thesis cites
    /// numbers no tool reported.
    pub async fn ask(
        &self,
        request: &AskRequest,
        data: &dyn crate::tools::MarketDataSource,
    ) -> Result<AgentAnswer, AgentError> {
        self.ask_with_progress(request, data, &NoProgress).await
    }

    /// [`Self::ask`], reporting each step to `progress` as it happens.
    ///
    /// The steps are not decoration. This call takes about a minute, most of it
    /// waiting on the model, and a socket that says nothing for a minute is
    /// indistinguishable from one that has died. See [`crate::progress`] for
    /// why the reporting is progress rather than streamed tokens.
    ///
    /// # Errors
    /// As [`Self::ask`].
    pub async fn ask_with_progress(
        &self,
        request: &AskRequest,
        data: &dyn crate::tools::MarketDataSource,
        progress: &dyn ProgressSink,
    ) -> Result<AgentAnswer, AgentError> {
        let skill = self.select_skill(request)?;

        // The user's own resolution anchors the ladder. A question asked while
        // looking at the 5m chart is about the 5m chart, and answering it from
        // the default 1D/4H/1H/5M ladder reads the right market at the wrong
        // place. The anchor only *promotes* a timeframe into a ladder that
        // already exists -- it never replaces the higher-timeframe context,
        // which is the part that makes the answer more than a zoomed-in guess.
        let chart_anchor = request
            .chart
            .as_ref()
            .and_then(crate::chart_context::ChartContext::ladder_anchor);

        let ladder = match &request.timeframes {
            Some(frame_strings) => TimeframeLadder::from_strs(frame_strings),
            None => match &skill {
                Some(s) => TimeframeLadder::from_skill(s),
                None => TimeframeLadder::default(),
            },
        };
        let ladder = match chart_anchor {
            Some(anchor) if !ladder.timeframes().contains(&anchor) => {
                let mut timeframes = ladder.timeframes().to_vec();
                timeframes.push(anchor);
                TimeframeLadder::new(timeframes)
            }
            _ => ladder,
        };
        if ladder.is_empty() {
            return Err(AgentError::DataUnavailable {
                symbol: request.symbol.clone(),
                timeframe: "ladder".into(),
                reason: "no usable timeframes in the request, skill, or default ladder".into(),
            });
        }

        let lookback = request.lookback.unwrap_or(self.config.lookback);
        progress.report(Progress::ReadingMarket {
            symbol: request.symbol.clone(),
            timeframes: ladder.len(),
        });

        // Memory is read **before** the prompt is built, in parallel with the
        // ladder reading: the recall section must be in the system prompt from
        // turn one, because a memory the model has to ask for is one it will
        // not use. A storage failure degrades to "no memories" rather than
        // failing the question — amnesia must not take the analysis down.
        let memories = match &request.memory {
            Some(memory) => match memory
                .source
                .recall(
                    memory.user_id(),
                    &request.symbol,
                    crate::agent_memory::RECALL_LIMIT,
                )
                .await
            {
                Ok(rows) => Some(rows),
                Err(err) => {
                    tracing::warn!(target: "ai_agent", %err, "memory recall failed; continuing without it");
                    None
                }
            },
            None => None,
        };
        let view = analyze_ladder(
            data,
            &request.symbol,
            &ladder,
            &self.config.market_state,
            lookback,
        )
        .await?;

        // Grounding starts from the ladder: every price the model is about to
        // be shown is a price it is allowed to cite.
        let mut range = PriceRange::new();
        let mut trace = Vec::new();
        for frame in &view.frames {
            let args = json!({"symbol": request.symbol, "timeframe": frame.timeframe.to_string()});
            let result = serde_json::to_value(&frame.state)?;
            range.observe_result(&result);
            trace.push(ToolTrace {
                tool: "analyze_timeframe".into(),
                args,
                result,
            });
        }

        let system = ask_system_prompt(
            &request.symbol,
            skill.as_ref(),
            &view,
            request.chart.as_ref(),
            memories.as_deref(),
        );
        // Screenshots ride on the first user message, primary view first. It is
        // attached here rather than as a separate message because Bedrock
        // requires a `user` turn to alternate with `assistant`, and an extra
        // image-only message would consume a turn of the loop's budget for no
        // reasoning. Multi-chart shells label each capture so the prompt's
        // image list (from `ChartContext::render`) pairs each caption to the
        // block at the same position.
        let shots: Vec<&crate::chart_context::ChartScreenshot> = request
            .chart
            .as_ref()
            .map(crate::chart_context::ChartContext::all_screenshots)
            .unwrap_or_default();
        let mut first_content: Vec<ContentBlock> = Vec::with_capacity(shots.len() + 1);
        for shot in &shots {
            first_content.push(ContentBlock::Image {
                media_type: shot.media_type.clone(),
                data: shot.data.clone(),
            });
        }
        first_content.push(ContentBlock::Text(request.question.clone()));
        let mut messages = vec![Message {
            role: crate::llm_client::Role::User,
            content: first_content,
        }];
        let mut tools = self.registry.specs();
        tools.push(submit_thesis_spec());

        // No backtest runner here: `backtest_*` tools stay registered and
        // return a clear "not attached" error, which the model can report
        // honestly rather than inventing a base rate. The drawings source is
        // the same shape: absent unless the host attached one, and the tool
        // says so rather than implying the chart is bare.
        let mut ctx = ToolContext::new(data).with_config(self.config.market_state);
        // The memory tools get the same grant the loop already holds, so the
        // model's explicit `remember` cannot write where the auto-store could
        // not: one identity, one door.
        if let Some(memory) = &request.memory {
            ctx = ctx.with_memory(
                memory.source.as_ref(),
                memory.writer.as_deref(),
                memory.user_id(),
            );
        }
        if let Some(drawings) = &request.drawings {
            ctx = ctx.with_drawings(drawings.source.as_ref(), drawings.user_id());
            // Writes ride the same grant: no writer attached means the write
            // tools report their absence, exactly like the read tool.
            if let Some(writer) = drawings.writer() {
                ctx = ctx.with_drawing_writer(writer.as_ref(), drawings.user_id());
            }
        }

        let mut usage = Usage {
            input_tokens: None,
            output_tokens: None,
        };
        let mut turns = 0_usize;

        // `max_turns` turns of analysis, then `ANSWER_TURNS` that can only
        // answer. Reserving them matters: a question that ends on another data
        // call produces no thesis at all, which is the worst possible failure
        // after the user has already waited a minute.
        let total = self.config.max_turns + ANSWER_TURNS;
        for turn in 0..total {
            turns = turn + 1;
            let answering = turn >= self.config.max_turns;
            let last = turn + 1 == total;
            // In the answer phase `submit_thesis` is the only tool announced
            // *and* the only one dispatched -- see the refusal below. Announcing
            // it alone was tried first and was not enough: a live run called
            // `detect_market_structure` anyway, and dispatching it let the
            // model spend the reserved turn on data it already had.
            let (turn_tools, tool_choice) = if answering {
                (
                    vec![submit_thesis_spec()],
                    Some(ToolChoice::Tool(SUBMIT_THESIS.into())),
                )
            } else {
                (tools.clone(), None)
            };

            progress.report(Progress::Thinking {
                turn: turn + 1,
                total,
                answering,
            });
            let response = self
                .llm
                .complete(LlmRequest {
                    system: Some(system.clone()),
                    messages: messages.clone(),
                    tools: turn_tools,
                    tool_choice,
                    max_tokens: self.config.max_tokens,
                    temperature: self.config.temperature,
                })
                .await?;
            usage = usage + response.usage;

            let calls = response.message.tool_calls();
            messages.push(response.message.clone());

            if calls.is_empty() || answering {
                // An empty or unsubmitted answer turn is the one failure a
                // user actually sees, and it says nothing about why. Log what
                // came back -- stop reason and text -- so it can be diagnosed
                // without re-running against the live provider.
                tracing::warn!(
                    target: "ai_agent",
                    turn,
                    last,
                    stop_reason = ?response.stop_reason,
                    calls = calls.len(),
                    text = %response.message.text().chars().take(200).collect::<String>(),
                    "no submit_thesis on this turn"
                );
            }

            if calls.is_empty() {
                // Prose only. Some models narrate before calling, so nudge
                // unless there is no turn left to nudge into.
                if last {
                    break;
                }
                messages.push(Message::user(
                    "Answer by calling submit_thesis with the structured fields. \
                     Do not answer in prose.",
                ));
                continue;
            }

            let mut results = Vec::new();
            let mut submitted = None;
            for call in &calls {
                if call.name == SUBMIT_THESIS {
                    submitted = Some(call);
                    break;
                }
                if answering {
                    // Refused rather than run. Executing it would be the whole
                    // bug: the model asks for data out of habit, gets it, and
                    // the turn reserved for the answer is gone.
                    results.push(ToolResult {
                        tool_use_id: call.id.clone(),
                        content: json!({
                            "accepted": false,
                            "error": format!(
                                "`{}` is not available now: the analysis phase is over.",
                                call.name
                            ),
                            "instruction": "Call submit_thesis with the thesis you have.",
                        }),
                        is_error: true,
                    });
                    continue;
                }
                progress.report(Progress::Tool {
                    name: call.name.clone(),
                });
                let result = self.run_tool(call, &ctx, &mut range, &mut trace).await;
                progress.report(Progress::ToolDone {
                    name: call.name.clone(),
                    ok: !result.is_error,
                });
                results.push(result);
            }

            if let Some(call) = submitted {
                // Logged before parsing: if the thesis is rejected for bad
                // numbers, this is the only record of what was actually sent.
                tracing::debug!(target: "ai_agent", args = %call.input, "submit_thesis");
                let mut thesis = parse_thesis(&call.input)?;
                thesis.skill_used = skill.as_ref().map(|s| s.id());
                thesis.provenance = trace.clone();

                // A rejected thesis is a fixable one when the model still has
                // a turn to fix it in: hand the exact reason back and let it
                // correct the levels. Failing outright would throw away a
                // question the user already waited a minute for, over a stop
                // that is one number wrong.
                if let Err(err @ AgentError::Ungrounded(_)) = thesis.finalize(&range) {
                    if !last {
                        tracing::warn!(target: "ai_agent", %err, "thesis rejected, asking for a correction");
                        progress.report(Progress::Correcting {
                            reason: err.to_string(),
                        });
                        messages.push(Message::tool_results(vec![ToolResult {
                            tool_use_id: call.id.clone(),
                            content: json!({
                                "accepted": false,
                                "error": err.to_string(),
                                "instruction": "Fix the levels and call submit_thesis again. \
                                                Every level must be one the tools reported.",
                            }),
                            is_error: true,
                        }]));
                        continue;
                    }
                    return Err(err);
                }

                if thesis.narrative.trim().is_empty() {
                    thesis.narrative = thesis.fallback_narrative();
                }

                // Auto-store, after the thesis is final: the levels it just
                // stood behind are the facts most worth carrying into the
                // next conversation. Best-effort and logged — a failed write
                // must not undo a delivered answer, but a silent one could
                // never be diagnosed.
                if let Some(memory) = &request.memory {
                    if let Some(writer) = memory.writer.as_ref() {
                        for fact in crate::agent_memory::facts_from_thesis(&thesis) {
                            if let Err(err) = writer.remember(memory.user_id(), &fact).await {
                                tracing::warn!(
                                    target: "ai_agent",
                                    %err,
                                    key = %fact.key,
                                    "auto-store of a thesis fact failed"
                                );
                            }
                        }
                    }
                }

                return Ok(AgentAnswer {
                    thesis,
                    skill: skill.as_ref().map(|s| s.id()),
                    ladder: view,
                    trace,
                    turns,
                    usage,
                });
            }

            if results.is_empty() {
                continue;
            }
            messages.push(Message::tool_results(results));
        }

        Err(AgentError::NoThesis { turns })
    }

    /// Turn a plain-language description into a validated strategy document.
    ///
    /// The document is drafted by the model, validated by `strategy-dsl`, and
    /// on failure the specific field-level error is handed back for another
    /// attempt. Nothing is returned that has not passed validation.
    ///
    /// # Errors
    /// [`AgentError::InvalidStrategyDocument`] when the retry budget runs out —
    /// carrying the last validation error, which is specific enough to show the
    /// user.
    pub async fn generate_strategy(
        &self,
        request: &StrategyRequest,
    ) -> Result<GeneratedStrategy, AgentError> {
        let skill = match &request.skill_id {
            Some(id) => Some(
                self.skills
                    .by_id(id)
                    .ok_or_else(|| AgentError::NoMatchingSkill(id.clone()))?
                    .clone(),
            ),
            None => None,
        };

        let system =
            strategy_system_prompt(&request.market, &request.entry_timeframe, skill.as_ref());
        let max_attempts = request.max_attempts.unwrap_or(self.config.max_attempts);

        let mut messages = vec![Message::user(&request.description)];
        let tools = vec![draft_strategy_spec()];
        let mut repaired_errors = Vec::new();

        for attempt in 1..=max_attempts {
            let response = self
                .llm
                .complete(LlmRequest {
                    system: Some(system.clone()),
                    messages: messages.clone(),
                    tools: tools.clone(),
                    tool_choice: Some(ToolChoice::Tool(DRAFT_STRATEGY.into())),
                    max_tokens: self.config.max_tokens,
                    temperature: self.config.temperature,
                })
                .await?;

            let calls = response.message.tool_calls();
            let Some(call) = calls.iter().find(|c| c.name == DRAFT_STRATEGY) else {
                messages.push(response.message.clone());
                messages.push(Message::user(
                    "Respond by calling draft_strategy with the strategy YAML.",
                ));
                continue;
            };

            let yaml = call.input["yaml"]
                .as_str()
                .ok_or_else(|| AgentError::InvalidToolArgs {
                    tool: DRAFT_STRATEGY.into(),
                    reason: "`yaml` must be a string".into(),
                })?
                .to_string();

            // The validator checks the document; it cannot check that the
            // document is the one that was asked for. A live draft came back
            // with `entry: 15m` against a request for 5m -- valid, and
            // useless, because nothing downstream would run it on the ladder
            // the user has data for.
            let rejection = match strategy_dsl::parse_and_validate(&yaml) {
                Ok(validated) => {
                    let entry_tf = validated.document().timeframes.get("entry");
                    let asked = request.entry_timeframe.trim();
                    match entry_tf {
                        Some(tf) if tf.to_string() == asked => {
                            return Ok(GeneratedStrategy {
                                validated,
                                yaml,
                                attempts: attempt,
                                repaired_errors,
                            });
                        }
                        other => Some(format!(
                            "timeframes.entry must be `{asked}` -- it was `{}`",
                            other.map_or_else(|| "missing".into(), |tf| tf.to_string())
                        )),
                    }
                }
                Err(err) => Some(err.to_string()),
            };

            if let Some(error) = rejection {
                // The validator already reports every issue with its field
                // path; handing that back verbatim is what makes the retry
                // a correction instead of a re-roll.
                repaired_errors.push(error.clone());
                messages.push(response.message.clone());
                messages.push(Message::tool_results(vec![ToolResult {
                    tool_use_id: call.id.clone(),
                    content: json!({
                        "valid": false,
                        "error": error,
                        "instruction": "Fix every issue listed and call draft_strategy again.",
                    }),
                    is_error: true,
                }]));
            }
        }

        Err(AgentError::InvalidStrategyDocument(
            strategy_dsl::DslError::Validation {
                issues: vec![strategy_dsl::ValidationIssue::new(
                    "document",
                    repaired_errors
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "no draft was produced".into()),
                )],
            },
        ))
    }

    /// Pick the skill for a request: pinned by id, else by relevance.
    fn select_skill(&self, request: &AskRequest) -> Result<Option<Skill>, AgentError> {
        if let Some(id) = &request.skill_id {
            return Ok(Some(
                self.skills
                    .by_id(id)
                    .ok_or_else(|| AgentError::NoMatchingSkill(id.clone()))?
                    .clone(),
            ));
        }
        let query = SkillQuery::for_market(&request.symbol)
            .with_terms(request.question.split_whitespace().map(str::to_string));
        Ok(self.skills.retrieve(&query).first().copied().cloned())
    }

    /// Run one non-terminal tool call, recording it for provenance and folding
    /// any prices it reported into the grounding range.
    async fn run_tool(
        &self,
        call: &ToolCall,
        ctx: &ToolContext<'_>,
        range: &mut PriceRange,
        trace: &mut Vec<ToolTrace>,
    ) -> ToolResult {
        match self.registry.execute(call, ctx).await {
            Ok(result) => {
                range.observe_result(&result);
                trace.push(ToolTrace {
                    tool: call.name.clone(),
                    args: call.input.clone(),
                    result: result.clone(),
                });
                ToolResult {
                    tool_use_id: call.id.clone(),
                    content: result,
                    is_error: false,
                }
            }
            Err(err) => {
                // Deliberately not pushed into `trace`: a failed call produced
                // no numbers, and provenance must not look like it did.
                tracing::debug!(target: "ai_agent", tool = %call.name, error = %err, "tool error returned to model");
                ToolResult {
                    tool_use_id: call.id.clone(),
                    content: json!({"error": err.to_string()}),
                    is_error: true,
                }
            }
        }
    }
}

/// Allow `usage + usage`.
impl std::ops::Add for Usage {
    type Output = Usage;
    fn add(self, other: Usage) -> Usage {
        Usage {
            input_tokens: add_tokens(self.input_tokens, other.input_tokens),
            output_tokens: add_tokens(self.output_tokens, other.output_tokens),
        }
    }
}

fn add_tokens(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// The system prompt for a thesis question.
///
/// The rules are stated as constraints rather than encouragement because this
/// is the only place the boundary in `docs/09` is enforced at runtime.
fn ask_system_prompt(
    symbol: &str,
    skill: Option<&Skill>,
    ladder: &LadderView,
    chart: Option<&crate::chart_context::ChartContext>,
    memories: Option<&[crate::agent_memory::MemoryRow]>,
) -> String {
    let mut out = String::new();
    out.push_str("You are the market analyst for an order-flow trading terminal.\n\n");

    out.push_str("## Hard rules\n");
    out.push_str(
        "1. You never calculate a number yourself. Every number in your answer \
         must come from a tool result or from the timeframe summary below. \
         If you need a value that no tool returned, call a tool for it.\n",
    );
    out.push_str(
        "2. You never infer a level from the shape of a chart. You read it from \
         the data the tools returned.\n",
    );
    out.push_str(
        "3. You answer by calling `submit_thesis`. Its fields are the product. \
         The narrative explains them; it does not replace them.\n",
    );
    out.push_str(
        "4. A condition you could not evaluate is `status: unknown`, never \
         `fail`. `fail` means you checked and it did not hold. If a tool \
         reports `available: false`, or the data simply is not there, the \
         honest answer is unknown -- and unknown is a legitimate answer, not a \
         weak one.\n",
    );
    out.push_str("5. Every check cites the tool that produced it in its `source` field.\n\n");

    if let Some(skill) = skill {
        out.push_str("## Skill\n");
        out.push_str(&skill.render());
        out.push_str(
            "\nFollow this methodology. Do not invent rules that are not in it; \
             if the setup does not satisfy it, say which conditions failed.\n\n",
        );
    }

    out.push_str("## Timeframes already read\n");
    out.push_str("These were computed in Rust from real candles. They are facts.\n\n");
    out.push_str(&ladder.digest());
    out.push('\n');

    // After the ladder: remembered facts are context the model reads before
    // the question, but the market read is the primary fact source and must
    // come first so memory is interpreted *against* it, not instead of it.
    if let Some(memories) = memories {
        out.push_str(&crate::agent_memory::render_recall(memories));
        out.push('\n');
    }

    // Placed after the ladder so the model reads the facts first and the
    // viewport second: the viewport says where to look, and must not be
    // mistaken for another source of numbers.
    if let Some(rendered) = chart.and_then(crate::chart_context::ChartContext::render) {
        out.push('\n');
        out.push_str(&rendered);
    }

    out.push_str(&format!(
        "\nSymbol: {symbol}. Use `submit_thesis` when you have enough, even if \
         some conditions are UNKNOWN -- an honest partial thesis beats a \
         confident empty one.\n"
    ));
    out
}

/// The system prompt for strategy generation.
///
/// The vocabulary is rendered from `strategy-dsl`'s own constants rather than
/// written out here, so the model is shown exactly the functions the validator
/// accepts -- a hand-written list would drift and produce unfixable drafts.
/// The document shape the strategy prompt shows the model, with `{market}` and
/// `{entry_timeframe}` left to substitute.
///
/// It is a template rather than an inline string so a test can format it and
/// run it through the real validator. That matters: a prompt example which
/// does not itself validate is worse than no example, because the model copies
/// it, `strategy-dsl` rejects the copy, and the model then has to guess its
/// way out of errors the prompt caused. `strategy_shape_validates` guards it.
const STRATEGY_SHAPE_TEMPLATE: &str = r#"name: "..."
version: "1.0"
kind: strategy
market: "{market}"
timeframes:
  trend: "4h"
  entry: "{entry_timeframe}"
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: "market_structure.trend == \"bullish\""
    - timeframe: entry
      condition: "close > vwap"
    - timeframe: entry
      condition: "delta > threshold(5)"
risk:
  max_risk_pct: 1.0
  stop: below_sweep_low
  take_profit:
    type: "risk_multiple"
    value: 2.0
invalidation:
  - timeframe: entry
    condition: "close_below(vwap)"
"#;

/// The worked example the prompt shows for a client-defined measurement.
///
/// A complete document rather than a fragment, and a template for the same
/// reason [`STRATEGY_SHAPE_TEMPLATE`] is: a test formats it and runs it through
/// the real validator. An example that does not validate is worse than none,
/// because the model copies it and then has to guess its way out of errors the
/// prompt produced.
///
/// A fair value gap is the example because it is the one a trader is most
/// likely to ask for by name and the one the built-in vocabulary is least able
/// to express: three candles, the first candle's high left behind below the
/// third candle's low. Nothing in `analytics-core` knows what a gap is.
const CONCEPT_EXAMPLE: &str = r#"name: "..."
version: "1.0"
kind: strategy
market: "{market}"
timeframes:
  entry: "{entry_timeframe}"
concepts:
  - name: gap
    label: fvg
    side: buy
    window: 3
    lower: {high: 0}
    upper: {low: 2}
    require:
      - {left: {high: 0}, op: below, right: {low: 2}}
    min_band_ratio: 0.2
entry:
  direction: long
  all_of:
    - timeframe: entry
      condition: "concepts.gap.fresh"
    - timeframe: entry
      condition: "close > concepts.gap.top"
risk:
  max_risk_pct: 1.0
  stop: {kind: below_recent_low, bars: 20}
  take_profit:
    type: "risk_multiple"
    value: 2.0
invalidation:
  - timeframe: entry
    condition: "close_below(vwap)"
"#;

fn strategy_system_prompt(market: &str, entry_timeframe: &str, skill: Option<&Skill>) -> String {
    let fields = strategy_dsl::ALL_FIELDS
        .iter()
        .map(|f| f.name())
        .collect::<Vec<_>>()
        .join(", ");
    let funcs = strategy_dsl::ALL_FUNCS
        .iter()
        .map(|f| f.name())
        .collect::<Vec<_>>()
        .join(", ");
    // The concept language, likewise rendered from the enums that enforce it.
    let selectors = analytics_core::concepts::Selector::NAMES.join(", ");
    let ops = analytics_core::concepts::Compare::ALL
        .iter()
        .map(|op| op.name())
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    out.push_str("You write Strategy DSL documents for an order-flow trading terminal.\n\n");
    out.push_str("## Output\n");
    out.push_str(
        "Call `draft_strategy` with a single `yaml` string. Nothing else. \
         The document is validated by the platform and you will be given the \
         exact errors to fix if it fails.\n\n",
    );

    out.push_str("## Required shape\n");
    out.push_str("```yaml\n");
    out.push_str(
        &STRATEGY_SHAPE_TEMPLATE
            .replace("{market}", market)
            .replace("{entry_timeframe}", entry_timeframe),
    );
    out.push_str("```\n\n");
    out.push_str(
        "Each entry under `all_of`, `any_of` and `invalidation` takes exactly \
         three keys: `timeframe`, `condition`, and an optional `label`. No \
         others -- in particular there is no `value` here; `value` belongs to \
         `risk.take_profit` and putting it in a condition is the single most \
         common validation failure.\n\n",
    );

    // Scoped to *conditions* on purpose. The heading used to read "nothing
    // outside these is accepted", full stop -- which, once concepts existed,
    // read as "you may not define a fair value gap". The list really does
    // bound what a condition may *name*; the escape hatch is declaring a
    // measurement, not inventing an operator, and the section below says so.
    out.push_str("## Vocabulary — nothing outside these is accepted in a condition\n");
    out.push_str(&format!("condition fields: {fields}\n"));
    out.push_str(&format!("condition functions: {funcs}\n"));
    out.push_str("`risk.stop` is exactly one of:\n");
    for example in STOP_EXAMPLES {
        out.push_str(&format!("  stop: {example}\n"));
    }
    out.push_str(&format!(
        "Optional `risk.take_profit` uses `type`, not `kind`: \
         `{{ type: {}, value: 2.0 }}` (kinds: {}).\n",
        TAKE_PROFIT_KINDS[0],
        TAKE_PROFIT_KINDS.join(", ")
    ));
    out.push_str(&format!(
        "kind: indicator, strategy, bot. max_risk_pct must be <= {}.\n\n",
        strategy_dsl::MAX_RISK_PCT_CEILING
    ));

    out.push_str("## Concepts — when the idea is not in that list\n");
    out.push_str(
        "Users describe patterns the vocabulary above cannot express: a fair value \
         gap, an order block, a breaker block. Do not approximate one with `delta` \
         and hope. Declare it. A `concepts:` block defines a measurement as data -- \
         a window of candles, two selectors giving the band its edges, and the \
         relations that make it that pattern:\n\n",
    );
    out.push_str("```yaml\n");
    out.push_str(
        &CONCEPT_EXAMPLE
            .replace("{market}", market)
            .replace("{entry_timeframe}", entry_timeframe),
    );
    out.push_str("```\n\n");
    out.push_str(&format!(
        "A selector is a candle value inside the window, counted from the oldest (0), \
         written as a one-key mapping: `{{high: 0}}`. Selectors: {selectors}.\n"
    ));
    out.push_str(&format!("`op` is one of: {ops}.\n"));
    out.push_str(&format!(
        "`window` is {}..={}: how many candles the pattern spans.\n",
        analytics_core::concepts::MIN_WINDOW,
        analytics_core::concepts::MAX_WINDOW
    ));
    out.push_str(
        "`side` is `buy` or `sell` -- lowercase, unlike the rest of the wire. \
         `min_band_ratio` is optional: the band must be at least that share of the \
         window's own range, which is how a loose pattern is stopped from matching \
         every window.\n",
    );
    out.push_str(&format!(
        "At most {} concepts per document.\n\n",
        strategy_dsl::validator::Limits::default().max_concepts
    ));
    out.push_str("Then read it from a condition, as `concepts.<name>.<part>`:\n");
    for part in ConceptPart::ALL {
        out.push_str(&format!(
            "  concepts.<name>.{:<9} -> {:<6} {}\n",
            part.name(),
            part.type_of().name(),
            part.description()
        ));
    }
    out.push_str(
        "\nThe parts describe the **newest** band the concept finds. A concept that \
         found nothing is `false` for a boolean part and absent for a number, and a \
         comparison against an absent value is false -- never true against zero.\n\n",
    );

    out.push_str("## Rules that most often fail validation\n");
    out.push_str(
        "- every `timeframe:` in a condition must be declared under `timeframes`;\n\
         - `entry` needs at least one condition in `all_of` or `any_of`;\n\
         - `invalidation` must not be empty;\n\
         - `risk.stop` is one of the stops above, not a bare price; the `atr` \
           and `fixed` kinds require an explicit `entry.direction`, because \
           unlike the level-based stops they carry no side of their own;\n\
         - a condition may only read a concept the *same* document declares, so a \
           `concepts:` block and the conditions using it go in together;\n\
         - every selector index in a concept must be inside its own `window`;\n\
         - the two selectors of a `require` entry must read the same kind of thing \
           -- price against price, volume against volume.\n",
    );

    out.push_str("## Type rules for conditions\n");
    out.push_str(
        "Every `condition:` must evaluate to **boolean**. The type checker \
         enforces this strictly. Common mistakes:\n\n\
         1. **Never use a bare string field as a condition.** Fields like \
           `trend`, `divergence`, `market_structure.trend`, `market_structure.break`, \
           `market_structure.break_direction`, and `liquidity_swept` are strings \
           (e.g. `\"bullish\"`, `\"bos\"`, `\"buy_side\"`). Writing \
           `condition: \"market_structure.trend\"` is a string, not a boolean. \
           Always compare: `condition: \"market_structure.trend == \\\"bullish\\\"\"`.\n\
         2. **String fields must be compared with quoted string literals.** \n           `market_structure.trend == bullish` fails because `bullish` is parsed \
           as a bool, not a string. The correct form is \
           `condition: \"market_structure.trend == \\\"bullish\\\"\"`.\n\
         3. **Bool fields must not be compared with `==`.** Fields like \
           `absorption_detected`, `absorption_bullish`, `imbalance_detected`, \
           `imbalance_buy`, `in_position` are already booleans. Write \
           `condition: \"absorption_detected\"`, not \
           `condition: \"absorption_detected == true\"`.\n\
         4. **Functions like `close_below`, `above`, `below`, `crosses_above` \
           already return booleans.** Write `condition: \"close_below(vwap)\"`, \
           not `condition: \"close_below(vwap) == true\"`.\n\
         5. **String-to-bool and bool-to-string comparisons always fail.** You \
           cannot compare `trend == true` or `absorption_detected == \\\"yes\\\"`.\n\n",
    );
    out.push_str("String fields: trend, divergence, market_structure.trend, market_structure.break, market_structure.break_direction, liquidity_swept\n");
    out.push_str("Bool fields: absorption_detected, absorption_bullish, absorption_bearish, imbalance_detected, imbalance_buy, imbalance_sell, imbalance_stacked, in_position\n\n");

    if let Some(skill) = skill {
        out.push_str("\n## Skill to encode\n");
        out.push_str(&skill.render());
        out.push('\n');
    }
    out
}

/// Every stop the DSL accepts, written exactly as it must appear as the value
/// of `risk.stop`.
///
/// Both shapes are real and they are not interchangeable: the four level-based
/// stops are bare strings, while the four parameterized ones are mappings under
/// a `kind` key. Listing the parameterized stops as bare words (as this did at
/// first) makes the model write `stop: below_recent_low`, which fails with
/// "requires `bars`" — a message the model then has to guess its way out of.
/// `stop_examples_still_deserialize` guards the list against drift.
const STOP_EXAMPLES: &[&str] = &[
    "below_sweep_low",
    "above_sweep_high",
    "below_swing_low",
    "above_swing_high",
    "{ kind: below_recent_low, bars: 20 }",
    "{ kind: above_recent_high, bars: 20 }",
    "{ kind: atr, multiple: 1.5, period: 14 }",
    "{ kind: fixed, price: 100.0 }",
];

/// Take-profit kinds, which use `type` where the stop uses `kind`.
const TAKE_PROFIT_KINDS: &[&str] = &["risk_multiple", "atr_multiple", "fixed_price"];

/// The tool the model calls to hand back a drafted strategy.
pub const DRAFT_STRATEGY: &str = "draft_strategy";

/// Schema for [`DRAFT_STRATEGY`].
#[must_use]
pub fn draft_strategy_spec() -> ToolSpec {
    ToolSpec {
        name: DRAFT_STRATEGY.into(),
        description: "Hand back a Strategy DSL document as YAML. It is validated \
                      by the platform; if it fails you are told exactly which \
                      fields are wrong and you draft again."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "yaml": {
                    "type": "string",
                    "description": "The complete strategy document, as YAML."
                }
            },
            "required": ["yaml"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{ContentBlock, LlmResponse, ScriptedClient, StopReason};
    use crate::tools::MarketDataSource;
    use analytics_core::types::{Candle, Trade};
    use analytics_core::Timeframe;
    use async_trait::async_trait;

    /// Serves candles on every timeframe, counting back from a fixed "now".
    struct Fixture {
        now: i64,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                now: 1_700_000_000_000_000_000,
            }
        }
    }

    #[async_trait]
    impl MarketDataSource for Fixture {
        async fn candles(
            &self,
            symbol: &str,
            timeframe: Timeframe,
            from_ns: i64,
            to_ns: i64,
        ) -> Result<Vec<Candle>, AgentError> {
            let width = timeframe.nanos();
            let mut t = self.now - 400 * width;
            let mut i = 0_usize;
            let mut out = Vec::new();
            while t < to_ns {
                // A gentle uptrend so trend detection has something to say.
                let price = 100_000.0 + (i % 40) as f64 * 10.0;
                if t >= from_ns {
                    out.push(Candle {
                        symbol: symbol.into(),
                        timeframe,
                        open_time: t,
                        open: price,
                        high: price + 25.0,
                        low: price - 25.0,
                        close: price + 5.0,
                        volume: 100.0,
                        buy_volume: 60.0,
                        sell_volume: 40.0,
                    });
                }
                t += width;
                i += 1;
            }
            Ok(out)
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
            timeframe: Timeframe,
        ) -> Result<Option<i64>, AgentError> {
            Ok(Some(timeframe.bucket_of(self.now)))
        }
    }

    fn thesis_call(entry: f64, stop: f64, target: f64) -> LlmResponse {
        let input = json!({
            "symbol": "BTCUSDT", "timeframe": "5m", "direction": "long",
            "confidence_pct": 62.0,
            "entry_price": entry, "stop_price": stop, "target_price": target,
            "invalidation": "5m close below the swept low",
            "higher_timeframe_checks": [
                {"label": "1d trend", "status": "pass", "detail": "bullish", "source": "analyze_timeframe"}
            ],
            "order_flow_checks": [
                {"label": "absorption", "status": "unknown", "detail": "no tick data", "source": "detect_absorption"}
            ],
        });
        LlmResponse {
            message: Message {
                role: crate::llm_client::Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: SUBMIT_THESIS.into(),
                    input,
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: Some(10),
                output_tokens: Some(20),
            },
        }
    }

    fn agent(responses: Vec<LlmResponse>) -> Agent {
        Agent::new(
            Arc::new(ScriptedClient::new(responses)),
            SkillLibrary::new(),
            AgentConfig::default(),
        )
    }

    /// The system prompt the orchestrator actually sent.
    ///
    /// Asserting on this rather than on a prompt the test builds itself is the
    /// point: the failure worth catching is the orchestrator forgetting to pass
    /// the chart through, and a test that constructs the prompt directly would
    /// pass while the loop dropped the packet on the floor.
    fn system_prompt_of(llm: &Arc<ScriptedClient>) -> String {
        llm.requests()
            .into_iter()
            .next()
            .and_then(|request| request.system)
            .expect("the orchestrator must send a system prompt")
    }

    #[tokio::test]
    async fn a_question_produces_a_thesis_grounded_in_the_ladder() {
        let source = Fixture::new();
        let agent = agent(vec![thesis_call(100_100.0, 100_000.0, 100_400.0)]);

        let answer = agent
            .ask(&AskRequest::new("BTCUSDT", "find me a long setup"), &source)
            .await
            .unwrap();

        assert_eq!(answer.thesis.direction, crate::thesis::Bias::Long);
        assert_eq!(answer.turns, 1);
        // (100400-100100) / (100100-100000) == 3.0
        assert!(
            (answer.thesis.risk_reward - 3.0).abs() < 1e-6,
            "got {}",
            answer.thesis.risk_reward
        );
        // Provenance: the ladder was read before the model spoke.
        assert!(!answer.trace.is_empty());
        assert!(answer.trace.iter().all(|t| t.tool == "analyze_timeframe"));
        assert!(!answer.thesis.narrative.is_empty());
    }

    /// The user's resolution joins the ladder rather than replacing it.
    ///
    /// This is the whole point of the packet: a question asked while looking at
    /// the 5m chart must be answered with the 5m chart *in view*, but the
    /// higher-timeframe context is what makes the answer more than a zoomed-in
    /// guess. Dropping the ladder would trade one failure for another.
    #[tokio::test]
    async fn the_viewport_resolution_joins_the_ladder_and_does_not_replace_it() {
        let source = Fixture::new();
        let llm = Arc::new(ScriptedClient::new(vec![thesis_call(
            100_100.0, 100_000.0, 100_400.0,
        )]));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let chart = crate::chart_context::ChartContext {
            // Not in the default 1D/4H/1H/5M ladder.
            timeframe: Some(Timeframe::M15),
            ..Default::default()
        };

        agent
            .ask(
                &AskRequest::new("BTCUSDT", "what is this level?").with_chart(chart),
                &source,
            )
            .await
            .unwrap();

        let system = system_prompt_of(&llm);
        // The default ladder survived...
        for expected in ["1d", "4h", "1h", "5m"] {
            assert!(
                system.contains(expected),
                "the default ladder lost {expected} when the viewport was attached: {system}"
            );
        }
        // ...and the user's resolution was added to it, not substituted for it.
        assert!(
            system.contains("15m"),
            "the viewport resolution is missing from the ladder: {system}"
        );
        assert!(
            system.contains("What the user is looking at"),
            "the viewport section is missing: {system}"
        );
    }

    /// A chart-aware question tells the model what "this" means.
    #[tokio::test]
    async fn a_viewport_question_is_told_that_deictics_mean_the_viewport() {
        let source = Fixture::new();
        let llm = Arc::new(ScriptedClient::new(vec![thesis_call(
            100_100.0, 100_000.0, 100_400.0,
        )]));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let chart = crate::chart_context::ChartContext {
            timeframe: Some(Timeframe::H1),
            visible_from_ns: Some(1_788_739_200 * 1_000_000_000),
            visible_to_ns: Some((1_788_739_200 + 11 * 3600) * 1_000_000_000),
            drawings: vec![crate::chart_context::DrawnLevel {
                kind: "horizontal".into(),
                price: Some(112_500.0),
                label: Some("weekly open".into()),
            }],
            ..Default::default()
        };

        agent
            .ask(
                &AskRequest::new("BTCUSDT", "do we hold this?").with_chart(chart),
                &source,
            )
            .await
            .unwrap();

        let system = system_prompt_of(&llm);
        assert!(system.contains("2026-09-07T00:00:00Z"), "{system}");
        assert!(system.contains("112500.0000"), "{system}");
        assert!(system.contains("weekly open"), "{system}");
        assert!(
            system.contains("\"this\", \"here\", \"that level\""),
            "without this the model answers about the latest bar, which is the \
             bug the whole packet exists to fix: {system}"
        );
    }

    /// A request with no chart renders exactly the prompt it always did.
    ///
    /// The regression that would otherwise be invisible: an older client sends
    /// no `chart`, and if the empty packet still emitted a heading the model
    /// would read "What the user is looking at" with nothing under it -- and
    /// speculate about what was withheld.
    #[tokio::test]
    async fn a_request_without_a_chart_produces_an_unchanged_prompt() {
        let source = Fixture::new();
        let llm = Arc::new(ScriptedClient::new(vec![thesis_call(
            100_100.0, 100_000.0, 100_400.0,
        )]));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        agent
            .ask(&AskRequest::new("BTCUSDT", "find me a long setup"), &source)
            .await
            .unwrap();

        let system = system_prompt_of(&llm);
        assert!(
            !system.contains("What the user is looking at"),
            "an absent chart must not leave a heading behind: {system}"
        );
        assert!(!system.contains("deictic"), "{system}");
    }

    /// A screenshot reaches the model as an image block, not as prose.
    ///
    /// Sending it as text would be the quiet failure: the request succeeds, the
    /// model is told a screenshot exists, and it never actually sees one -- so
    /// it answers as though the user had sent nothing and the user has no way to
    /// tell.
    #[tokio::test]
    async fn a_screenshot_is_sent_as_an_image_block_ahead_of_the_question() {
        let source = Fixture::new();
        let llm = Arc::new(ScriptedClient::new(vec![thesis_call(
            100_100.0, 100_000.0, 100_400.0,
        )]));
        let agent = Agent::new(llm.clone(), SkillLibrary::new(), AgentConfig::default());

        let chart = crate::chart_context::ChartContext {
            timeframe: Some(Timeframe::H1),
            screenshot: Some(crate::chart_context::ChartScreenshot {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
                label: None,
            }),
            ..Default::default()
        };

        agent
            .ask(
                &AskRequest::new("BTCUSDT", "what do you see?").with_chart(chart),
                &source,
            )
            .await
            .unwrap();

        let request = llm.requests().into_iter().next().expect("a request");
        let first = request.messages.first().expect("a first user message");
        assert_eq!(first.content.len(), 2, "image plus text: {first:?}");
        match &first.content[0] {
            ContentBlock::Image { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "iVBORw0KGgo=");
            }
            other => panic!("the image must come first, got {other:?}"),
        }
        match &first.content[1] {
            ContentBlock::Text(text) => assert_eq!(text, "what do you see?"),
            other => panic!("the question must follow the image, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_run_reports_the_steps_it_took() {
        // The panel has nothing to draw without these, and a run that reported
        // nothing is indistinguishable from one that has hung -- which is the
        // difference between waiting and reloading.
        let source = Fixture::new();
        let agent = agent(vec![thesis_call(100_100.0, 100_000.0, 100_400.0)]);
        let progress = crate::progress::Collected::default();

        agent
            .ask_with_progress(
                &AskRequest::new("BTCUSDT", "find me a long setup"),
                &source,
                &progress,
            )
            .await
            .unwrap();

        let steps = progress.steps();
        // The ladder is read first, before the model is asked anything.
        assert!(
            matches!(
                steps.first(),
                Some(Progress::ReadingMarket { timeframes, .. }) if *timeframes > 0
            ),
            "{steps:?}"
        );
        // Then the model is called, and the call is announced. The fixture
        // answers on its first turn, so that turn is an analysis turn.
        assert_eq!(
            steps.get(1),
            Some(&Progress::Thinking {
                turn: 1,
                total: DEFAULT_MAX_TURNS + ANSWER_TURNS,
                answering: false,
            }),
            "{steps:?}"
        );
        // It answered without asking for a tool, because the ladder came from
        // the orchestrator -- so there is nothing else to report.
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, Progress::Tool { .. } | Progress::ToolDone { .. })),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_tool_call_is_reported_with_how_it_went() {
        let source = Fixture::new();
        let agent = agent(vec![
            named_call("get_vwap"),
            thesis_call(100_100.0, 100_000.0, 100_400.0),
        ]);
        let progress = crate::progress::Collected::default();

        agent
            .ask_with_progress(
                &AskRequest::new("BTCUSDT", "find me a long setup"),
                &source,
                &progress,
            )
            .await
            .unwrap();

        let steps = progress.steps();
        let called = steps
            .iter()
            .position(|s| matches!(s, Progress::Tool { name } if name == "get_vwap"));
        let done = steps
            .iter()
            .position(|s| matches!(s, Progress::ToolDone { name, ok: true } if name == "get_vwap"));
        assert!(called.is_some() && done.is_some(), "{steps:?}");
        // Announced before it ran, and answered after -- the order is what lets
        // a panel show a tool as in-flight rather than only as finished.
        assert!(called < done, "{steps:?}");
    }

    #[tokio::test]
    async fn a_tool_that_failed_is_reported_as_having_failed() {
        let source = Fixture::new();
        // A tool the registry does not have. A failure the model can read is
        // information rather than an abort, so the run continues -- but the
        // panel must not show a step that quietly succeeded.
        let agent = agent(vec![
            named_call("no_such_tool"),
            thesis_call(100_100.0, 100_000.0, 100_400.0),
        ]);
        let progress = crate::progress::Collected::default();

        agent
            .ask_with_progress(
                &AskRequest::new("BTCUSDT", "find me a long setup"),
                &source,
                &progress,
            )
            .await
            .unwrap();

        let steps = progress.steps();
        assert!(
            steps.iter().any(
                |s| matches!(s, Progress::ToolDone { name, ok: false } if name == "no_such_tool")
            ),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn a_thesis_citing_prices_no_tool_reported_is_rejected() {
        let source = Fixture::new();
        // 250_000 is nowhere near the 100k-ish fixture. Repeated because a
        // rejected thesis is now handed back for correction rather than
        // failing on the spot -- so the bad level has to survive every
        // attempt for the error to reach the caller.
        let agent = agent(vec![
            thesis_call(250_000.0, 249_000.0, 253_000.0);
            DEFAULT_MAX_TURNS + ANSWER_TURNS
        ]);

        let err = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::Ungrounded(_)), "got {err}");
    }

    fn named_call(name: &str) -> LlmResponse {
        LlmResponse {
            message: Message {
                role: crate::llm_client::Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: name.into(),
                    input: json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn the_answer_turns_cannot_be_spent_on_more_data() {
        // The bug, seen live: on the turn reserved for the answer the model
        // called `detect_liquidity` out of habit, and the registry happily ran
        // it -- so the reserved turn produced data instead of a thesis and the
        // question failed. Announcing only `submit_thesis` did not stop it;
        // refusing the call does.
        let source = Fixture::new();
        // Exploration turns say nothing, so the only data call in the script
        // is the one that must be refused.
        let mut responses = Vec::new();
        for _ in 0..DEFAULT_MAX_TURNS {
            responses.push(LlmResponse {
                message: Message {
                    role: crate::llm_client::Role::Assistant,
                    content: vec![ContentBlock::Text("thinking".into())],
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            });
        }
        responses.push(named_call("detect_liquidity")); // the refused one
        responses.push(thesis_call(100_100.0, 100_000.0, 100_400.0));

        let agent = agent(responses);
        let answer = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap();

        assert_eq!(answer.turns, DEFAULT_MAX_TURNS + 2);
        // The refused call must not have reached the tools, or the turn was
        // spent on data after all.
        assert!(
            !answer.trace.iter().any(|t| t.tool == "detect_liquidity"),
            "the answer phase ran a data tool: {:?}",
            answer.trace.iter().map(|t| &t.tool).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_rejected_thesis_is_handed_back_for_correction() {
        let source = Fixture::new();
        // First attempt cites a level no tool reported; the model is told
        // exactly why and fixes it on the second.
        let agent = agent(vec![
            thesis_call(250_000.0, 249_000.0, 253_000.0),
            thesis_call(100_100.0, 100_000.0, 100_400.0),
        ]);

        let answer = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap();

        assert_eq!(answer.turns, 2, "the correction should be a second turn");
        assert!(
            (answer.thesis.risk_reward - 3.0).abs() < 1e-6,
            "got {}",
            answer.thesis.risk_reward
        );
    }

    #[tokio::test]
    async fn a_model_that_only_writes_prose_never_yields_a_thesis() {
        let source = Fixture::new();
        let prose = LlmResponse {
            message: Message {
                role: crate::llm_client::Role::Assistant,
                content: vec![ContentBlock::Text("BTC looks bullish.".into())],
            },
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        };
        let agent = agent(vec![prose; 8]);

        let err = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::NoThesis { .. }), "got {err}");
    }

    #[tokio::test]
    async fn a_data_tool_call_is_executed_and_recorded_before_the_thesis() {
        use crate::llm_client::Role;

        let source = Fixture::new();
        let first = LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "get_vwap".into(),
                    input: json!({"symbol": "BTCUSDT", "timeframe": "5m"}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        };
        let agent = agent(vec![first, thesis_call(100_100.0, 100_000.0, 100_400.0)]);

        let answer = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap();

        assert!(answer.trace.iter().any(|t| t.tool == "get_vwap"));
        assert_eq!(answer.turns, 2);
    }

    #[tokio::test]
    async fn a_failing_tool_does_not_kill_the_turn() {
        use crate::llm_client::Role;

        let source = Fixture::new();
        let bad = LlmResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "analyze_timeframe".into(),
                    input: json!({"symbol": "BTCUSDT", "timeframe": "nonsense"}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        };
        let agent = agent(vec![bad, thesis_call(100_100.0, 100_000.0, 100_400.0)]);

        let answer = agent
            .ask(&AskRequest::new("BTCUSDT", "long setup"), &source)
            .await
            .unwrap();
        assert_eq!(answer.turns, 2);
    }

    const VALID_YAML: &str = r#"
name: "VWAP reclaim"
version: "1.0"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  all_of:
    - timeframe: entry
      condition: "close_above(vwap)"
risk:
  max_risk_pct: 1.0
  stop: below_swing_low
invalidation:
  - timeframe: entry
    condition: "close_below(vwap)"
"#;

    fn draft(yaml: &str) -> LlmResponse {
        LlmResponse {
            message: Message {
                role: crate::llm_client::Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "d1".into(),
                    name: DRAFT_STRATEGY.into(),
                    input: json!({"yaml": yaml}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn a_valid_first_draft_is_returned_after_one_attempt() {
        let agent = agent(vec![draft(VALID_YAML)]);
        let out = agent
            .generate_strategy(&StrategyRequest::new(
                "buy when price reclaims vwap",
                "BTCUSDT",
                "5m",
            ))
            .await
            .unwrap();
        assert_eq!(out.attempts, 1);
        assert!(out.repaired_errors.is_empty());
        assert_eq!(out.document().market, "BTCUSDT");
    }

    #[tokio::test]
    async fn an_invalid_first_draft_is_corrected_on_retry() {
        // First draft: risk above the ceiling and an empty invalidation.
        let bad = r#"
name: "Too risky"
version: "1.0"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  all_of:
    - timeframe: entry
      condition: "close_above(vwap)"
risk:
  max_risk_pct: 40.0
  stop: below_swing_low
invalidation: []
"#;
        let agent = agent(vec![draft(bad), draft(VALID_YAML)]);
        let out = agent
            .generate_strategy(&StrategyRequest::new("x", "BTCUSDT", "5m"))
            .await
            .unwrap();

        assert_eq!(out.attempts, 2);
        assert_eq!(out.repaired_errors.len(), 1);
        assert!(out.repaired_errors[0].contains("max_risk_pct"));
    }

    #[tokio::test]
    async fn a_draft_for_the_wrong_timeframe_is_sent_back() {
        // Valid, and not what was asked for: a live draft answered a request
        // for 5m with `entry: 15m`. `strategy-dsl` has no opinion on that, so
        // this layer has to.
        let wrong_tf = VALID_YAML.replace("entry: \"5m\"", "entry: \"15m\"");
        assert_ne!(wrong_tf, VALID_YAML, "the fixture must actually differ");

        let agent = agent(vec![draft(&wrong_tf), draft(VALID_YAML)]);
        let out = agent
            .generate_strategy(&StrategyRequest::new("x", "BTCUSDT", "5m"))
            .await
            .unwrap();

        assert_eq!(out.attempts, 2);
        assert!(
            out.repaired_errors[0].contains("timeframes.entry"),
            "got {:?}",
            out.repaired_errors
        );
        assert_eq!(
            out.document().timeframes.get("entry").unwrap().to_string(),
            "5m"
        );
    }

    #[tokio::test]
    async fn a_document_that_never_validates_reports_the_last_specific_error() {
        let bad = r#"
name: "Broken"
version: "1.0"
kind: strategy
market: "BTCUSDT"
timeframes:
  entry: "5m"
entry:
  all_of: []
risk:
  max_risk_pct: 40.0
  stop: below_swing_low
invalidation: []
"#;
        let agent = agent(vec![draft(bad), draft(bad), draft(bad)]);
        let err = agent
            .generate_strategy(&StrategyRequest::new("x", "BTCUSDT", "5m"))
            .await
            .unwrap_err();

        assert!(
            matches!(err, AgentError::InvalidStrategyDocument(_)),
            "got {err}"
        );
        // The surfaced error names the offending field, not "invalid document".
        assert!(err.to_string().contains("max_risk_pct"), "got {err}");
    }

    #[test]
    fn stop_examples_still_deserialize() {
        // Guards the prompt's stop list against drift from `strategy-dsl`.
        // This caught the parameterized stops being listed as bare words.
        for example in STOP_EXAMPLES {
            let parsed = serde_yaml::from_str::<strategy_dsl::StopSpec>(example);
            assert!(
                parsed.is_ok(),
                "`{example}` is no longer a valid stop: {parsed:?}"
            );
        }
    }

    #[test]
    fn the_shape_the_prompt_shows_actually_validates() {
        // The prompt tells the model to copy this. If it does not validate,
        // the model copies a broken document and then has to guess how to fix
        // errors the prompt itself produced.
        let doc = STRATEGY_SHAPE_TEMPLATE
            .replace("{market}", "BTCUSDT")
            .replace("{entry_timeframe}", "5m");
        match strategy_dsl::parse_and_validate(&doc) {
            Ok(_) => {}
            Err(err) => panic!("the prompt's own shape no longer validates:\n{doc}\n{err}"),
        }
    }

    #[test]
    fn the_prompt_warns_about_the_mistake_the_model_actually_makes() {
        // A live generation put `value:` inside an `all_of` entry -- copied
        // from `risk.take_profit`. Naming it beats hoping.
        let prompt = strategy_system_prompt("BTCUSDT", "5m", None);
        assert!(
            prompt.contains("no `value` here"),
            "prompt does not warn about `value` in conditions"
        );
    }

    #[test]
    fn the_strategy_prompt_lists_the_real_vocabulary() {
        let prompt = strategy_system_prompt("BTCUSDT", "5m", None);
        for func in strategy_dsl::ALL_FUNCS {
            assert!(
                prompt.contains(func.name()),
                "prompt is missing `{}`",
                func.name()
            );
        }
        for field in strategy_dsl::ALL_FIELDS {
            assert!(
                prompt.contains(field.name()),
                "prompt is missing `{}`",
                field.name()
            );
        }
        assert!(
            prompt.contains("5.0") || prompt.contains("5"),
            "risk ceiling"
        );
    }

    #[test]
    fn the_concept_example_the_prompt_shows_actually_validates() {
        // The same argument as `the_shape_the_prompt_shows_actually_validates`,
        // and it matters more here. The concept block is the one part of the
        // language a model has no prior for -- it has never seen this DSL -- so
        // it copies the example character for character. If the example is
        // wrong the model cannot recover, because it has nothing to correct
        // *toward*.
        let doc = CONCEPT_EXAMPLE
            .replace("{market}", "BTCUSDT")
            .replace("{entry_timeframe}", "5m");
        let validated = strategy_dsl::parse_and_validate(&doc).unwrap_or_else(|err| {
            panic!("the prompt's own concept example no longer validates:\n{doc}\n{err}")
        });

        // And non-vacuously: a document with no `concepts:` block would sail
        // through the check above while teaching the model nothing. The example
        // has to actually declare the concept its conditions read.
        assert_eq!(
            validated.document().concepts.len(),
            1,
            "the example must declare a concept, or validating it proves nothing"
        );
        assert_eq!(validated.document().concepts[0].name, "gap");
        assert!(
            doc.contains("concepts.gap."),
            "the example must read the concept it declares:\n{doc}"
        );
    }

    #[test]
    fn the_prompt_describes_the_whole_concept_language() {
        // Rendered from the enums, so this is really a check that nothing was
        // left out of the rendering. A part the model is never told about is a
        // part it will never write, and that failure is silent: documents keep
        // validating, they just never use the language.
        let prompt = strategy_system_prompt("BTCUSDT", "5m", None);

        for part in ConceptPart::ALL {
            assert!(
                prompt.contains(part.name()),
                "prompt is missing concept part `{}`",
                part.name()
            );
            assert!(
                prompt.contains(part.description()),
                "prompt does not say what `{}` reads",
                part.name()
            );
        }

        // Exact lists rather than word-by-word containment: `close` and `below`
        // appear all over this prompt for other reasons, so `contains("close")`
        // would pass on a prompt that never mentioned selectors at all.
        let selectors = analytics_core::concepts::Selector::NAMES.join(", ");
        assert!(
            prompt.contains(&selectors),
            "prompt does not list the concept selectors: {selectors}"
        );
        let ops = analytics_core::concepts::Compare::ALL
            .iter()
            .map(|op| op.name())
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            prompt.contains(&ops),
            "prompt does not list the comparison ops: {ops}"
        );
        assert!(
            prompt.contains(&format!(
                "{}..={}",
                analytics_core::concepts::MIN_WINDOW,
                analytics_core::concepts::MAX_WINDOW
            )),
            "prompt does not give the window bounds"
        );

        // Both halves of the language, not just one. Listing the parts without
        // showing how to declare a concept would leave the model able to read a
        // measurement it has no way to create -- and listing the block without
        // the reference syntax leaves it with a definition it never uses.
        assert!(
            prompt.contains("concepts:"),
            "the prompt never shows a concepts block"
        );
        assert!(
            prompt.contains("concepts.<name>."),
            "the prompt never shows how a condition reads a concept"
        );
    }

    #[test]
    fn usage_accumulates_across_calls() {
        let a = Usage {
            input_tokens: Some(1),
            output_tokens: None,
        };
        let b = Usage {
            input_tokens: Some(2),
            output_tokens: Some(3),
        };
        let sum = a + b;
        assert_eq!(sum.input_tokens, Some(3));
        assert_eq!(sum.output_tokens, Some(3));
    }
}
