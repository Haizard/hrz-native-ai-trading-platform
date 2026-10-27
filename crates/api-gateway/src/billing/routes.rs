//! `/billing/*`: the plan catalog, a user's entitlements, and the provider
//! webhook (`docs/52`).
//!
//! ## Public versus authenticated
//!
//! The catalog is public: a pricing page has to render before sign-in, and it
//! contains no user data. Everything about *a* subscription is behind a bearer
//! token. The webhook is public in the HTTP sense -- a provider has no JWT --
//! and authenticated by its HMAC signature instead.
//!
//! ## Subscribe has an idempotency key, and it is the client's
//!
//! A double-click or a retried request must not start two periods. The key is
//! optional in the body because a curl client may not have one, but the
//! frontend always sends one. The database stores the last key on the row, and
//! a replay returns the current entitlements unchanged.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

use super::plans::{
    self, BillingCycle, Plan, PlanId, CREDIT_AUTHOR_TURN, CREDIT_BACKTEST_READ,
    CREDIT_CHART_ANALYSIS, CREDIT_QUICK_ANSWER, CREDIT_REVIEW, CREDIT_STRATEGY_GENERATION,
    CREDIT_WATCHER_RUN, CREDIT_WORKSPACE_TURN, TRIAL_DAYS,
};
use super::provider::BillingMode;
use super::service::{BillingService, Entitlements, UsageSummary, WebhookOutcome};
use super::BillingError;

/// What one action costs in AI credits, for the UI's cost hints.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CreditCosts {
    /// A chart question.
    pub chart_analysis: i64,
    /// A short answer without a chart.
    pub quick_answer: i64,
    /// Backtest interpretation.
    pub backtest_read: i64,
    /// Strategy generation.
    pub strategy_generation: i64,
    /// One zone-watcher run.
    pub watcher_run: i64,
    /// One workspace turn.
    pub workspace_turn: i64,
    /// Revision review.
    pub review: i64,
    /// Author-mode socket turn.
    pub author_turn: i64,
}

/// `GET /billing/plans`
#[derive(Debug, Clone, Serialize)]
pub struct PlansResponse {
    /// The complete matrix, cheapest first.
    pub plans: &'static [Plan; 5],
    /// Days in the card-free trial.
    pub trial_days: i64,
    /// Which lifecycle activates a subscription here.
    pub mode: BillingMode,
    /// Whether this deployment can process a subscription at all.
    pub billing_available: bool,
    /// Whether the community indicator library exists on this deployment.
    pub community_indicators_available: bool,
    /// Weighted credit costs.
    pub credit_costs: CreditCosts,
}

/// `POST /billing/subscribe`'s body.
#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    /// The plan to subscribe to.
    pub plan: String,
    /// `monthly` or `annual`; defaults to monthly.
    #[serde(default)]
    pub cycle: Option<String>,
    /// A client-generated key that makes the call safe to retry.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// `GET /billing/plans`
pub async fn plans(State(state): State<AppState>) -> Json<PlansResponse> {
    Json(PlansResponse {
        plans: plans::catalog(),
        trial_days: TRIAL_DAYS,
        mode: state
            .billing
            .as_ref()
            .map_or(BillingMode::Internal, |billing| billing.mode()),
        billing_available: state.billing.is_some(),
        community_indicators_available: state.db.is_some(),
        credit_costs: CreditCosts {
            chart_analysis: CREDIT_CHART_ANALYSIS,
            quick_answer: CREDIT_QUICK_ANSWER,
            backtest_read: CREDIT_BACKTEST_READ,
            strategy_generation: CREDIT_STRATEGY_GENERATION,
            watcher_run: CREDIT_WATCHER_RUN,
            workspace_turn: CREDIT_WORKSPACE_TURN,
            review: CREDIT_REVIEW,
            author_turn: CREDIT_AUTHOR_TURN,
        },
    })
}

/// `GET /billing/subscription`
pub async fn entitlements(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Entitlements>, ApiError> {
    let billing = service(&state)?;
    Ok(Json(billing.entitlements(user.user_id).await?))
}

/// `GET /billing/usage`
pub async fn usage(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<UsageSummary>, ApiError> {
    let billing = service(&state)?;
    let entitlements = billing.entitlements(user.user_id).await?;
    Ok(Json(entitlements.usage))
}

/// `POST /billing/subscribe`
pub async fn subscribe(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<SubscribeRequest>,
) -> Result<Json<Entitlements>, ApiError> {
    let billing = service(&state)?;
    let plan =
        PlanId::parse(&body.plan).ok_or_else(|| BillingError::UnknownPlan(body.plan.clone()))?;
    let cycle = match body.cycle.as_deref() {
        None => BillingCycle::Monthly,
        Some(value) => BillingCycle::parse(value)
            .ok_or_else(|| BillingError::UnknownCycle(value.to_string()))?,
    };
    let key = body
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    Ok(Json(
        billing.subscribe(user.user_id, plan, cycle, key).await?,
    ))
}

/// `POST /billing/cancel`
pub async fn cancel(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Entitlements>, ApiError> {
    let billing = service(&state)?;
    Ok(Json(billing.cancel(user.user_id).await?))
}

/// `POST /billing/resume`
pub async fn resume(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<Entitlements>, ApiError> {
    let billing = service(&state)?;
    Ok(Json(billing.resume(user.user_id).await?))
}

/// `POST /billing/webhook`
///
/// Public by necessity: a payment provider cannot hold a session token. The
/// HMAC signature is the authentication, checked by the service before the body
/// is parsed.
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WebhookOutcome>, ApiError> {
    let billing = service(&state)?;
    Ok(Json(billing.handle_webhook(&headers, &body).await?))
}

fn service(state: &AppState) -> Result<&Arc<BillingService>, ApiError> {
    state.billing.as_ref().ok_or_else(|| {
        ApiError::coded(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "BILLING_UNAVAILABLE",
            "billing is not configured in this deployment (the database is unset)",
        )
    })
}
