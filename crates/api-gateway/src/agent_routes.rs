//! `/agent/ask` and `/agent/generate-strategy` (`docs/12`).
//!
//! These are the two endpoints the Phase 5 chart talks to.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use ai_agent::{AgentAnswer, AskRequest, StrategyRequest};

use crate::error::ApiError;
use crate::market_data::DbMarketData;
use crate::AppState;

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
    /// Present only when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Vec<ai_agent::ToolTrace>>,
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

/// `POST /agent/ask`
pub async fn ask(
    State(state): State<AppState>,
    Json(body): Json<AskBody>,
) -> Result<Json<AskResponse>, ApiError> {
    let agent = state.agent.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID \
             and AWS credentials",
        )
    })?;
    let db = state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable("no database configured; the agent has no market data to read")
    })?;

    let mut request = AskRequest::new(&body.symbol, &body.question);
    if let Some(id) = &body.skill_id {
        request = request.with_skill(id);
    }
    if let Some(frames) = &body.timeframes {
        request = request.with_timeframes(frames.clone());
    }

    let answer: AgentAnswer = agent
        .ask(&request, &DbMarketData::new(db.clone()))
        .await
        .map_err(ApiError::from)?;

    Ok(Json(to_response(answer, body.include_trace)))
}

/// `POST /agent/generate-strategy`
pub async fn generate_strategy(
    State(state): State<AppState>,
    Json(body): Json<GenerateStrategyBody>,
) -> Result<Json<GenerateStrategyResponse>, ApiError> {
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

fn to_response(answer: AgentAnswer, include_trace: bool) -> AskResponse {
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
        trace: include_trace.then_some(answer.trace),
    }
}
