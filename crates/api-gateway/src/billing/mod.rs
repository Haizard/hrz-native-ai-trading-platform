//! Subscriptions, entitlements and metering (`docs/52`).
//!
//! ## What this module is
//!
//! The plan matrix ([`plans`]), the lifecycle and its lazy reconciliation
//! ([`service`]), the provider seam ([`provider`]), and the HTTP surface
//! ([`routes`]). Together they answer one question -- **what may this user do
//! right now** -- and make every route ask it rather than guessing.
//!
//! ## Community indicators are open to every plan
//!
//! The library is not a paid gate: browsing, installing and publishing are
//! available on Free, the way a public script library is. The plan's
//! `community_indicators` level only decides catalog placement -- Desk authors
//! sort first in `featured`. Abuse is bounded by platform constants in the
//! community routes, not by plan limits (`docs/53`).
//!
//! ## Errors are 402, not 403
//!
//! "You have not paid for this" is a distinct outcome from "this is not yours".
//! A 403 would tell a client to give up; a 402 with a machine-readable code and
//! the required plan tells it exactly which upgrade resolves the refusal. The
//! frontend keys the upgrade prompt off that status.

pub mod plans;
pub mod provider;
pub mod routes;
pub mod service;

pub use plans::{
    catalog, plan, BillingCycle, CommunityIndicators, Feature, Features, Limits, Plan, PlanId,
    Pricing, Resource, UsageMetric, CREDIT_AUTHOR_TURN, CREDIT_BACKTEST_READ,
    CREDIT_CHART_ANALYSIS, CREDIT_QUICK_ANSWER, CREDIT_REVIEW, CREDIT_STRATEGY_GENERATION,
    CREDIT_WATCHER_RUN, CREDIT_WORKSPACE_TURN, TRIAL_DAYS,
};
pub use provider::{BillingConfig, BillingMode};
pub use routes::{cancel, entitlements, plans as plans_route, resume, subscribe, usage, webhook};
pub use service::{
    AccessStatus, BillingService, Entitlements, SubscriptionView, UsageLine, UsageSummary,
    WebhookOutcome,
};

use axum::http::StatusCode;
use thiserror::Error;
use uuid::Uuid;

use crate::error::ApiError;
use crate::AppState;

/// A billing failure, mapped to the API's error envelope by [`ApiError`].
#[derive(Debug, Error)]
pub enum BillingError {
    /// The database could not be read or written.
    #[error("billing storage failed: {0}")]
    Database(#[from] db::DbError),

    /// Billing is not configured in this deployment (no database).
    #[error("billing is not configured")]
    Unavailable,

    /// The deployment expects a payment provider's checkout, and none is wired.
    #[error("a payment provider is required for this action, and none is configured")]
    ProviderUnavailable,

    /// The webhook signature did not match.
    #[error("the webhook signature is missing or invalid")]
    InvalidSignature,

    /// The deployment has no webhook secret.
    #[error("BILLING_WEBHOOK_SECRET is not set")]
    WebhookUnconfigured,

    /// The named plan does not exist.
    #[error("`{0}` is not a plan")]
    UnknownPlan(String),

    /// The named cycle does not exist.
    #[error("`{0}` is not a billing cycle; choose monthly or annual")]
    UnknownCycle(String),

    /// The request was structurally wrong.
    #[error("{0}")]
    InvalidRequest(String),

    /// There is no live subscription to change.
    #[error("there is no active subscription to change")]
    NotSubscribed,

    /// A standing resource is at the plan's cap.
    #[error("the {resource} limit for the {plan} plan is {limit}")]
    LimitReached {
        /// The resource's stable name.
        resource: String,
        /// The cap.
        limit: i64,
        /// The effective plan.
        plan: PlanId,
    },

    /// A boolean feature is not in the plan.
    #[error("the {feature:?} feature is not included in the {plan} plan")]
    FeatureRequired {
        /// The feature.
        feature: Feature,
        /// The effective plan.
        plan: PlanId,
    },
}

impl From<BillingError> for ApiError {
    fn from(error: BillingError) -> Self {
        match error {
            BillingError::Database(error) => {
                tracing::error!(target: "billing", error = %error, "billing storage failed");
                ApiError::internal("the billing store could not be reached; try again shortly")
            }
            BillingError::Unavailable => ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "BILLING_UNAVAILABLE",
                "billing is not configured in this deployment (the database is unset)",
            ),
            BillingError::ProviderUnavailable => ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "PAYMENT_PROVIDER_REQUIRED",
                "this deployment activates subscriptions through its payment provider; \
                 complete checkout with the provider to continue",
            ),
            BillingError::InvalidSignature => ApiError::coded(
                StatusCode::UNAUTHORIZED,
                "INVALID_SIGNATURE",
                "the webhook signature is missing or invalid",
            ),
            BillingError::WebhookUnconfigured => ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "BILLING_WEBHOOK_UNCONFIGURED",
                "BILLING_WEBHOOK_SECRET is not set; webhooks cannot be verified",
            ),
            BillingError::UnknownPlan(value) => ApiError::coded(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_PLAN",
                format!("`{value}` is not a plan. Choose one of: free, core, pro, elite, desk."),
            ),
            BillingError::UnknownCycle(value) => ApiError::coded(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_CYCLE",
                format!("`{value}` is not a billing cycle; choose monthly or annual."),
            ),
            BillingError::InvalidRequest(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "BILLING_REQUEST_INVALID", message)
            }
            BillingError::NotSubscribed => ApiError::coded(
                StatusCode::CONFLICT,
                "SUBSCRIPTION_REQUIRED",
                "there is no active subscription to change; subscribe first",
            ),
            BillingError::LimitReached {
                resource,
                limit,
                plan,
            } => {
                let code = match resource.as_str() {
                    "ai_credit" => "AI_CREDITS_EXHAUSTED",
                    "backtest" => "BACKTEST_QUOTA_EXCEEDED",
                    _ => "PLAN_LIMIT_REACHED",
                };
                ApiError::coded(
                    StatusCode::PAYMENT_REQUIRED,
                    code,
                    format!(
                        "the {plan} plan is limited to {limit} {resource}; upgrade to continue"
                    ),
                )
                .with_details(serde_json::json!({
                    "resource": resource,
                    "limit": limit,
                    "plan": plan.as_str(),
                }))
            }
            BillingError::FeatureRequired { feature, plan } => ApiError::coded(
                StatusCode::PAYMENT_REQUIRED,
                "PLAN_FEATURE_REQUIRED",
                format!(
                    "{} requires the {} plan; you are on {}",
                    feature.code(),
                    feature.minimum_plan(),
                    plan
                ),
            )
            .with_details(serde_json::json!({
                "feature": feature.code(),
                "plan": plan.as_str(),
                "required_plan": feature.minimum_plan().as_str(),
            })),
        }
    }
}

/// Entitlements for a user, or `None` when this deployment has no billing.
///
/// The `None` arm is the "up but degraded" rule: without a database there is
/// nothing to meter against, and a route falls back to Free behavior rather
/// than refusing to work at all.
///
/// # Errors
/// Propagates a storage failure as a 500.
pub async fn entitlements_optional(
    state: &AppState,
    user_id: Option<Uuid>,
) -> Result<Option<Entitlements>, ApiError> {
    let (Some(billing), Some(user_id)) = (state.billing.as_ref(), user_id) else {
        return Ok(None);
    };
    Ok(Some(billing.entitlements(user_id).await?))
}

/// Consume metered usage before doing the paid work.
///
/// # Errors
/// 402 with `AI_CREDITS_EXHAUSTED` / `BACKTEST_QUOTA_EXCEEDED` when the plan's
/// allowance is spent.
pub async fn enforce_usage(
    state: &AppState,
    user_id: Uuid,
    metric: UsageMetric,
    amount: i64,
) -> Result<Option<Entitlements>, ApiError> {
    let Some(billing) = state.billing.as_ref() else {
        return Ok(None);
    };
    Ok(Some(billing.consume(user_id, metric, amount).await?))
}

/// Require a boolean feature for an authenticated user.
///
/// # Errors
/// 402 with `PLAN_FEATURE_REQUIRED` when the plan does not carry it.
pub async fn enforce_feature(
    state: &AppState,
    user_id: Uuid,
    feature: Feature,
) -> Result<Option<Entitlements>, ApiError> {
    let Some(billing) = state.billing.as_ref() else {
        return Ok(None);
    };
    Ok(Some(billing.require_feature(user_id, feature).await?))
}

/// Require a boolean feature on a route that also serves anonymous callers.
///
/// An anonymous caller is on Free, so a paid feature is refused with the
/// upgrade detail rather than a login redirect: the chart still works, and the
/// 402 tells the client which plan would unlock the panel.
///
/// # Errors
/// 402 when the caller is anonymous or their plan lacks the feature.
pub async fn enforce_feature_optional(
    state: &AppState,
    user_id: Option<Uuid>,
    feature: Feature,
) -> Result<Option<Entitlements>, ApiError> {
    match user_id {
        Some(user_id) => enforce_feature(state, user_id, feature).await,
        None if state.billing.is_some() => Err(BillingError::FeatureRequired {
            feature,
            plan: PlanId::Free,
        }
        .into()),
        None => Ok(None),
    }
}

/// Require room for one more of a standing resource.
///
/// `current` is the caller's count, read from the table that owns the resource.
///
/// # Errors
/// 402 with `PLAN_LIMIT_REACHED` when the resource is at the plan's cap.
pub async fn enforce_capacity(
    state: &AppState,
    user_id: Uuid,
    resource: Resource,
    current: i64,
) -> Result<Option<Entitlements>, ApiError> {
    let Some(billing) = state.billing.as_ref() else {
        return Ok(None);
    };
    Ok(Some(
        billing.require_capacity(user_id, resource, current).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limit_errors_are_402_with_a_specific_code() {
        let error: ApiError = BillingError::LimitReached {
            resource: "ai_credit".to_string(),
            limit: 25,
            plan: PlanId::Free,
        }
        .into();
        assert_eq!(error.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(error.code(), "AI_CREDITS_EXHAUSTED");
        assert_eq!(
            error.details().and_then(|d| d.get("plan")),
            Some(&serde_json::json!("free"))
        );
    }

    #[test]
    fn feature_errors_name_the_upgrade() {
        let error: ApiError = BillingError::FeatureRequired {
            feature: Feature::AdvancedOrderflow,
            plan: PlanId::Core,
        }
        .into();
        assert_eq!(error.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(error.code(), "PLAN_FEATURE_REQUIRED");
        assert_eq!(
            error.details().and_then(|d| d.get("required_plan")),
            Some(&serde_json::json!("pro"))
        );
    }
}
