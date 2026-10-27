//! The subscription lifecycle and entitlement resolution (`docs/52`).
//!
//! ## One place decides "what is this user entitled to"
//!
//! Every route that gates something asks [`BillingService::entitlements`], or
//! one of the small helpers built on it. There is no second interpretation of a
//! plan, and no route reads the `subscriptions` table directly. That is what
//! keeps a trial that quietly expired from still unlocking order flow because
//! one handler forgot to check the date.
//!
//! ## Expiry is resolved lazily, on read
//!
//! There is no scheduler that flips rows at midnight. [`BillingService`]
//! reconciles a row the first time it is read after its period ends: a trial
//! becomes active (or expired), an active plan renews, a canceled one expires.
//! A user who does not open the app costs nothing, and a process that was down
//! at the boundary has no backlog to process -- the next read is the backlog.
//!
//! ## Two clocks, on purpose
//!
//! The *subscription* clock sets entitlement. The *usage* clock sets when
//! credits reset. They share an anchor for a paid user -- the period start --
//! and a free user resets on the calendar month. Keeping them as dates rather
//! than timestamps is what lets `usage_counters` promise one row per period.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use serde::Serialize;
use uuid::Uuid;

use db::{Database, SubscriptionRow, SubscriptionWrite};

use super::plans::{
    plan, BillingCycle, Feature, Limits, PlanId, Resource, UsageMetric, TRIAL_DAYS,
};
use super::provider::{BillingConfig, BillingMode, HmacProvider, WebhookEvent};
use super::BillingError;

/// The status a user's access is in, as the UI renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessStatus {
    /// No paid subscription; the Free plan applies.
    Free,
    /// Inside a no-card trial of a paid plan.
    Trialing,
    /// Paid and current.
    Active,
    /// A renewal failed; still entitled through the period.
    PastDue,
    /// Will not renew; still entitled through the period.
    Canceled,
    /// The period ended without renewal; Free applies.
    Expired,
}

/// The subscription half of an entitlement response.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionView {
    /// The lifecycle state.
    pub status: AccessStatus,
    /// How often the plan is billed.
    pub cycle: BillingCycle,
    /// When the current period began.
    pub period_start: DateTime<Utc>,
    /// When the current period ends.
    pub period_end: DateTime<Utc>,
    /// When a trial ends, when one is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_end: Option<DateTime<Utc>>,
    /// Whether the plan stops at `period_end`.
    pub cancel_at_period_end: bool,
    /// Whether the trial has ever been used.
    pub trial_used: bool,
    /// `internal` or the provider that owns the subscription.
    pub provider: String,
}

/// One usage line: used, cap, and what is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UsageLine {
    /// Consumed this period.
    pub used: i64,
    /// The plan's cap; `-1` means unlimited.
    pub limit: i64,
    /// What is left; `-1` means unlimited.
    pub remaining: i64,
}

/// The usage half of an entitlement response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageSummary {
    /// The first instant of the usage period.
    pub period_start: DateTime<Utc>,
    /// When the counters reset.
    pub resets_at: DateTime<Utc>,
    /// Weighted AI credits.
    pub ai_credits: UsageLine,
    /// Backtests and parameter sweeps.
    pub backtests: UsageLine,
}

/// Everything a client needs to render the current plan and its allowances.
#[derive(Debug, Clone, Serialize)]
pub struct Entitlements {
    /// The effective plan (Free after expiry).
    pub plan: PlanId,
    /// The plan's display name.
    pub plan_name: &'static str,
    /// The numeric allowances.
    pub limits: Limits,
    /// The feature flags.
    pub features: super::plans::Features,
    /// The subscription state.
    pub subscription: SubscriptionView,
    /// This period's usage.
    pub usage: UsageSummary,
    /// Whether the community indicator library exists for this deployment.
    ///
    /// Whether the community library is served by this deployment.
    ///
    /// A [`BillingService`] only exists with a database, and the library is
    /// stored there, so it is always available when entitlements can be read.
    pub community_indicators_available: bool,
}

/// The result of accepting a provider webhook.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookOutcome {
    /// Whether the provider had already delivered this event.
    pub duplicate: bool,
    /// The event kind that was handled.
    pub kind: String,
}

/// The billing service: configuration plus the database it records to.
#[derive(Debug, Clone)]
pub struct BillingService {
    database: Arc<Database>,
    config: BillingConfig,
}

/// The resolved access state, before it is rendered.
///
/// Private because it is an intermediate: the public answer is
/// [`Entitlements`], and letting a route hold this would be a second place that
/// interprets a plan.
#[derive(Debug, Clone)]
struct Access {
    plan: PlanId,
    status: AccessStatus,
    cycle: BillingCycle,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    trial_end: Option<DateTime<Utc>>,
    cancel_at_period_end: bool,
    trial_used: bool,
    provider: String,
}

/// An owned subscription row write.
///
/// `SubscriptionWrite` borrows its strings, which makes it awkward to build
/// across an `.await`; this owned form is the one intermediate every write path
/// uses.
#[derive(Debug, Clone)]
struct SubscriptionChange {
    plan_id: String,
    cycle: String,
    status: String,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    trial_end: Option<DateTime<Utc>>,
    cancel_at_period_end: bool,
    trial_used: bool,
    provider: String,
    provider_ref: Option<String>,
    last_idempotency_key: Option<String>,
}

impl SubscriptionChange {
    fn from_row(row: &SubscriptionRow) -> Self {
        Self {
            plan_id: row.plan_id.clone(),
            cycle: row.cycle.clone(),
            status: row.status.clone(),
            period_start: row.period_start,
            period_end: row.period_end,
            trial_end: row.trial_end,
            cancel_at_period_end: row.cancel_at_period_end,
            trial_used: row.trial_used,
            provider: row.provider.clone(),
            provider_ref: row.provider_ref.clone(),
            last_idempotency_key: row.last_idempotency_key.clone(),
        }
    }

    async fn save(
        &self,
        service: &BillingService,
        user_id: Uuid,
    ) -> Result<SubscriptionRow, BillingError> {
        let write = SubscriptionWrite {
            plan_id: &self.plan_id,
            cycle: &self.cycle,
            status: &self.status,
            period_start: self.period_start,
            period_end: self.period_end,
            trial_end: self.trial_end,
            cancel_at_period_end: self.cancel_at_period_end,
            trial_used: self.trial_used,
            provider: &self.provider,
            provider_ref: self.provider_ref.as_deref(),
            last_idempotency_key: self.last_idempotency_key.as_deref(),
        };
        Ok(db::upsert_subscription(service.database.pool(), user_id, &write).await?)
    }
}

impl BillingService {
    /// Build a service over a database and configuration.
    #[must_use]
    pub fn new(database: Arc<Database>, config: BillingConfig) -> Self {
        Self { database, config }
    }

    /// Build from environment configuration.
    #[must_use]
    pub fn from_env(database: Arc<Database>) -> Self {
        Self::new(database, BillingConfig::from_env())
    }

    /// Which lifecycle is authoritative.
    #[must_use]
    pub const fn mode(&self) -> BillingMode {
        self.config.mode
    }

    /// Resolve the user's effective plan, subscription state and usage.
    ///
    /// # Errors
    /// Returns [`BillingError::Database`] when the store cannot be read.
    pub async fn entitlements(&self, user_id: Uuid) -> Result<Entitlements, BillingError> {
        let now = Utc::now();
        let row = self.reconciled(user_id, now).await?;
        let access = access_for(row.as_ref(), now);
        let limits = plan(access.plan).limits;

        let usage_period = access.period_start.date_naive();
        let rows = db::usage_for_period(self.database.pool(), user_id, usage_period).await?;
        let used = |metric: &str| {
            rows.iter()
                .find(|row| row.metric == metric)
                .map_or(0, |row| row.used)
        };
        let credit_used = used(UsageMetric::AiCredit.as_str());
        let backtest_used = used(UsageMetric::Backtest.as_str());

        Ok(Entitlements {
            plan: access.plan,
            plan_name: plan(access.plan).name,
            limits,
            features: plan(access.plan).features,
            subscription: SubscriptionView {
                status: access.status,
                cycle: access.cycle,
                period_start: access.period_start,
                period_end: access.period_end,
                trial_end: access.trial_end,
                cancel_at_period_end: access.cancel_at_period_end,
                trial_used: access.trial_used,
                provider: access.provider,
            },
            usage: UsageSummary {
                period_start: access.period_start,
                resets_at: access.period_end,
                ai_credits: line(credit_used, limits.max_ai_credits),
                backtests: line(backtest_used, limits.max_backtests),
            },
            community_indicators_available: true,
        })
    }

    /// Consume `amount` of a metered resource, or refuse with the plan's cap.
    ///
    /// The returned entitlements carry the updated usage line, so a caller can
    /// show the new balance without a second request.
    ///
    /// # Errors
    /// [`BillingError::LimitReached`] when the consumption would cross the cap.
    pub async fn consume(
        &self,
        user_id: Uuid,
        metric: UsageMetric,
        amount: i64,
    ) -> Result<Entitlements, BillingError> {
        let mut entitlements = self.entitlements(user_id).await?;
        let limit = metric.limit(&entitlements.limits);
        let period_start = entitlements.usage.period_start.date_naive();

        let consumed = db::consume_usage(
            self.database.pool(),
            user_id,
            period_start,
            metric.as_str(),
            amount,
            limit,
        )
        .await?;

        let Some(used) = consumed else {
            return Err(BillingError::LimitReached {
                resource: metric.as_str().to_string(),
                limit,
                plan: entitlements.plan,
            });
        };

        match metric {
            UsageMetric::AiCredit => {
                entitlements.usage.ai_credits = line(used, limit);
            }
            UsageMetric::Backtest => {
                entitlements.usage.backtests = line(used, limit);
            }
        }
        Ok(entitlements)
    }

    /// Require a boolean feature.
    ///
    /// Live bots are a counted limit, not a flag; gate them with
    /// [`BillingService::require_capacity`] and [`Resource::LiveBots`].
    ///
    /// # Errors
    /// [`BillingError::FeatureRequired`] when the plan does not include it.
    pub async fn require_feature(
        &self,
        user_id: Uuid,
        feature: Feature,
    ) -> Result<Entitlements, BillingError> {
        let entitlements = self.entitlements(user_id).await?;
        if feature.allowed_by(&entitlements.features) {
            Ok(entitlements)
        } else {
            Err(BillingError::FeatureRequired {
                feature,
                plan: entitlements.plan,
            })
        }
    }

    /// Require room for one more of a standing resource.
    ///
    /// `current` is the count the caller already read, so the cap can be
    /// enforced without this service taking a dependency on every table that
    /// owns a resource. A check-then-insert race remains possible for standing
    /// resources -- two tabs can both see one free bot slot -- and is accepted:
    /// the consequence is an extra bot, not an extra charge.
    ///
    /// # Errors
    /// [`BillingError::LimitReached`] when the resource is at its cap.
    pub async fn require_capacity(
        &self,
        user_id: Uuid,
        resource: Resource,
        current: i64,
    ) -> Result<Entitlements, BillingError> {
        let entitlements = self.entitlements(user_id).await?;
        let limit = resource.limit(&entitlements.limits);
        if limit < 0 || current < limit {
            Ok(entitlements)
        } else {
            Err(BillingError::LimitReached {
                resource: resource.code().to_string(),
                limit,
                plan: entitlements.plan,
            })
        }
    }

    /// Start or change a paid subscription.
    ///
    /// In internal mode this grants the plan directly (after a trial, when one
    /// is still available). In provider mode it refuses: the provider's
    /// `subscription.activated` webhook is the only path that may grant access,
    /// and until a checkout client exists, pretending otherwise would give the
    /// user a plan nobody was charged for.
    ///
    /// # Errors
    /// Unknown plan/cycle, provider mode, or a storage failure.
    pub async fn subscribe(
        &self,
        user_id: Uuid,
        plan_id: PlanId,
        cycle: BillingCycle,
        idempotency_key: Option<&str>,
    ) -> Result<Entitlements, BillingError> {
        if !plan_id.is_paid() {
            return Err(BillingError::InvalidRequest(
                "choose a paid plan; cancellation returns you to Free at the end of the period"
                    .to_string(),
            ));
        }
        if self.config.mode == BillingMode::Provider {
            return Err(BillingError::ProviderUnavailable);
        }

        let now = Utc::now();
        let existing = db::get_subscription(self.database.pool(), user_id).await?;

        // A retried request with the same key must not start a second period.
        if let (Some(row), Some(key)) = (existing.as_ref(), idempotency_key) {
            if row.last_idempotency_key.as_deref() == Some(key) {
                return self.entitlements(user_id).await;
            }
        }

        let trial_used = existing.as_ref().is_some_and(|row| row.trial_used);
        let currently_entitled = existing.as_ref().is_some_and(|row| is_live_paid(row, now));
        let change = match existing.as_ref() {
            // A plan change during a trial keeps the trial end date: the user
            // is still evaluating, and restarting the clock would grant a
            // second trial by choosing a different plan.
            Some(row) if row.status == "trialing" && now < row.period_end => SubscriptionChange {
                plan_id: plan_id.as_str().to_string(),
                cycle: cycle.as_str().to_string(),
                status: "trialing".to_string(),
                period_start: row.period_start,
                period_end: row.period_end,
                trial_end: row.trial_end,
                cancel_at_period_end: false,
                trial_used: true,
                provider: "internal".to_string(),
                provider_ref: None,
                last_idempotency_key: idempotency_key.map(str::to_string),
            },
            _ if !trial_used && !currently_entitled => {
                trial_change(plan_id, cycle, now, idempotency_key)
            }
            _ => active_change(plan_id, cycle, now, trial_used, idempotency_key),
        };

        change.save(self, user_id).await?;
        self.entitlements(user_id).await
    }

    /// Stop renewing at the end of the current period.
    ///
    /// The user keeps the plan until `period_end`; that is what they paid for.
    ///
    /// # Errors
    /// [`BillingError::NotSubscribed`] when there is no live paid subscription.
    pub async fn cancel(&self, user_id: Uuid) -> Result<Entitlements, BillingError> {
        let now = Utc::now();
        let Some(row) = db::get_subscription(self.database.pool(), user_id).await? else {
            return Err(BillingError::NotSubscribed);
        };
        if !is_live_paid(&row, now) {
            return Err(BillingError::NotSubscribed);
        }
        if !row.cancel_at_period_end {
            let mut change = SubscriptionChange::from_row(&row);
            change.cancel_at_period_end = true;
            change.save(self, user_id).await?;
        }
        self.entitlements(user_id).await
    }

    /// Undo a scheduled cancellation, while the period is still running.
    ///
    /// # Errors
    /// [`BillingError::NotSubscribed`] when the subscription has already ended.
    pub async fn resume(&self, user_id: Uuid) -> Result<Entitlements, BillingError> {
        let now = Utc::now();
        let Some(row) = db::get_subscription(self.database.pool(), user_id).await? else {
            return Err(BillingError::NotSubscribed);
        };
        if row.status == "expired" || now >= row.period_end {
            return Err(BillingError::NotSubscribed);
        }
        let mut change = SubscriptionChange::from_row(&row);
        change.cancel_at_period_end = false;
        if change.status == "canceled" {
            change.status = "active".to_string();
        }
        change.save(self, user_id).await?;
        self.entitlements(user_id).await
    }

    /// Verify, deduplicate and apply a provider webhook.
    ///
    /// # Errors
    /// Signature failures, malformed events, and storage failures.
    pub async fn handle_webhook(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> Result<WebhookOutcome, BillingError> {
        let secret = self
            .config
            .webhook_secret()
            .cloned()
            .ok_or(BillingError::WebhookUnconfigured)?;
        let provider = HmacProvider::new(secret.as_ref().clone());
        provider.verify(headers, body)?;
        let event = provider.parse(body)?;

        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| BillingError::InvalidRequest(format!("webhook body: {error}")))?;
        let is_new = db::record_billing_event(
            self.database.pool(),
            event.user_id,
            provider.name(),
            &event.event_id,
            &event.kind,
            &payload,
        )
        .await?;
        if !is_new {
            return Ok(WebhookOutcome {
                duplicate: true,
                kind: event.kind,
            });
        }

        self.apply_event(&event, provider.name()).await?;
        Ok(WebhookOutcome {
            duplicate: false,
            kind: event.kind,
        })
    }

    /// Read the subscription row, reconciling an elapsed period.
    async fn reconciled(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<SubscriptionRow>, BillingError> {
        let Some(mut row) = db::get_subscription(self.database.pool(), user_id).await? else {
            return Ok(None);
        };
        if reconcile(&mut row, now) {
            let plan_id = row.plan_id.clone();
            let cycle = row.cycle.clone();
            let status = row.status.clone();
            let provider = row.provider.clone();
            let saved = {
                let write = SubscriptionWrite {
                    plan_id: &plan_id,
                    cycle: &cycle,
                    status: &status,
                    period_start: row.period_start,
                    period_end: row.period_end,
                    trial_end: row.trial_end,
                    cancel_at_period_end: row.cancel_at_period_end,
                    trial_used: row.trial_used,
                    provider: &provider,
                    provider_ref: row.provider_ref.as_deref(),
                    last_idempotency_key: row.last_idempotency_key.as_deref(),
                };
                db::upsert_subscription(self.database.pool(), user_id, &write).await?
            };
            return Ok(Some(saved));
        }
        Ok(Some(row))
    }

    async fn apply_event(&self, event: &WebhookEvent, provider: &str) -> Result<(), BillingError> {
        let now = Utc::now();
        let kind = event.kind.as_str();

        if kind == "subscription.activated" || kind == "subscription.renewed" {
            let user_id = event.user_id.ok_or_else(|| {
                BillingError::InvalidRequest(format!("`{kind}` requires `user_id`"))
            })?;
            let existing = db::get_subscription(self.database.pool(), user_id).await?;
            let plan_id = event
                .plan_id
                .as_deref()
                .and_then(PlanId::parse)
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|row| PlanId::parse(&row.plan_id))
                })
                .ok_or_else(|| {
                    BillingError::InvalidRequest(format!("`{kind}` requires a known `plan_id`"))
                })?;
            if !plan_id.is_paid() {
                return Err(BillingError::InvalidRequest(
                    "a paid plan is required for an activation event".to_string(),
                ));
            }
            let cycle = event
                .cycle
                .as_deref()
                .and_then(BillingCycle::parse)
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|row| BillingCycle::parse(&row.cycle))
                })
                .unwrap_or(BillingCycle::Monthly);
            let period_end = event.period_end.unwrap_or_else(|| add_cycle(now, cycle));
            if period_end <= now {
                return Err(BillingError::InvalidRequest(
                    "`period_end` must be in the future".to_string(),
                ));
            }
            let change = SubscriptionChange {
                plan_id: plan_id.as_str().to_string(),
                cycle: cycle.as_str().to_string(),
                status: "active".to_string(),
                period_start: now,
                period_end,
                trial_end: None,
                cancel_at_period_end: false,
                trial_used: true,
                provider: provider.to_string(),
                provider_ref: event.provider_ref.clone(),
                last_idempotency_key: existing
                    .as_ref()
                    .and_then(|row| row.last_idempotency_key.clone()),
            };
            change.save(self, user_id).await?;
            return Ok(());
        }

        let Some(user_id) = event.user_id else {
            tracing::warn!(
                target: "billing",
                kind,
                "webhook event has no user_id; recorded but not applied"
            );
            return Ok(());
        };
        let Some(row) = db::get_subscription(self.database.pool(), user_id).await? else {
            tracing::warn!(
                target: "billing",
                kind,
                %user_id,
                "webhook event has no subscription; recorded but not applied"
            );
            return Ok(());
        };

        match kind {
            "subscription.canceled" => {
                let mut change = SubscriptionChange::from_row(&row);
                change.status = "canceled".to_string();
                change.cancel_at_period_end = true;
                change.provider = provider.to_string();
                change.save(self, user_id).await?;
            }
            "subscription.expired" => {
                let mut change = SubscriptionChange::from_row(&row);
                change.status = "expired".to_string();
                change.cancel_at_period_end = true;
                change.provider = provider.to_string();
                change.save(self, user_id).await?;
            }
            "payment.failed" => {
                let mut change = SubscriptionChange::from_row(&row);
                change.status = "past_due".to_string();
                change.provider = provider.to_string();
                change.save(self, user_id).await?;
            }
            other => {
                tracing::warn!(
                    target: "billing",
                    kind = other,
                    "unknown webhook event recorded but not applied"
                );
            }
        }
        Ok(())
    }
}

fn trial_change(
    plan_id: PlanId,
    cycle: BillingCycle,
    now: DateTime<Utc>,
    idempotency_key: Option<&str>,
) -> SubscriptionChange {
    let trial_end = now + Duration::days(TRIAL_DAYS);
    SubscriptionChange {
        plan_id: plan_id.as_str().to_string(),
        cycle: cycle.as_str().to_string(),
        status: "trialing".to_string(),
        period_start: now,
        period_end: trial_end,
        trial_end: Some(trial_end),
        cancel_at_period_end: false,
        trial_used: true,
        provider: "internal".to_string(),
        provider_ref: None,
        last_idempotency_key: idempotency_key.map(str::to_string),
    }
}

fn active_change(
    plan_id: PlanId,
    cycle: BillingCycle,
    now: DateTime<Utc>,
    trial_used: bool,
    idempotency_key: Option<&str>,
) -> SubscriptionChange {
    SubscriptionChange {
        plan_id: plan_id.as_str().to_string(),
        cycle: cycle.as_str().to_string(),
        status: "active".to_string(),
        period_start: now,
        period_end: add_cycle(now, cycle),
        trial_end: None,
        cancel_at_period_end: false,
        trial_used,
        provider: "internal".to_string(),
        provider_ref: None,
        last_idempotency_key: idempotency_key.map(str::to_string),
    }
}

fn line(used: i64, limit: i64) -> UsageLine {
    UsageLine {
        used,
        limit,
        remaining: if limit < 0 { -1 } else { (limit - used).max(0) },
    }
}

fn is_live_paid(row: &SubscriptionRow, now: DateTime<Utc>) -> bool {
    PlanId::parse(&row.plan_id).is_some_and(|plan| plan.is_paid())
        && row.status != "expired"
        && now < row.period_end
}

/// Resolve the effective access from a row and the current instant.
fn access_for(row: Option<&SubscriptionRow>, now: DateTime<Utc>) -> Access {
    let (month_start, month_end) = month_bounds(now);
    let free = Access {
        plan: PlanId::Free,
        status: AccessStatus::Free,
        cycle: BillingCycle::Monthly,
        period_start: month_start,
        period_end: month_end,
        trial_end: None,
        cancel_at_period_end: false,
        trial_used: row.is_some_and(|row| row.trial_used),
        provider: row.map_or_else(|| "internal".to_string(), |row| row.provider.clone()),
    };

    let Some(row) = row else {
        return free;
    };
    let Some(plan_id) = PlanId::parse(&row.plan_id) else {
        tracing::warn!(
            target: "billing",
            plan = %row.plan_id,
            "subscription names an unknown plan; treating the user as Free"
        );
        return free;
    };
    if !plan_id.is_paid() || row.status == "expired" {
        return Access {
            status: if row.status == "expired" {
                AccessStatus::Expired
            } else {
                AccessStatus::Free
            },
            ..free
        };
    }
    let status = match row.status.as_str() {
        "trialing" => AccessStatus::Trialing,
        "active" => AccessStatus::Active,
        "past_due" => AccessStatus::PastDue,
        "canceled" => AccessStatus::Canceled,
        "expired" => AccessStatus::Expired,
        _ => AccessStatus::Active,
    };
    Access {
        plan: plan_id,
        status,
        cycle: BillingCycle::parse(&row.cycle).unwrap_or(BillingCycle::Monthly),
        period_start: row.period_start,
        period_end: row.period_end,
        trial_end: row.trial_end,
        cancel_at_period_end: row.cancel_at_period_end,
        trial_used: row.trial_used,
        provider: row.provider.clone(),
    }
}

/// Advance or expire a row whose period has elapsed. Returns whether it changed.
fn reconcile(row: &mut SubscriptionRow, now: DateTime<Utc>) -> bool {
    let Some(plan_id) = PlanId::parse(&row.plan_id) else {
        return false;
    };
    if !plan_id.is_paid() {
        return false;
    }
    let cycle = BillingCycle::parse(&row.cycle).unwrap_or(BillingCycle::Monthly);

    match row.status.as_str() {
        "trialing" => {
            let trial_end = row.trial_end.unwrap_or(row.period_end);
            if now < trial_end {
                return false;
            }
            if row.cancel_at_period_end {
                row.status = "expired".to_string();
            } else {
                row.status = "active".to_string();
                row.trial_end = None;
                row.period_start = row.period_end;
                row.period_end = add_cycle(row.period_end, cycle);
            }
            true
        }
        "active" => {
            if now < row.period_end {
                return false;
            }
            if row.cancel_at_period_end {
                row.status = "expired".to_string();
            } else {
                row.period_start = row.period_end;
                row.period_end = add_cycle(row.period_end, cycle);
            }
            true
        }
        "past_due" | "canceled" => {
            if now < row.period_end {
                return false;
            }
            row.status = "expired".to_string();
            true
        }
        _ => false,
    }
}

fn add_cycle(base: DateTime<Utc>, cycle: BillingCycle) -> DateTime<Utc> {
    let months = match cycle {
        BillingCycle::Monthly => 1,
        BillingCycle::Annual => 12,
    };
    base.checked_add_months(Months::new(months))
        .unwrap_or_else(|| base + Duration::days(if months == 1 { 30 } else { 365 }))
}

fn month_bounds(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or(now);
    let end = start
        .checked_add_months(Months::new(1))
        .unwrap_or(start + Duration::days(31));
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, period_end: DateTime<Utc>) -> SubscriptionRow {
        SubscriptionRow {
            user_id: Uuid::nil(),
            plan_id: "pro".to_string(),
            cycle: "monthly".to_string(),
            status: status.to_string(),
            period_start: period_end - Duration::days(30),
            period_end,
            trial_end: None,
            cancel_at_period_end: false,
            trial_used: true,
            provider: "internal".to_string(),
            provider_ref: None,
            last_idempotency_key: None,
            created_at: period_end - Duration::days(30),
            updated_at: period_end - Duration::days(30),
        }
    }

    #[test]
    fn active_plan_renews_when_the_period_ends() {
        let now = Utc::now();
        let mut row = row("active", now - Duration::hours(1));
        let old_end = row.period_end;
        assert!(reconcile(&mut row, now));
        assert_eq!(row.status, "active");
        assert_eq!(row.period_start, old_end);
        assert!(row.period_end > old_end);
    }

    #[test]
    fn canceled_plan_expires_when_the_period_ends() {
        let now = Utc::now();
        let mut row = row("active", now - Duration::hours(1));
        row.cancel_at_period_end = true;
        assert!(reconcile(&mut row, now));
        assert_eq!(row.status, "expired");
    }

    #[test]
    fn trial_converts_to_active() {
        let now = Utc::now();
        let mut row = row("trialing", now - Duration::hours(1));
        row.trial_end = Some(row.period_end);
        assert!(reconcile(&mut row, now));
        assert_eq!(row.status, "active");
        assert_eq!(row.trial_end, None);
    }

    #[test]
    fn active_plan_within_the_period_is_untouched() {
        let now = Utc::now();
        let mut row = row("active", now + Duration::days(3));
        let before = row.clone();
        assert!(!reconcile(&mut row, now));
        assert_eq!(row, before);
    }

    #[test]
    fn expired_plan_falls_back_to_free_but_keeps_the_status() {
        let now = Utc::now();
        let row = row("expired", now - Duration::days(1));
        let access = access_for(Some(&row), now);
        assert_eq!(access.plan, PlanId::Free);
        assert_eq!(access.status, AccessStatus::Expired);
    }

    #[test]
    fn paid_plan_before_period_end_is_entitled() {
        let now = Utc::now();
        let row = row("canceled", now + Duration::days(3));
        let access = access_for(Some(&row), now);
        assert_eq!(access.plan, PlanId::Pro);
        assert_eq!(access.status, AccessStatus::Canceled);
    }

    #[test]
    fn free_usage_period_is_the_calendar_month() {
        let now = Utc::now();
        let access = access_for(None, now);
        assert_eq!(access.period_start.day(), 1);
        assert!(access.period_end > access.period_start);
    }

    #[test]
    fn unlimited_remaining_is_negative_one() {
        assert_eq!(line(10, -1).remaining, -1);
        assert_eq!(line(10, 25).remaining, 15);
        assert_eq!(line(30, 25).remaining, 0);
    }
}
