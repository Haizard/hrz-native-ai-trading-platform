//! Persistence for the bot trading engine (`docs/11-BOT-TRADING-ENGINE.md`,
//! `docs/13-DATABASE-SCHEMA.md`).
//!
//! ## Why the row types live here
//!
//! `docs/03` forbids `trading-engine` from depending on `backtester`, and the
//! same instinct applies in the other direction: the database layer should not
//! know about `PaperBot` or `DecisionOutcome`. So this module takes plain row
//! structs, and the caller maps its own types into them. The alternative --
//! `db` depending on `trading-engine` -- would make the schema follow the bot
//! instead of the bot following the schema.
//!
//! ## Appends, not updates
//!
//! `audit_log` is append-only by design (`docs/15`): nothing here updates or
//! deletes a row. A trade that was later corrected is a *new* event, not an
//! edit, because an audit trail you can rewrite is not an audit trail.

use serde_json::Value;
use sqlx::{PgPool, QueryBuilder, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// Rows per multi-row INSERT, matching `repositories.rs`.
const INSERT_CHUNK: usize = 500;

/// One completed simulated or real trade, as `trades_executed` stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedTrade {
    /// The bot that produced it.
    pub bot_id: Uuid,
    /// Symbol traded.
    pub symbol: String,
    /// `long` or `short`.
    pub side: String,
    /// Fill price.
    pub entry_price: f64,
    /// Resolved stop.
    pub stop_price: Option<f64>,
    /// Resolved target.
    pub target_price: Option<f64>,
    /// Exit price, when closed.
    pub exit_price: Option<f64>,
    /// Result in R.
    pub r_multiple: Option<f64>,
    /// Entry time, unix nanos.
    pub opened_at: i64,
    /// Exit time, unix nanos.
    pub closed_at: Option<i64>,
    /// The conditions that fired, and what closed it.
    pub conditions_fired: Value,
}

/// One `audit_log` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    /// Whose bot it was, when it belongs to someone.
    pub user_id: Option<Uuid>,
    /// A stable, greppable event name, e.g. `bot.decision`.
    pub event_type: String,
    /// The event's detail. **Credentials must never reach this field.**
    pub payload: Value,
    /// When it happened, unix nanos.
    pub ts: i64,
}

/// Find or create the `strategies` row for a document.
///
/// The table has no unique key on `(user_id, name, version)`, so this selects
/// first rather than relying on `ON CONFLICT`. Strategies are versioned and
/// never mutated in place (`docs/13`), which is why the lookup includes the
/// version: the same name at a new version is a new row on purpose.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_or_create_strategy(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    version: &str,
    document: &Value,
    created_by: &str,
) -> Result<Uuid, DbError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM strategies \
         WHERE user_id = $1 AND name = $2 AND version = $3 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .fetch_optional(pool)
    .await?;

    if let Some(id) = existing {
        return Ok(id);
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO strategies (user_id, name, version, document, created_by) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(version)
    .bind(document)
    .bind(created_by)
    .fetch_one(pool)
    .await?;

    Ok(id)
}

/// Record a new bot.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_bot(
    pool: &PgPool,
    user_id: Uuid,
    strategy_id: Uuid,
    mode: &str,
    venue: Option<&str>,
) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO bots (user_id, strategy_id, mode, status, venue) \
         VALUES ($1, $2, $3, 'running', $4) RETURNING id",
    )
    .bind(user_id)
    .bind(strategy_id)
    .bind(mode)
    .bind(venue)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Move a bot to a new status: `running`, `paused`, `stopped` or `killed`.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn set_bot_status(pool: &PgPool, bot_id: Uuid, status: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE bots SET status = $2 WHERE id = $1")
        .bind(bot_id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

/// Persist executed trades.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_executed_trades(
    pool: &PgPool,
    trades: &[ExecutedTrade],
) -> Result<(), DbError> {
    for chunk in trades.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO trades_executed (bot_id, symbol, side, entry_price, stop_price, \
             target_price, exit_price, r_multiple, opened_at, closed_at, conditions_fired) ",
        );
        query.push_values(chunk, |mut row, trade| {
            row.push_bind(trade.bot_id)
                .push_bind(trade.symbol.as_str())
                .push_bind(trade.side.as_str())
                .push_bind(trade.entry_price)
                .push_bind(trade.stop_price)
                .push_bind(trade.target_price)
                .push_bind(trade.exit_price)
                .push_bind(trade.r_multiple)
                .push_bind(ns_to_dt(trade.opened_at))
                .push_bind(trade.closed_at.map(ns_to_dt))
                .push_bind(trade.conditions_fired.clone());
        });
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Append audit events.
///
/// # Errors
/// Returns [`DbError::Pool`] if the write fails.
pub async fn insert_audit_events(pool: &PgPool, events: &[AuditEvent]) -> Result<(), DbError> {
    for chunk in events.chunks(INSERT_CHUNK) {
        let mut query = QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO audit_log (user_id, event_type, payload, ts) ",
        );
        query.push_values(chunk, |mut row, event| {
            row.push_bind(event.user_id)
                .push_bind(event.event_type.as_str())
                .push_bind(event.payload.clone())
                .push_bind(ns_to_dt(event.ts));
        });
        query.build().execute(pool).await?;
    }
    Ok(())
}

/// Look up a user by email.
///
/// Deliberately a lookup and not a `find_or_create`: a bot run must attach to
/// a real account, and silently inventing a user row would put trades in the
/// name of somebody who does not exist.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn find_user_by_email(pool: &PgPool, email: &str) -> Result<Option<Uuid>, DbError> {
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}

/// Provision an owner for a paper bot.
///
/// A bot row references `users`, and there is no signup flow yet (auth is
/// Phase 7). Rather than let a runner invent a user implicitly, provisioning is
/// its own explicit call -- `paper-cli` exposes it as `--create-owner` -- so
/// writing a row to `users` is always a deliberate act.
///
/// The password hash is a marker that cannot match any password, because this
/// account is an owner of record and never a way to log in. When Phase 7 lands
/// it should either attach a real credential or move bots to a system owner.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn create_owner(pool: &PgPool, email: &str) -> Result<Uuid, DbError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2)          ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id",
    )
    .bind(email)
    .bind("!no-login")
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Delete a bot and everything it wrote.
///
/// **Not part of the trading loop.** `audit_log` is append-only by design
/// (`docs/15`) and nothing in normal operation removes rows from it. This
/// exists so an integration test can write to a real database and leave it
/// exactly as it found it, and so an operator can remove a bot that was created
/// by mistake. It is deliberately named `purge` rather than `delete` to make
/// that read as the heavy-handed thing it is.
///
/// # Errors
/// Returns [`DbError::Pool`] if the deletes fail.
pub async fn purge_bot(pool: &PgPool, bot_id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM trades_executed WHERE bot_id = $1")
        .bind(bot_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM audit_log WHERE payload->>'bot_id' = $1")
        .bind(bot_id.to_string())
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM bots WHERE id = $1")
        .bind(bot_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A bot's state, for an operator looking at it.
///
/// Assembled from the tables the trading loop already writes, rather than from
/// a separate status row that could drift out of step with them. A status
/// column that disagrees with the trade log is worse than no status at all.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BotSummary {
    /// The bot.
    pub id: Uuid,
    /// `paper` or `live`.
    pub mode: String,
    /// `running`, `paused`, `stopped` or `killed`.
    pub status: String,
    /// Venue, when it trades a real one.
    pub venue: Option<String>,
    /// When the bot row was created, unix nanos.
    pub created_at: i64,
    /// Completed trades.
    pub trades: i64,
    /// Trades still open.
    pub open_trades: i64,
    /// Summed R across completed trades.
    pub cumulative_r: f64,
    /// `on_candle` decisions recorded.
    pub decisions: i64,
    /// The newest decision's close time, unix nanos.
    pub last_decision_at: Option<i64>,
    /// Notifications raised, including any breach.
    pub notifications: i64,
}

/// Read a bot's state, or `None` if there is no such bot.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn bot_summary(pool: &PgPool, bot_id: Uuid) -> Result<Option<BotSummary>, DbError> {
    let row = sqlx::query(
        "SELECT b.id, b.mode, b.status, b.venue, b.created_at, \
                (SELECT count(*) FROM trades_executed t WHERE t.bot_id = b.id) AS trades, \
                (SELECT count(*) FROM trades_executed t \
                  WHERE t.bot_id = b.id AND t.closed_at IS NULL) AS open_trades, \
                (SELECT coalesce(sum(t.r_multiple), 0) FROM trades_executed t \
                  WHERE t.bot_id = b.id) AS cumulative_r, \
                (SELECT count(*) FROM audit_log a \
                  WHERE a.payload->>'bot_id' = b.id::text \
                    AND a.event_type IN ('bot.decision', 'bot.live_decision')) AS decisions, \
                (SELECT max((a.payload->>'at')::bigint) FROM audit_log a \
                  WHERE a.payload->>'bot_id' = b.id::text \
                    AND a.event_type IN ('bot.decision', 'bot.live_decision')) AS last_decision_at, \
                (SELECT count(*) FROM audit_log a \
                  WHERE a.payload->>'bot_id' = b.id::text \
                    AND a.event_type = 'bot.notification') AS notifications \
         FROM bots b WHERE b.id = $1",
    )
    .bind(bot_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else { return Ok(None) };
    Ok(Some(BotSummary {
        id: row.try_get("id")?,
        mode: row.try_get("mode")?,
        status: row.try_get("status")?,
        venue: row.try_get("venue")?,
        created_at: dt_to_ns(row.try_get("created_at")?),
        trades: row.try_get("trades")?,
        open_trades: row.try_get("open_trades")?,
        cumulative_r: row.try_get("cumulative_r")?,
        decisions: row.try_get("decisions")?,
        last_decision_at: row.try_get("last_decision_at")?,
        notifications: row.try_get("notifications")?,
    }))
}

/// A strategy's paper track record, aggregated across every paper bot that ran
/// it.
///
/// The gate in `trading_engine::LiveGate` asks for "how much has this strategy
/// proven in simulation", and that question is about the *strategy*, not about
/// one bot: a user who ran four paper bots over the same document has done
/// four times the proving, and a per-bot count would let a fresh bot reset the
/// clock.
///
/// Only `mode = 'paper'` rows count. A live trade is not evidence that a
/// strategy was ready to go live.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StrategyPaperRecord {
    /// Closed paper trades across every bot running this strategy.
    pub closed_trades: i64,
    /// Summed R across those trades.
    pub cumulative_r: f64,
    /// When the first paper bot for this strategy was created, unix nanos.
    pub first_at: Option<i64>,
    /// When the newest paper trade closed, unix nanos.
    pub last_at: Option<i64>,
}

impl StrategyPaperRecord {
    /// How long the strategy has been in simulation, in hours.
    ///
    /// Measured from the first paper bot's creation to the newest paper trade's
    /// close, and falling back to `now` when nothing has closed yet -- a bot
    /// that has been running for two days and found no setups has still been
    /// *watched* for two days, which is what the requirement is about.
    ///
    /// Never negative: a clock adjustment between the two rows would otherwise
    /// produce a negative duration, and a negative duration silently satisfies
    /// "at least 48 hours".
    #[must_use]
    pub fn hours(&self, now_ns: i64) -> f64 {
        let Some(first) = self.first_at else {
            return 0.0;
        };
        let last = self.last_at.unwrap_or(now_ns);
        let nanos = last.saturating_sub(first).max(0);
        nanos as f64 / 3_600_000_000_000.0
    }
}

/// Read a strategy's paper track record for one user.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn strategy_paper_record(
    pool: &PgPool,
    user_id: Uuid,
    strategy_id: Uuid,
) -> Result<StrategyPaperRecord, DbError> {
    let row = sqlx::query(
        "SELECT \
            (SELECT count(*) FROM trades_executed t JOIN bots b ON b.id = t.bot_id \
              WHERE b.user_id = $1 AND b.strategy_id = $2 AND b.mode = 'paper' \
                AND t.closed_at IS NOT NULL) AS closed_trades, \
            (SELECT coalesce(sum(t.r_multiple), 0) FROM trades_executed t \
              JOIN bots b ON b.id = t.bot_id \
              WHERE b.user_id = $1 AND b.strategy_id = $2 AND b.mode = 'paper') AS cumulative_r, \
            (SELECT min(b.created_at) FROM bots b \
              WHERE b.user_id = $1 AND b.strategy_id = $2 AND b.mode = 'paper') AS first_at, \
            (SELECT max(t.closed_at) FROM trades_executed t JOIN bots b ON b.id = t.bot_id \
              WHERE b.user_id = $1 AND b.strategy_id = $2 AND b.mode = 'paper') AS last_at",
    )
    .bind(user_id)
    .bind(strategy_id)
    .fetch_one(pool)
    .await?;

    let first_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("first_at")?;
    let last_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("last_at")?;

    Ok(StrategyPaperRecord {
        closed_trades: row.try_get("closed_trades")?,
        cumulative_r: row.try_get("cumulative_r")?,
        first_at: first_at.map(dt_to_ns),
        last_at: last_at.map(dt_to_ns),
    })
}

/// A bot's newest decisions, newest first, as the stored payloads.
///
/// Returns the payloads rather than formatted lines: this layer knows what is
/// stored, not how anyone wants to read it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn recent_decisions(
    pool: &PgPool,
    bot_id: Uuid,
    limit: i64,
) -> Result<Vec<Value>, DbError> {
    let rows = sqlx::query(
        "SELECT payload FROM audit_log \
         WHERE payload->>'bot_id' = $1 AND event_type = 'bot.decision' \
         ORDER BY ts DESC LIMIT $2",
    )
    .bind(bot_id.to_string())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| row.try_get::<Value, _>("payload").map_err(DbError::from))
        .collect()
}

/// Delete every test owner whose email starts with `prefix`, and everything
/// they own.
///
/// ## Why this exists
///
/// An integration test that asserts and *then* cleans up leaves its rows behind
/// when an assertion fails, because a panic skips the cleanup. The first run of
/// `persistence.rs` failed twice, and its fixtures accumulated in a shared
/// database until `bot.decision` rows from dead runs outnumbered the live
/// bot's. A test that cannot be re-run cleanly is a test whose failures get
/// worse the more you run it.
///
/// So the tests call this at the *start*, not the end: a stale fixture is
/// removed before it can be counted. It is scoped to a caller-supplied prefix
/// so it cannot reach anything a real user owns, and it is documented as a
/// test tool rather than a feature.
///
/// ## Only owners older than `older_than_minutes`
///
/// The age filter is load-bearing. Cargo runs a file's tests in parallel, so a
/// sweep with no age limit deletes the fixtures of tests that are *currently
/// running* -- which is exactly what happened: the file passed alone and failed
/// in `cargo test --workspace`. A fixture younger than the window belongs to
/// someone who has not finished yet.
///
/// Returns how many owners were removed.
///
/// # Errors
/// Returns [`DbError::Pool`] if a delete fails.
pub async fn purge_owners_with_prefix(
    pool: &PgPool,
    prefix: &str,
    older_than_minutes: i64,
) -> Result<u64, DbError> {
    // Ordered by foreign key: trades and audit rows first, then the rows they
    // point at.
    let pattern = format!("{prefix}%");
    let age = format!("{older_than_minutes} minutes");
    for statement in [
        "DELETE FROM trades_executed WHERE bot_id IN (\
           SELECT b.id FROM bots b JOIN users u ON u.id = b.user_id \
           WHERE u.email LIKE $1 AND u.created_at < now() - $2::interval)",
        "DELETE FROM audit_log WHERE user_id IN (\
           SELECT id FROM users WHERE email LIKE $1 AND created_at < now() - $2::interval)",
        "DELETE FROM bots WHERE user_id IN (\
           SELECT id FROM users WHERE email LIKE $1 AND created_at < now() - $2::interval)",
        "DELETE FROM strategies WHERE user_id IN (\
           SELECT id FROM users WHERE email LIKE $1 AND created_at < now() - $2::interval)",
    ] {
        sqlx::query(statement)
            .bind(&pattern)
            .bind(&age)
            .execute(pool)
            .await?;
    }
    let result =
        sqlx::query("DELETE FROM users WHERE email LIKE $1 AND created_at < now() - $2::interval")
            .bind(&pattern)
            .bind(&age)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// How many trades a bot recorded.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn count_executed_trades(pool: &PgPool, bot_id: Uuid) -> Result<i64, DbError> {
    let row = sqlx::query("SELECT count(*) AS n FROM trades_executed WHERE bot_id = $1")
        .bind(bot_id)
        .fetch_one(pool)
        .await?;
    Ok(row.try_get::<i64, _>("n")?)
}

/// Remove an owner and everything owned by them.
///
/// Same standing as [`purge_bot`]: not part of the trading loop, and the reason
/// it exists is so an integration test can write to a real database and leave
/// it exactly as it found it.
///
/// The order is forced by the foreign keys, and it is not the order you would
/// guess: `bots` references both `users` and `strategies` with no `ON DELETE
/// CASCADE`, so deleting the strategies first fails whenever a bot exists. The
/// version that did exactly that had never been run with a bot present, which
/// is why the live-trading tests are the ones that found it.
///
/// # Errors
/// Returns [`DbError::Pool`] if the deletes fail.
pub async fn purge_owner(pool: &PgPool, user_id: Uuid) -> Result<(), DbError> {
    // `live_orders` cascades from `bots`, so deleting the bots clears it.
    sqlx::query(
        "DELETE FROM trades_executed WHERE bot_id IN (SELECT id FROM bots WHERE user_id = $1)",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM bots WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM audit_log WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    // Phase 8: the venue opt-in history references the user and does not
    // cascade either. Without this, "delete my account" fails for exactly the
    // users who have traded live.
    sqlx::query("DELETE FROM venue_opt_ins WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM strategies WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Count the audit events of one type for one user, newest first.
///
/// Exists so a caller can assert that a decision really was written without
/// reading the whole table.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn count_audit_events(
    pool: &PgPool,
    user_id: Uuid,
    event_type: &str,
) -> Result<i64, DbError> {
    let row =
        sqlx::query("SELECT count(*) AS n FROM audit_log WHERE user_id = $1 AND event_type = $2")
            .bind(user_id)
            .bind(event_type)
            .fetch_one(pool)
            .await?;
    Ok(row.try_get::<i64, _>("n")?)
}
