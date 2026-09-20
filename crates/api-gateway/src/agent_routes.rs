//! `/agent/ask` and `/agent/generate-strategy` (`docs/12`).
//!
//! These are the two endpoints the Phase 5 chart talks to.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use ai_agent::{AgentAnswer, AskRequest, StrategyRequest};

use crate::AppState;
use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::market_data::WindowMarketData;

/// Body of `POST /agent/ask`.
#[derive(Debug, Deserialize)]
pub struct AskBody {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// The question, in plain language.
    pub question: String,
    /// Pin a skill by id instead of retrieving by relevance.
    pub skill_id: Option<String>,
    /// Ladder override, coarse to fine, e.g. `["4h", "1h", "5m"]`.
    pub timeframes: Option<Vec<String>>,
    /// The chart the user is looking at when they ask.
    ///
    /// Optional, and absent for every existing client. See
    /// [`ai_agent::ChartContext`] for why this is a viewport rather than data:
    /// it says where to look, and carries no numbers the agent may cite.
    #[serde(default)]
    pub chart: Option<ai_agent::ChartContext>,
    /// Return every tool call and its raw result alongside the thesis.
    ///
    /// Off by default because a four-timeframe ladder of full `MarketState`s
    /// is a large payload, and the chart only needs it for the audit view.
    /// The calls are always logged server-side regardless.
    #[serde(default)]
    pub include_trace: bool,
}

/// Response of `POST /agent/ask`.
#[derive(Debug, Serialize)]
pub struct AskResponse {
    /// The explainable thesis.
    pub thesis: ai_agent::TradeThesis,
    /// The skill that supplied the methodology, if one was retrieved.
    pub skill: Option<String>,
    /// Model round trips used.
    pub turns: usize,
    /// Per-timeframe summary the model was shown.
    pub ladder: Vec<FrameSummary>,
    /// The viewport the agent was told about, echoed back.
    ///
    /// Echoed rather than omitted because the user needs to be able to check
    /// that the agent reasoned about the chart they were actually on. A stale
    /// viewport is a plausible failure -- the user pans, then types -- and
    /// without this the answer would look wrong for reasons nothing explains.
    /// The screenshot is reported by description, never by payload: an audit
    /// view that re-downloads a megabyte of PNG is not an audit view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart: Option<ChartSummary>,
    /// Present only when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Vec<ai_agent::ToolTrace>>,
}

/// What the agent was told about the viewport, minus the image.
#[derive(Debug, Serialize)]
pub struct ChartSummary {
    /// Resolution that anchored the ladder, e.g. `1h`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<String>,
    /// Bars the viewport spanned, when the resolution was known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_bars: Option<i64>,
    /// How many drawings were carried.
    pub drawings: usize,
    /// A description of the screenshot, when one was attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
}

impl ChartSummary {
    fn from_context(chart: &ai_agent::ChartContext) -> Self {
        Self {
            timeframe: chart.timeframe.map(|tf| tf.to_string()),
            visible_bars: chart.visible_bars().filter(|bars| *bars >= 0),
            drawings: chart.drawings.len(),
            screenshot: chart.screenshot.as_ref().map(|s| s.describe()),
        }
    }
}

/// One line of the ladder, for rendering next to the chart.
#[derive(Debug, Serialize)]
pub struct FrameSummary {
    /// Timeframe, e.g. `4h`.
    pub timeframe: String,
    /// Last price on that timeframe.
    pub price: f64,
    /// Trend as reported by analytics-core.
    pub trend: String,
}

/// Body of `POST /agent/generate-strategy`.
#[derive(Debug, Deserialize)]
pub struct GenerateStrategyBody {
    /// What the strategy should do, in plain language.
    pub description: String,
    /// Market the strategy trades.
    pub market: Option<String>,
    /// Timeframe the entry condition fires on.
    pub timeframe: Option<String>,
    /// Pin a skill whose methodology to encode.
    pub skill_id: Option<String>,
}

/// Response of `POST /agent/generate-strategy`.
#[derive(Debug, Serialize)]
pub struct GenerateStrategyResponse {
    /// The validated document.
    pub document: ai_agent::StrategyDocument,
    /// The YAML as produced, for storage and diffing.
    pub yaml: String,
    /// Draft/validate round trips.
    pub attempts: usize,
    /// Validation errors that were fed back and fixed.
    pub repaired_errors: Vec<String>,
}

/// Spend one of the caller's agent tokens, or refuse.
///
/// `docs/12` asks for per-user limits on `/agent/*` specifically, because these
/// are the endpoints that cost money per call. The limit is checked before the
/// agent is even looked at, so a refused request does no work.
pub(crate) fn check_agent_limit(state: &AppState, user: &UserContext) -> Result<(), ApiError> {
    state
        .agent_limits
        .check(user.user_id, crate::auth::now_seconds() as f64)
        .map_err(|limited| {
            ApiError::coded(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                format!(
                    "too many agent requests. This endpoint calls a paid model, so it is limited to {} a minute with a burst of {}. Try again in {} seconds.",
                    state.agent_limits.limit().per_minute,
                    state.agent_limits.limit().burst,
                    limited.retry_after_seconds
                ),
            )
            .with_retry_after(limited.retry_after_seconds)
        })
}

/// `POST /agent/ask`
pub async fn ask(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<AskBody>,
) -> Result<Json<AskResponse>, ApiError> {
    check_agent_limit(&state, &user)?;
    let agent = state.agent.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID \
             and AWS credentials",
        )
    })?;

    let mut request = AskRequest::new(&body.symbol, &body.question);
    if let Some(id) = &body.skill_id {
        request = request.with_skill(id);
    }
    if let Some(frames) = &body.timeframes {
        request = request.with_timeframes(frames.clone());
    }
    if let Some(mut chart) = body.chart {
        // Validated *here*, before the rate limit is spent and before the model
        // is called. A screenshot the provider cannot decode would otherwise
        // cost a full paid turn and surface as an opaque upstream error, and a
        // user has no way to tell that from the model failing to understand.
        if let Some(screenshot) = chart.screenshot.take() {
            chart.screenshot = Some(screenshot.validate().map_err(|reason| {
                ApiError::coded(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "SCREENSHOT_INVALID",
                    reason,
                )
            })?);
        }
        // A busy chart is not an error, so the clamp trims rather than refuses.
        chart.clamp_drawings();
        if !chart.is_empty() {
            tracing::info!(
                target: "api_gateway",
                symbol = %body.symbol,
                context = %chart.summary(),
                "agent asked with chart context"
            );
            request = request.with_chart(chart);
        }
    }

    let started = std::time::Instant::now();
    state.metrics.count(
        observability::metrics::AGENT_REQUESTS,
        "Agent runs started",
        &observability::metrics::Labels::none(),
    );

    // A chart is entitled to a live feed even before any bot is running, and
    // asking the agent about a symbol is the same claim. Without this the
    // buffer never fills for that symbol and every read pays for the venue.
    state.bots.ensure_feed_for(&body.symbol.to_uppercase());

    let answer: AgentAnswer = agent
        .ask(&request, &WindowMarketData::new(state.windows.clone()))
        .await
        .map_err(ApiError::from)?;

    // Counted here rather than at the top so a refused or failed run does not
    // contribute a latency. An average that includes the fast failures is an
    // average that goes *down* when the provider starts rejecting, which is the
    // one moment the number is being read.
    state.metrics.observe(
        observability::metrics::AGENT_LATENCY,
        "Agent run duration in seconds",
        &observability::metrics::Labels::none(),
        started.elapsed().as_secs_f64(),
    );
    state.metrics.count(
        observability::metrics::AGENT_THESES,
        "Theses the agent produced",
        &observability::metrics::Labels::none(),
    );

    Ok(Json(to_response_with_chart(
        answer,
        body.include_trace,
        request.chart.as_ref(),
    )))
}

/// `POST /agent/generate-strategy`
pub async fn generate_strategy(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<GenerateStrategyBody>,
) -> Result<Json<GenerateStrategyResponse>, ApiError> {
    check_agent_limit(&state, &user)?;
    let agent = state.agent.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID \
             and AWS credentials",
        )
    })?;

    let mut request = StrategyRequest::new(
        &body.description,
        body.market.as_deref().unwrap_or("BTCUSDT"),
        body.timeframe.as_deref().unwrap_or("5m"),
    );
    request.skill_id = body.skill_id;

    let generated = agent
        .generate_strategy(&request)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(GenerateStrategyResponse {
        document: generated.document().clone(),
        yaml: generated.yaml,
        attempts: generated.attempts,
        repaired_errors: generated.repaired_errors,
    }))
}

pub(crate) fn to_response(answer: AgentAnswer, include_trace: bool) -> AskResponse {
    to_response_with_chart(answer, include_trace, None)
}

/// [`to_response`], echoing the viewport the agent was given.
pub(crate) fn to_response_with_chart(
    answer: AgentAnswer,
    include_trace: bool,
    chart: Option<&ai_agent::ChartContext>,
) -> AskResponse {
    let ladder = answer
        .ladder
        .frames
        .iter()
        .map(|frame| FrameSummary {
            timeframe: frame.timeframe.to_string(),
            price: frame.state.price,
            trend: format!("{:?}", frame.state.trend),
        })
        .collect();

    AskResponse {
        thesis: answer.thesis,
        skill: answer.skill,
        turns: answer.turns,
        ladder,
        chart: chart
            .filter(|context| !context.is_empty())
            .map(ChartSummary::from_context),
        trace: include_trace.then_some(answer.trace),
    }
}
