//! Subscriptions, usage counters and the billing event log (`docs/52`).
//!
//! ## What lives here, and what does not
//!
//! This module is persistence only. The plan matrix -- prices, limits, feature
//! flags -- lives in the gateway's `billing` module, because a deployment must
//! not be able to disagree with the code that enforces it. The gateway decides
//! *what* a plan means; this module stores *who* is on it and *how much* they
//! have used.
//!
//! ## Usage is consumed atomically
//!
//! [`consume_usage`] does the check and the increment in one statement. The
//! obvious alternative -- read the counter, compare, then write -- has a
//! time-of-check-to-time-of-use window, and two concurrent agent calls are
//! exactly the traffic that finds it. A user with 1 credit left must not be
//! able to spend it twice by asking two questions at once.
//!
//! ## Counts are separate from counters
//!
//! Credits and backtests are *period* usage: they reset each billing period and
//! live in `usage_counters`. Bots, watchers and layouts are *standing*
//! resources: they exist until deleted, so their cap is a count of rows, not a
//! counter that resets. Mixing the two would either reset a user's bots every
//! month or leave their credits accumulating forever.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;

/// A user's subscription, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRow {
    /// The subscriber.
    pub user_id: Uuid,
    /// The plan identifier; the gateway owns the vocabulary.
    pub plan_id: String,
    /// `monthly` or `annual`.
    pub cycle: String,
    /// `trialing`, `active`, `past_due`, `canceled` or `expired`.
    pub status: String,
    /// When the current period began.
    pub period_start: DateTime<Utc>,
    /// When the current period ends.
    pub period_end: DateTime<Utc>,
    /// When a trial ends, when the row is trialing.
    pub trial_end: Option<DateTime<Utc>>,
    /// Whether the plan stops at `period_end`.
    pub cancel_at_period_end: bool,
    /// Whether this user has ever used the free trial.
    pub trial_used: bool,
    /// `internal` or a payment provider's name.
    pub provider: String,
    /// The provider's subscription reference, when there is one.
    pub provider_ref: Option<String>,
    /// The idempotency key this row was last written with.
    pub last_idempotency_key: Option<String>,
    /// When the row was created.
    pub created_at: DateTime<Utc>,
    /// When the row last changed.
    pub updated_at: DateTime<Utc>,
}

/// The writable half of a subscription. Timestamps are owned by the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionWrite<'a> {
    /// The plan identifier.
    pub plan_id: &'a str,
    /// `monthly` or `annual`.
    pub cycle: &'a str,
    /// The lifecycle state.
    pub status: &'a str,
    /// Period start.
    pub period_start: DateTime<Utc>,
    /// Period end.
    pub period_end: DateTime<Utc>,
    /// Trial end, when trialing.
    pub trial_end: Option<DateTime<Utc>>,
    /// Whether the plan stops at period end.
    pub cancel_at_period_end: bool,
    /// Whether the trial has been used.
    pub trial_used: bool,
    /// `internal` or a payment provider's name.
    pub provider: &'a str,
    /// The provider's subscription reference.
    pub provider_ref: Option<&'a str>,
    /// The client idempotency key for this write.
    pub last_idempotency_key: Option<&'a str>,
}

/// One metric's usage in one period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    /// The period's first day.
    pub period_start: NaiveDate,
    /// The metric name.
    pub metric: String,
    /// How much has been consumed.
    pub used: i64,
}

fn row_to_subscription(row: &sqlx::postgres::PgRow) -> Result<SubscriptionRow, DbError> {
    Ok(SubscriptionRow {
        user_id: row.try_get("user_id")?,
        plan_id: row.try_get("plan_id")?,
        cycle: row.try_get("cycle")?,
        status: row.try_get("status")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        trial_end: row.try_get("trial_end")?,
        cancel_at_period_end: row.try_get("cancel_at_period_end")?,
        trial_used: row.try_get("trial_used")?,
        provider: row.try_get("provider")?,
        provider_ref: row.try_get("provider_ref")?,
        last_idempotency_key: row.try_get("last_idempotency_key")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Read a user's subscription, when they have one.
pub async fn get_subscription(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<SubscriptionRow>, DbError> {
    let row = sqlx::query("SELECT * FROM subscriptions WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_subscription).transpose()
}

/// Create or replace a user's subscription row.
///
/// `created_at` is preserved on update: it is the first time this user
/// subscribed, not the last time the row changed.
pub async fn upsert_subscription(
    pool: &PgPool,
    user_id: Uuid,
    write: &SubscriptionWrite<'_>,
) -> Result<SubscriptionRow, DbError> {
    let row = sqlx::query(
        r#"
        INSERT INTO subscriptions (
            user_id, plan_id, cycle, status, period_start, period_end, trial_end,
            cancel_at_period_end, trial_used, provider, provider_ref,
            last_idempotency_key
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        ON CONFLICT (user_id) DO UPDATE SET
            plan_id = EXCLUDED.plan_id,
            cycle = EXCLUDED.cycle,
            status = EXCLUDED.status,
            period_start = EXCLUDED.period_start,
            period_end = EXCLUDED.period_end,
            trial_end = EXCLUDED.trial_end,
            cancel_at_period_end = EXCLUDED.cancel_at_period_end,
            trial_used = EXCLUDED.trial_used,
            provider = EXCLUDED.provider,
            provider_ref = EXCLUDED.provider_ref,
            last_idempotency_key = EXCLUDED.last_idempotency_key,
            updated_at = now()
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(write.plan_id)
    .bind(write.cycle)
    .bind(write.status)
    .bind(write.period_start)
    .bind(write.period_end)
    .bind(write.trial_end)
    .bind(write.cancel_at_period_end)
    .bind(write.trial_used)
    .bind(write.provider)
    .bind(write.provider_ref)
    .bind(write.last_idempotency_key)
    .fetch_one(pool)
    .await?;
    row_to_subscription(&row)
}

/// Flip the cancel-at-period-end flag.
///
/// Returns `None` when the user has no subscription row.
pub async fn set_cancel_at_period_end(
    pool: &PgPool,
    user_id: Uuid,
    cancel: bool,
) -> Result<Option<SubscriptionRow>, DbError> {
    let row = sqlx::query(
        "UPDATE subscriptions SET cancel_at_period_end = $2, updated_at = now() \
         WHERE user_id = $1 RETURNING *",
    )
    .bind(user_id)
    .bind(cancel)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_subscription).transpose()
}

/// Record a billing event, returning `true` when it was new.
///
/// A duplicate means the provider retried. The caller must treat it as
/// "already applied" and do nothing -- the unique key is what makes a webhook
/// safe to replay.
pub async fn record_billing_event(
    pool: &PgPool,
    user_id: Option<Uuid>,
    provider: &str,
    event_id: &str,
    kind: &str,
    payload: &serde_json::Value,
) -> Result<bool, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO billing_events (user_id, provider, event_id, kind, payload) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (provider, event_id) DO NOTHING \
         RETURNING id",
    )
    .bind(user_id)
    .bind(provider)
    .bind(event_id)
    .bind(kind)
    .bind(payload)
    .fetch_optional(pool)
    .await?;
    Ok(id.is_some())
}

/// Consume `amount` of `metric` if it fits under `limit`.
///
/// `limit < 0` means unlimited. Returns the new total when the consumption
/// succeeded, and `None` when it would cross the limit. The check and the
/// increment are one statement, so two concurrent calls cannot both see the
/// last unit as available.
pub async fn consume_usage(
    pool: &PgPool,
    user_id: Uuid,
    period_start: NaiveDate,
    metric: &str,
    amount: i64,
    limit: i64,
) -> Result<Option<i64>, DbError> {
    let used: Option<i64> = sqlx::query_scalar(
        r#"
        INSERT INTO usage_counters (user_id, period_start, metric, used)
        SELECT $1, $2, $3, $4
        WHERE $5 < 0 OR $4 <= $5
        ON CONFLICT (user_id, period_start, metric) DO UPDATE
            SET used = usage_counters.used + EXCLUDED.used,
                updated_at = now()
            WHERE $5 < 0 OR usage_counters.used + EXCLUDED.used <= $5
        RETURNING used
        "#,
    )
    .bind(user_id)
    .bind(period_start)
    .bind(metric)
    .bind(amount)
    .bind(limit)
    .fetch_optional(pool)
    .await?;
    Ok(used)
}

/// Every counter this user has for one period.
pub async fn usage_for_period(
    pool: &PgPool,
    user_id: Uuid,
    period_start: NaiveDate,
) -> Result<Vec<UsageRow>, DbError> {
    let rows = sqlx::query(
        "SELECT period_start, metric, used FROM usage_counters \
         WHERE user_id = $1 AND period_start = $2 ORDER BY metric",
    )
    .bind(user_id)
    .bind(period_start)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(UsageRow {
                period_start: row.try_get("period_start")?,
                metric: row.try_get("metric")?,
                used: row.try_get("used")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// Count the user's bots in one mode that are still live (`running`/`paused`).
///
/// A stopped or killed bot is not consuming a slot: the cap is about how many
/// strategies can act at once, not how many rows a user has ever made.
pub async fn count_active_bots(pool: &PgPool, user_id: Uuid, mode: &str) -> Result<i64, DbError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM bots \
         WHERE user_id = $1 AND mode = $2 AND status IN ('running', 'paused')",
    )
    .bind(user_id)
    .bind(mode)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Count the user's saved watch monitors.
pub async fn count_watch_monitors(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM watch_monitors WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// Count the user's enabled watch monitors.
pub async fn count_enabled_watch_monitors(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM watch_monitors WHERE user_id = $1 AND enabled")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    Ok(count)
}

/// Count the user's saved chart sessions, templates included.
pub async fn count_chart_sessions(pool: &PgPool, user_id: Uuid) -> Result<i64, DbError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chart_sessions WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}
