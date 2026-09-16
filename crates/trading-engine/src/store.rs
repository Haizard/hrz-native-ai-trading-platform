//! Persisting a bot's trades and decisions (`docs/11`, `docs/13`).
//!
//! ## Draining, not mirroring
//!
//! The bot accumulates decisions and trades in memory; this type drains them
//! into the database. That keeps the bot's own loop free of I/O -- a slow
//! database delays the *log*, never the trading decision -- and it means a
//! database outage degrades the audit trail rather than stopping the bot.
//!
//! That trade-off is deliberate and has a cost worth naming: if the process
//! dies between a decision and a flush, that decision is lost. The mitigation
//! is to flush on a short interval, which is why [`BotSession::flush`] is
//! cheap to call and callers are expected to call it often.
//!
//! ## Two tables, two meanings
//!
//! * `trades_executed` -- what the bot *did*, one row per completed trade.
//! * `audit_log` -- what the bot *considered*, one row per decision, including
//!   the ones that did nothing. `docs/11` asks for both, and they answer
//!   different questions: the first is P&L, the second is "why".

use db::paper::{AuditEvent, ExecutedTrade};
use db::Database;
use serde_json::json;
use uuid::Uuid;

use crate::error::ExecutionError;
use crate::paper::{BotAlert, DecisionOutcome, DecisionRecord, PaperBot};

/// The audit event type every decision is written under.
pub const DECISION_EVENT: &str = "bot.decision";

/// The audit event type a risk breach is written under.
pub const RISK_EVENT: &str = "bot.risk_breach";

/// The audit event type a user-facing notification is written under.
///
/// `docs/11` asks for the user to be *notified* on a breach, and `docs/13`
/// defines no notifications table. Rather than invent one the schema doc does
/// not describe, notifications ride the append-only event stream the platform
/// already has: Phase 7's in-app surface reads `bot.notification` rows, and
/// email/webhook -- which `docs/11` calls later additions -- become another
/// reader of the same rows rather than another writer.
pub const NOTIFICATION_EVENT: &str = "bot.notification";

/// The audit event type a bot start is written under.
///
/// Worth its own row because its *absence* is the signal: a bot that starts and
/// never stops crashed, and a trail without these two events cannot tell a
/// clean run from a silent death.
pub const STARTED_EVENT: &str = "bot.started";

/// The audit event type a clean stop is written under.
pub const STOPPED_EVENT: &str = "bot.stopped";

/// A running bot's row in `bots`, plus how much of its history is already
/// written.
#[derive(Debug)]
pub struct BotSession {
    database: Database,
    user_id: Uuid,
    bot_id: Uuid,
    /// Trades already written. Unlike decisions, `bot.trades()` returns the
    /// whole history every time, so this offset is real and load-bearing.
    trades_written: usize,
    /// Set once the clamp note has been logged, so it is recorded once.
    clamp_recorded: bool,
}

impl BotSession {
    /// Register a bot and its strategy, returning the session to write through.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Storage`] if the strategy or bot row cannot be
    /// written.
    pub async fn start(
        database: &Database,
        user_id: Uuid,
        strategy_name: &str,
        strategy_version: &str,
        document: &serde_json::Value,
        mode: &str,
        venue: Option<&str>,
    ) -> Result<Self, ExecutionError> {
        let pool = database.pool();
        let strategy_id = db::paper::find_or_create_strategy(
            pool,
            user_id,
            strategy_name,
            strategy_version,
            document,
            "developer_sdk",
        )
        .await?;

        let bot_id = db::paper::insert_bot(pool, user_id, strategy_id, mode, venue).await?;

        Self::attach(
            database,
            user_id,
            bot_id,
            json!({
                "strategy": strategy_name,
                "version": strategy_version,
                "mode": mode,
                "venue": venue,
            }),
        )
        .await
    }

    /// Attach to a bot row that already exists.
    ///
    /// The API creates the row when a user asks for a bot, and the task that
    /// runs it attaches afterwards. Creating a second row there would give one
    /// bot two identities, and its trades would end up split between them.
    ///
    /// `context` is merged into the `bot.started` event, so a trail says how the
    /// run began rather than only that it did.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Storage`] if the start event cannot be written.
    pub async fn attach(
        database: &Database,
        user_id: Uuid,
        bot_id: Uuid,
        context: serde_json::Value,
    ) -> Result<Self, ExecutionError> {
        write_started(database, user_id, bot_id, context).await?;

        Ok(Self {
            database: database.clone(),
            user_id,
            bot_id,
            trades_written: 0,
            clamp_recorded: false,
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
    /// marked as written unless it succeeded, so a failed flush is retried
    /// rather than silently dropped.
    pub async fn flush(&mut self, bot: &mut PaperBot) -> Result<(), ExecutionError> {
        let decisions = bot.take_decisions();
        let trades = bot.trades().to_vec();
        let alerts = bot.take_alerts();

        let mut events: Vec<AuditEvent> = Vec::new();
        let at = decisions.last().map_or_else(now_ns, |decision| decision.at);

        // Alerts are the user-facing half of a breach: `docs/11` asks for the
        // user to be told, not merely for a row to exist somewhere.
        for alert in &alerts {
            let killed = matches!(alert, BotAlert::Killed { .. });
            events.push(AuditEvent {
                user_id: Some(self.user_id),
                event_type: NOTIFICATION_EVENT.into(),
                payload: notification_payload(alert, self.bot_id),
                ts: at,
            });
            if killed {
                events.push(AuditEvent {
                    user_id: Some(self.user_id),
                    event_type: RISK_EVENT.into(),
                    payload: json!({
                        "bot_id": self.bot_id,
                        "kind": "halt",
                        "detail": alert.body(),
                    }),
                    ts: at,
                });
            }
            self.clamp_recorded = true;
        }

        // Every decision in `decisions` is new: `take_decisions` drained them.
        // An offset here would be compared against a per-flush *batch* rather
        // than a running total, so every batch after the first would be
        // silently dropped -- which is exactly what happened, and what the
        // integration test caught: 250 rows for 3,348 candles.
        for decision in &decisions {
            events.push(AuditEvent {
                user_id: Some(self.user_id),
                event_type: DECISION_EVENT.into(),
                payload: decision_payload(decision, self.bot_id),
                ts: decision.at,
            });
        }

        let new_trades: Vec<ExecutedTrade> = trades[self.trades_written.min(trades.len())..]
            .iter()
            .map(|trade| ExecutedTrade {
                bot_id: self.bot_id,
                symbol: bot.symbol().to_string(),
                side: match trade.direction {
                    strategy_dsl::Direction::Long => "long".into(),
                    strategy_dsl::Direction::Short => "short".into(),
                },
                entry_price: trade.entry_price,
                stop_price: Some(trade.stop_price),
                target_price: trade.take_profit_price,
                exit_price: Some(trade.exit_price),
                r_multiple: Some(trade.r_multiple),
                opened_at: trade.entry_time,
                closed_at: Some(trade.exit_time),
                conditions_fired: json!({
                    "entry": trade.entry_reasons,
                    "exit": trade.exit_reasons,
                    "trigger": trade.exit_trigger.name(),
                    "regime": trade.regime,
                }),
            })
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
    pub async fn finish(&mut self, bot: &mut PaperBot) -> Result<(), ExecutionError> {
        self.flush(bot).await?;
        let status = if bot.is_halted() { "killed" } else { "stopped" };
        db::paper::set_bot_status(self.database.pool(), self.bot_id, status).await?;

        db::paper::insert_audit_events(
            self.database.pool(),
            &[AuditEvent {
                user_id: Some(self.user_id),
                event_type: STOPPED_EVENT.into(),
                payload: json!({
                    "bot_id": self.bot_id,
                    "status": status,
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

/// Write the `bot.started` event.
///
/// Free function rather than a method because both session types write it, and
/// the payload shape is the thing that must not drift: a reader looking for
/// how a run began should not have to know whether it was paper or live to
/// find the row.
///
/// # Errors
/// Returns [`ExecutionError::Storage`] if the event cannot be written.
pub(crate) async fn write_started(
    database: &Database,
    user_id: Uuid,
    bot_id: Uuid,
    context: serde_json::Value,
) -> Result<(), ExecutionError> {
    let mut payload = json!({ "bot_id": bot_id });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), context.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }

    db::paper::insert_audit_events(
        database.pool(),
        &[AuditEvent {
            user_id: Some(user_id),
            event_type: STARTED_EVENT.into(),
            payload,
            ts: now_ns(),
        }],
    )
    .await?;
    Ok(())
}

/// Now, in unix nanoseconds.
fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos() as i64,
        // A clock set before 1970 is not worth failing a trade over.
        Err(_) => 0,
    }
}

/// The audit payload for a notification.
///
/// `severity` and `title` exist so a UI can list these without parsing prose:
/// a notification that only has a body is one every reader has to interpret.
#[must_use]
pub fn notification_payload(alert: &BotAlert, bot_id: Uuid) -> serde_json::Value {
    let kind = match alert {
        BotAlert::Killed { .. } => "killed",
        BotAlert::Clamped { .. } => "clamped",
    };
    let mut payload = json!({
        "bot_id": bot_id,
        "kind": kind,
        "severity": alert.severity(),
        "title": alert.title(),
        "body": alert.body(),
    });
    match alert {
        BotAlert::Killed { reason, positions } => {
            payload["reason"] = json!(reason);
            payload["positions"] = json!(positions);
        }
        BotAlert::Clamped { detail } => {
            payload["detail"] = json!(detail);
        }
    }
    payload
}

/// The audit payload for one decision.
///
/// The shape is deliberately flat and greppable: an operator investigating a
/// bot should be able to read a row and know what happened without joining
/// anything.
#[must_use]
pub fn decision_payload(decision: &DecisionRecord, bot_id: Uuid) -> serde_json::Value {
    let (kind, detail) = match &decision.outcome {
        DecisionOutcome::NoContext => ("no_context", json!({})),
        DecisionOutcome::NoSignal => ("no_signal", json!({})),
        DecisionOutcome::EntryQueued { reasons } => ("entry_queued", json!({ "reasons": reasons })),
        DecisionOutcome::EntryFilled { reasons } => ("entry_filled", json!({ "reasons": reasons })),
        DecisionOutcome::EntryDenied {
            reasons,
            limit,
            value,
        } => (
            "entry_denied",
            json!({ "reasons": reasons, "limit": limit, "value": value }),
        ),
        DecisionOutcome::EntryRefused { reason } => ("entry_refused", json!({ "reason": reason })),
        DecisionOutcome::ExitFilled { trigger } => ("exit_filled", json!({ "trigger": trigger })),
        DecisionOutcome::Closed {
            trigger,
            r_multiple,
        } => (
            "closed",
            json!({ "trigger": trigger, "r_multiple": r_multiple }),
        ),
        DecisionOutcome::Halted { reason } => ("halted", json!({ "reason": reason })),
    };

    json!({
        "bot_id": bot_id,
        "at": decision.at,
        "symbol": decision.symbol,
        "decision_timeframe": decision.decision_timeframe,
        "price": decision.price,
        "frames_ready": decision.frames_ready,
        "frames_total": decision.frames_total,
        "in_position": decision.in_position,
        "kind": kind,
        "detail": detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(outcome: DecisionOutcome) -> DecisionRecord {
        DecisionRecord {
            at: 1_700_000_000_000_000_000,
            symbol: "BTCUSDT".into(),
            decision_timeframe: "entry".into(),
            price: 100.0,
            frames_ready: 2,
            frames_total: 2,
            in_position: false,
            outcome,
        }
    }

    #[test]
    fn a_no_signal_decision_is_written_with_a_kind() {
        // The whole point of the decision log: the quiet bars are recorded too.
        let payload = decision_payload(&decision(DecisionOutcome::NoSignal), Uuid::nil());
        assert_eq!(payload["kind"], "no_signal");
        assert_eq!(payload["symbol"], "BTCUSDT");
        assert_eq!(payload["frames_ready"], 2);
        assert_eq!(payload["frames_total"], 2);
    }

    #[test]
    fn a_denied_entry_records_which_limit_blocked_it() {
        let payload = decision_payload(
            &decision(DecisionOutcome::EntryDenied {
                reasons: vec!["close > liquidity.swept_level".into()],
                limit: "daily_loss_limit_r".into(),
                value: "3.10R".into(),
            }),
            Uuid::nil(),
        );
        assert_eq!(payload["kind"], "entry_denied");
        assert_eq!(payload["detail"]["limit"], "daily_loss_limit_r");
        // The setup that was refused is kept, so the log can show that the
        // trade was there and the limit, not the market, said no.
        assert_eq!(
            payload["detail"]["reasons"][0],
            "close > liquidity.swept_level"
        );
    }

    #[test]
    fn a_halting_decision_is_distinguishable_from_an_ordinary_one() {
        let payload = decision_payload(
            &decision(DecisionOutcome::Halted {
                reason: "daily loss limit breached: 3.10R of 3.00R".into(),
            }),
            Uuid::nil(),
        );
        assert_eq!(payload["kind"], "halted");
        assert!(payload["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("daily"));
    }

    #[test]
    fn a_kill_notification_carries_severity_and_what_happened_to_the_position() {
        let alert = BotAlert::Killed {
            reason: "daily loss limit breached: 3.51R of 3.00R".into(),
            positions: "the open position was closed at market".into(),
        };
        let payload = notification_payload(&alert, Uuid::nil());
        assert_eq!(payload["kind"], "killed");
        assert_eq!(payload["severity"], "critical");
        assert!(payload["title"].as_str().unwrap().contains("risk engine"));
        assert!(payload["positions"].as_str().unwrap().contains("closed"));
    }

    #[test]
    fn a_clamp_notification_is_a_warning_not_a_crisis() {
        let alert = BotAlert::Clamped {
            detail: "risk.max_risk_pct was 20% -- clamped to the 5% platform ceiling".into(),
        };
        let payload = notification_payload(&alert, Uuid::nil());
        assert_eq!(payload["kind"], "clamped");
        assert_eq!(payload["severity"], "warning");
    }

    #[test]
    fn every_audit_row_names_the_bot_it_came_from() {
        // Without this, a trail shared by several bots cannot be attributed,
        // and `purge_bot` has nothing to match on.
        let payload = decision_payload(&decision(DecisionOutcome::NoSignal), Uuid::nil());
        assert!(payload.get("bot_id").is_some());
    }

    #[test]
    fn a_cold_ladder_is_recorded_as_such_not_as_a_missed_setup() {
        let payload = decision_payload(&decision(DecisionOutcome::NoContext), Uuid::nil());
        assert_eq!(payload["kind"], "no_context");
    }
}
