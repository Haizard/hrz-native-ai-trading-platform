//! Persistence for live trading (`docs/11`, `docs/15`).
//!
//! ## Two jobs, and the second one is a safety property
//!
//! 1. **Opt-in state.** Which venues a user has enabled for real orders, and
//!    the history of how that changed.
//! 2. **The order book of record.** Every order the platform has sent, keyed by
//!    the client id it generated. [`record_live_order`] inserts with
//!    `ON CONFLICT DO NOTHING` and reports whether it was new, which makes the
//!    database the second line of defence behind
//!    [`trading_engine::OrderGateway`](https://docs.rs/trading-engine): even if
//!    two tasks raced on one decision, only one placement is recorded.
//!
//! ## Terminal statuses are a set, and it is spelled out
//!
//! `status` mirrors the venue's vocabulary. What counts as *finished* is
//! written down once, in [`TERMINAL_STATUSES`], because "is this order still
//! live?" is asked by reconciliation, by the UI and by the alert rules, and
//! three copies of that list is three chances to disagree.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::DbError;
use crate::repositories::{dt_to_ns, ns_to_dt};

/// Order statuses that mean the order will not trade again.
pub const TERMINAL_STATUSES: [&str; 4] = ["FILLED", "CANCELED", "REJECTED", "EXPIRED"];

/// One `live_orders` row.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveOrderRow {
    /// The id we generated; also the exchange's idempotency key.
    pub client_order_id: String,
    /// The bot that placed it.
    pub bot_id: Uuid,
    /// Which venue.
    pub venue: String,
    /// Symbol.
    pub symbol: String,
    /// `BUY` or `SELL`.
    pub side: String,
    /// The venue's order-type word.
    pub order_type: String,
    /// Base quantity.
    pub quantity: f64,
    /// Venue status word.
    pub status: String,
    /// The venue's id, once it answered.
    pub exchange_order_id: Option<String>,
    /// Filled quantity.
    pub filled_qty: f64,
    /// Average fill price.
    pub avg_price: Option<f64>,
    /// When it was sent, unix nanos.
    pub placed_at: i64,
}

/// Record an opt-in or a revoke.
///
/// # Errors
/// Returns [`DbError::Pool`] if the insert fails.
pub async fn record_venue_opt_in(
    pool: &PgPool,
    user_id: Uuid,
    venue: &str,
    action: &str,
    reason: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO venue_opt_ins (user_id, venue, action, reason) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(venue.to_ascii_lowercase())
    .bind(action)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// The venues a user currently has enabled.
///
/// The newest row per venue decides: an opt-in followed by a revoke means the
/// venue is off, which is why this reads the latest action rather than counting
/// opt-ins.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn opted_in_venues(pool: &PgPool, user_id: Uuid) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (venue) venue, action FROM venue_opt_ins \
         WHERE user_id = $1 ORDER BY venue, ts DESC, id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter(|row| row.get::<String, _>("action") == "opt_in")
        .map(|row| row.get::<String, _>("venue"))
        .collect())
}

/// Record an order, reporting whether it was new.
///
/// Returns `false` when the id already exists, which is the case a retry
/// produces. The caller is expected to treat `false` as "already sent" rather
/// than as a failure: the exchange holds one order either way.
///
/// # Errors
/// Returns [`DbError::Pool`] if the insert fails.
pub async fn record_live_order(pool: &PgPool, order: &LiveOrderRow) -> Result<bool, DbError> {
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO live_orders (client_order_id, bot_id, venue, symbol, side, order_type, \
         quantity, status, exchange_order_id, filled_qty, avg_price, placed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (client_order_id) DO NOTHING \
         RETURNING client_order_id",
    )
    .bind(order.client_order_id.as_str())
    .bind(order.bot_id)
    .bind(order.venue.as_str())
    .bind(order.symbol.as_str())
    .bind(order.side.as_str())
    .bind(order.order_type.as_str())
    .bind(order.quantity)
    .bind(order.status.as_str())
    .bind(order.exchange_order_id.as_deref())
    .bind(order.filled_qty)
    .bind(order.avg_price)
    .bind(ns_to_dt(order.placed_at))
    .fetch_optional(pool)
    .await?;

    Ok(inserted.is_some())
}

/// Update an order from a venue report.
///
/// # Errors
/// Returns [`DbError::Pool`] if the update fails.
pub async fn update_live_order(
    pool: &PgPool,
    client_order_id: &str,
    status: &str,
    filled_qty: f64,
    avg_price: Option<f64>,
    exchange_order_id: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE live_orders SET status = $2, filled_qty = $3, avg_price = $4, \
         exchange_order_id = COALESCE($5, exchange_order_id), updated_at = now() \
         WHERE client_order_id = $1",
    )
    .bind(client_order_id)
    .bind(status)
    .bind(filled_qty)
    .bind(avg_price)
    .bind(exchange_order_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Orders we still believe are live for a bot.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn open_live_orders(pool: &PgPool, bot_id: Uuid) -> Result<Vec<LiveOrderRow>, DbError> {
    let rows = sqlx::query(
        "SELECT client_order_id, bot_id, venue, symbol, side, order_type, quantity, status, \
         exchange_order_id, filled_qty, avg_price, placed_at \
         FROM live_orders WHERE bot_id = $1 AND status <> ALL($2) ORDER BY placed_at",
    )
    .bind(bot_id)
    .bind(TERMINAL_STATUSES.map(str::to_string).to_vec())
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_order).collect())
}

/// Recent orders for a bot, newest first.
///
/// # Errors
/// Returns [`DbError::Pool`] if the query fails.
pub async fn list_live_orders(
    pool: &PgPool,
    bot_id: Uuid,
    limit: i64,
) -> Result<Vec<LiveOrderRow>, DbError> {
    let rows = sqlx::query(
        "SELECT client_order_id, bot_id, venue, symbol, side, order_type, quantity, status, \
         exchange_order_id, filled_qty, avg_price, placed_at \
         FROM live_orders WHERE bot_id = $1 ORDER BY placed_at DESC LIMIT $2",
    )
    .bind(bot_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_order).collect())
}

/// Map a row to the struct.
fn row_to_order(row: sqlx::postgres::PgRow) -> LiveOrderRow {
    LiveOrderRow {
        client_order_id: row.get("client_order_id"),
        bot_id: row.get("bot_id"),
        venue: row.get("venue"),
        symbol: row.get("symbol"),
        side: row.get("side"),
        order_type: row.get("order_type"),
        quantity: row.get("quantity"),
        status: row.get("status"),
        exchange_order_id: row.get("exchange_order_id"),
        filled_qty: row.get("filled_qty"),
        avg_price: row.get("avg_price"),
        placed_at: dt_to_ns(row.get("placed_at")),
    }
}
