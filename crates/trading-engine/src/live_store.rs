//! Persisting a live bot's decisions, trades and orders (`docs/11`, `docs/13`).
//!
//! ## Why this is a sibling of `store.rs` rather than a branch inside it
//!
//! [`BotSession`](crate::store::BotSession) drains a `PaperBot`: it calls
//! `take_decisions`, `take_alerts`, `cumulative_r`, and every one of those
//! methods has a live counterpart with a different shape -- a live decision
//! carries an *outcome* (entered, denied, refused, protection lost) where a
//! paper one carries a signal, and a live run has orders to record that a
//! paper run has never heard of. Folding both into one type would mean a
//! method per bot kind and a match on every line; two types that share the
//! audit vocabulary they write is the smaller surface.
//!
//! ## The order book is written from *our* side
//!
//! `live_orders` records what the platform believes it sent, not what the
//! exchange reports. That is not a detail: reconciliation exists to compare
//! our view against theirs, and a table filled from the exchange's answer
//! would make every comparison trivially clean. The venue's answer updates the
//! row (status, fill), it does not create it.
//!
//! ## Flushing is diffed, not repeated
//!
//! [`OrderGateway`](crate::execution::OrderGateway) keeps every order for the
//! life of the bot, so a flush that wrote all of them every time would be N
//! statements per tick against a database that costs about a second per
//! statement. The session keeps what it has already written and only touches
//! rows that changed.

use std::collections::HashMap;

use db::Database;
use db::paper::{AuditEvent, ExecutedTrade};
use serde_json::json;
use uuid::Uuid;

use crate::error::ExecutionError;
use crate::execution::{ExchangeAdapter, OrderAck, OrderSide};
use crate::live::{LiveBot, LiveOutcome, LiveRecord, LiveTrade};

/// The audit event type a live decision is written under.
///
/// Distinct from `bot.decision` on purpose. The two share a table, and a
/// reader asking "what did this bot consider?" should not have to guess
/// whether the rows it got came from a simulator or a venue.
pub const LIVE_DECISION_EVENT: &str = "bot.live_decision";

/// The audit event type an order placement is written under.
pub const LIVE_ORDER_EVENT: &str = "bot.live_order";

/// The audit event type a reconciliation result is written under.
pub const RECONCILE_EVENT: &str = "bot.reconcile";

/// A running live bot's row in `bots`, plus how much of its history is written.
#[derive(Debug)]
pub struct LiveSession {
    database: Database,
    user_id: Uuid,
    bot_id: Uuid,
    /// How many of `bot.trades()` are already in `trades_executed`.
    ///
    /// An index is safe here, unlike for orders: `trades()` only ever grows by
    /// appending.
    trades_written: usize,
    /// Orders already in `live_orders`, with the acknowledgement we wrote, so
    /// a flush can skip the ones that have not changed.
    orders_written: HashMap<String, OrderAck>,
}

impl LiveSession {
    /// Attach to a bot row that already exists.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Storage`] if the start event cannot be written.
    pub async fn attach(
        database: &Database,
        user_id: Uuid,
        bot_id: Uuid,
        context: serde_json::Value,
    ) -> Result<Self, ExecutionError> {
        crate::store::write_started(database, user_id, bot_id, context).await?;
        Ok(Self {
            database: database.clone(),
            user_id,
            bot_id,
            trades_written: 0,
            orders_written: HashMap::new(),
        })
    }

    /// The `bots.id` this session writes to.
    #[must_use]
    pub const fn bot_id(&self) -> Uuid {
        self.bot_id
    }

    /// Write everything the bot has produced since the last flush.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Storage`] if any write fails. Nothing is
    /// marked written unless it succeeded, so a failed flush is retried rather
    /// than silently dropped -- which matters more here than in paper mode,
    /// because an order that is not recorded is an order nobody will
    /// reconcile.
    pub async fn flush<A: ExchangeAdapter>(
        &mut self,
        bot: &mut LiveBot<A>,
    ) -> Result<(), ExecutionError> {
        let records = bot.take_records();
        let trades = bot.trades().to_vec();
        let venue = bot.venue().to_string();
        let symbol = bot.symbol().to_string();

        let mut events: Vec<AuditEvent> = Vec::new();
        let at = records.last().map_or_else(now_ns, |record| record.at);

        for record in &records {
            events.push(AuditEvent {
                user_id: Some(self.user_id),
                event_type: LIVE_DECISION_EVENT.into(),
                payload: live_decision_payload(record, self.bot_id),
                ts: record.at,
            });
        }

        // Orders first, so a `trades_executed` row can never reference a
        // placement that is not on record.
        for (request, ack) in bot.orders() {
            if self.orders_written.get(&request.client_order_id) == Some(ack) {
                continue;
            }

            let row = db::live::LiveOrderRow {
                client_order_id: request.client_order_id.clone(),
                bot_id: self.bot_id,
                venue: venue.clone(),
                symbol: request.symbol.clone(),
                side: request.side.as_str().to_string(),
                order_type: request.order_type.as_str().to_string(),
                quantity: request.quantity,
                status: ack.status.as_str().to_string(),
                exchange_order_id: Some(ack.exchange_order_id.clone()),
                filled_qty: ack.filled_qty,
                avg_price: ack.avg_price,
                placed_at: at,
            };

            // Insert-or-update rather than insert-then-update: the second form
            // costs a statement every time, and on this database a statement
            // is a second.
            let is_new = db::live::record_live_order(self.database.pool(), &row).await?;
            if !is_new {
                db::live::update_live_order(
                    self.database.pool(),
                    &request.client_order_id,
                    ack.status.as_str(),
                    ack.filled_qty,
                    ack.avg_price,
                    Some(ack.exchange_order_id.as_str()),
                )
                .await?;
            }

            events.push(AuditEvent {
                user_id: Some(self.user_id),
                event_type: LIVE_ORDER_EVENT.into(),
                payload: json!({
                    "bot_id": self.bot_id,
                    "client_order_id": request.client_order_id,
                    "venue": venue,
                    "symbol": request.symbol,
                    "side": request.side.as_str(),
                    "order_type": request.order_type.as_str(),
                    "quantity": request.quantity,
                    "status": ack.status.as_str(),
                    "filled_qty": ack.filled_qty,
                    "avg_price": ack.avg_price,
                    "exchange_order_id": ack.exchange_order_id,
                }),
                ts: at,
            });

            self.orders_written
                .insert(request.client_order_id.clone(), ack.clone());
        }

        let new_trades: Vec<ExecutedTrade> = trades[self.trades_written.min(trades.len())..]
            .iter()
            .map(|trade| executed_trade(trade, self.bot_id, &symbol, &venue))
            .collect();

        if !events.is_empty() {
            db::paper::insert_audit_events(self.database.pool(), &events).await?;
        }
        if !new_trades.is_empty() {
            db::paper::insert_executed_trades(self.database.pool(), &new_trades).await?;
        }

        self.trades_written = trades.len();
        Ok(())
    }

    /// Move the bot to a terminal status and write whatever is left.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Storage`] if the final flush or status update
    /// fails.
    pub async fn finish<A: ExchangeAdapter>(
        &mut self,
        bot: &mut LiveBot<A>,
    ) -> Result<(), ExecutionError> {
        self.flush(bot).await?;
        let status = if bot.is_halted() { "killed" } else { "stopped" };
        db::paper::set_bot_status(self.database.pool(), self.bot_id, status).await?;

        db::paper::insert_audit_events(
            self.database.pool(),
            &[AuditEvent {
                user_id: Some(self.user_id),
                event_type: crate::store::STOPPED_EVENT.into(),
                payload: json!({
                    "bot_id": self.bot_id,
                    "status": status,
                    "mode": "live",
                    "venue": bot.venue(),
                    "trades": bot.trades().len(),
                    "cumulative_r": bot.cumulative_r(),
                    "halt_reason": bot.halt_reason(),
                }),
                ts: now_ns(),
            }],
        )
        .await?;
        Ok(())
    }
}

/// The audit payload for one live decision.
///
/// `outcome` is a stable machine word and everything else is a field, so a UI
/// can colour a decision without parsing prose -- the same rule the paper
/// decision payload follows.
#[must_use]
pub fn live_decision_payload(record: &LiveRecord, bot_id: Uuid) -> serde_json::Value {
    let (kind, detail) = match &record.outcome {
        LiveOutcome::NoContext => ("no_context", json!({})),
        LiveOutcome::NoSignal => ("no_signal", json!({})),
        LiveOutcome::Entered {
            reasons,
            quantity,
            price,
        } => (
            "entered",
            json!({ "reasons": reasons, "quantity": quantity, "price": price }),
        ),
        LiveOutcome::EntryDenied {
            reasons,
            limit,
            value,
        } => (
            "entry_denied",
            json!({ "reasons": reasons, "limit": limit, "value": value }),
        ),
        LiveOutcome::EntryRefused { reason } => ("entry_refused", json!({ "reason": reason })),
        LiveOutcome::Closed {
            trigger,
            r_multiple,
        } => (
            "closed",
            json!({ "trigger": trigger, "r_multiple": r_multiple }),
        ),
        LiveOutcome::Exited { trigger } => ("exited", json!({ "trigger": trigger })),
        LiveOutcome::ProtectionLost { detail } => ("protection_lost", json!({ "detail": detail })),
        LiveOutcome::Halted { reason } => ("halted", json!({ "reason": reason })),
    };

    json!({
        "bot_id": bot_id,
        "at": record.at,
        "symbol": record.symbol,
        "price": record.price,
        "in_position": record.in_position,
        "outcome": kind,
        "detail": detail,
    })
}

/// A completed live trade as an `trades_executed` row.
///
/// `conditions_fired` carries the venue and the trigger because the live half
/// of a run has no `regime` and no entry reasons: the strategy's *reasons*
/// live on the entry decision's audit row, and duplicating them here would be
/// two places to keep in step.
fn executed_trade(trade: &LiveTrade, bot_id: Uuid, symbol: &str, venue: &str) -> ExecutedTrade {
    ExecutedTrade {
        bot_id,
        symbol: symbol.to_string(),
        side: match trade.side {
            OrderSide::Buy => "long".into(),
            OrderSide::Sell => "short".into(),
        },
        entry_price: trade.entry_price,
        stop_price: Some(trade.stop_price),
        target_price: trade.target_price,
        exit_price: Some(trade.exit_price),
        r_multiple: Some(trade.r_multiple),
        opened_at: trade.opened_at,
        closed_at: Some(trade.closed_at),
        conditions_fired: json!({
            "mode": "live",
            "venue": venue,
            "trigger": trade.trigger,
            "quantity": trade.quantity,
        }),
    }
}

/// Now, in unix nanoseconds.
fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i64)
}
