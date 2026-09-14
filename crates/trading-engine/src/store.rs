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
use crate::paper::{DecisionOutcome, DecisionRecord, PaperBot};

/// The audit event type every decision is written under.
pub const DECISION_EVENT: &str = "bot.decision";

/// The audit event type a risk breach is written under.
pub const RISK_EVENT: &str = "bot.risk_breach";

/// A running bot's row in `bots`, plus how much of its history is already
/// written.
#[derive(Debug)]
pub struct BotSession {
    database: Database,
    user_id: Uuid,
    bot_id: Uuid,
    decisions_written: usize,
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

        Ok(Self {
            database: database.clone(),
            user_id,
            bot_id,
            decisions_written: 0,
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

        let mut events: Vec<AuditEvent> = Vec::new();

        if !self.clamp_recorded {
            if let Some(note) = bot.clamp_note() {
                events.push(AuditEvent {
                    user_id: Some(self.user_id),
                    event_type: RISK_EVENT.into(),
                    payload: json!({ "kind": "clamped", "detail": note }),
                    ts: decisions.first().map_or(0, |d| d.at),
                });
                self.clamp_recorded = true;
            }
        }

        for decision in &decisions[self.decisions_written.min(decisions.len())..] {
            events.push(AuditEvent {
                user_id: Some(self.user_id),
                event_type: DECISION_EVENT.into(),
                payload: decision_payload(decision),
                ts: decision.at,
            });
        }

        // A halt is a risk event, not just a decision: it needs its own
        // greppable type so an alert can fire on it.
        if let Some(reason) = bot.halt_reason() {
            let already = events
                .iter()
                .any(|e| e.event_type == RISK_EVENT && e.payload["kind"] == "halt");
            if !already {
                events.push(AuditEvent {
                    user_id: Some(self.user_id),
                    event_type: RISK_EVENT.into(),
                    payload: json!({ "kind": "halt", "detail": reason }),
                    ts: decisions.last().map_or(0, |d| d.at),
                });
            }
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

        self.decisions_written = decisions.len();
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
        Ok(())
    }
}

/// The audit payload for one decision.
///
/// The shape is deliberately flat and greppable: an operator investigating a
/// bot should be able to read a row and know what happened without joining
/// anything.
#[must_use]
pub fn decision_payload(decision: &DecisionRecord) -> serde_json::Value {
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
        let payload = decision_payload(&decision(DecisionOutcome::NoSignal));
        assert_eq!(payload["kind"], "no_signal");
        assert_eq!(payload["symbol"], "BTCUSDT");
        assert_eq!(payload["frames_ready"], 2);
        assert_eq!(payload["frames_total"], 2);
    }

    #[test]
    fn a_denied_entry_records_which_limit_blocked_it() {
        let payload = decision_payload(&decision(DecisionOutcome::EntryDenied {
            reasons: vec!["close > liquidity.swept_level".into()],
            limit: "daily_loss_limit_r".into(),
            value: "3.10R".into(),
        }));
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
        let payload = decision_payload(&decision(DecisionOutcome::Halted {
            reason: "daily loss limit breached: 3.10R of 3.00R".into(),
        }));
        assert_eq!(payload["kind"], "halted");
        assert!(payload["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("daily"));
    }

    #[test]
    fn a_cold_ladder_is_recorded_as_such_not_as_a_missed_setup() {
        let payload = decision_payload(&decision(DecisionOutcome::NoContext));
        assert_eq!(payload["kind"], "no_context");
    }
}
