//! Indicator workspace alert delivery worker.
//!
//! ## What this does
//!
//! When a running bot fires a decision event (e.g., `EntryQueued` with
//! `long_setup` reasons), this worker checks whether any workspace has an alert
//! preference for that event and, if so, delivers the notification to the
//! configured webhook.
//!
//! ## Deduplication
//!
//! A strategy can fire the same event on consecutive candles. Without
//! deduplication, every candle would deliver a notification, and a webhook that
//! accepts them all would get a flood of identical messages. The worker tracks
//! the last delivery time per (workspace, revision, event) triple and suppresses
//! duplicates within a configurable cooldown window.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::bots::BotEvent;

/// Minimum time between deliveries for the same (workspace, revision, event).
const DELIVERY_COOLDOWN: Duration = Duration::from_secs(3600);

/// A deduplication key: workspace + revision + event name.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct AlertKey {
    workspace_id: uuid::Uuid,
    revision_id: uuid::Uuid,
    event_name: String,
}

/// The state the worker needs to deliver alerts.
pub struct AlertDeliveryState {
    /// The database, for reading alert preferences.
    pub db: Arc<db::Database>,
    /// Where to POST alert payloads.
    pub webhook_url: Option<String>,
    /// HTTP client, shared across deliveries.
    pub http: reqwest::Client,
}

/// Spawn the indicator alert delivery worker.
pub fn spawn_indicator_alert_delivery(
    state: AlertDeliveryState,
    mut events: broadcast::Receiver<BotEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cooldowns: HashMap<AlertKey, std::time::Instant> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.tick().await;

        loop {
            tokio::select! {
                Ok(event) = events.recv() => {
                    if let BotEvent::Decision { bot_id, record } = &event {
                        handle_decision(&state, &mut cooldowns, *bot_id, record).await;
                    }
                }
                _ = ticker.tick() => {
                    let now = std::time::Instant::now();
                    cooldowns.retain(|_, last| now.duration_since(*last) < DELIVERY_COOLDOWN);
                }
            }
        }
    })
}

/// Map a decision outcome to the event names it represents.
///
/// The first reason in `EntryQueued` / `EntryFilled` / `EntryDenied` is the
/// strategy's declared signal name (e.g., `long_setup`).
fn event_names_for_outcome(outcome: &trading_engine::DecisionOutcome) -> Vec<String> {
    use trading_engine::DecisionOutcome;
    match outcome {
        DecisionOutcome::EntryQueued { reasons }
        | DecisionOutcome::EntryFilled { reasons }
        | DecisionOutcome::EntryDenied { reasons, .. } => {
            reasons.first().cloned().into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// Process one bot decision and deliver matching alerts.
async fn handle_decision(
    state: &AlertDeliveryState,
    cooldowns: &mut HashMap<AlertKey, std::time::Instant>,
    bot_id: uuid::Uuid,
    record: &trading_engine::DecisionRecord,
) {
    let names = event_names_for_outcome(&record.outcome);
    if names.is_empty() {
        return;
    }

    let pool = state.db.pool();
    let rows = match db::list_enabled_indicator_alert_preferences(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "could not read indicator alert preferences");
            return;
        }
    };

    if rows.is_empty() {
        return;
    }

    for event_name in &names {
        for row in &rows {
            if &row.event_name != event_name {
                continue;
            }

            let key = AlertKey {
                workspace_id: row.workspace_id,
                revision_id: row.revision_id,
                event_name: row.event_name.clone(),
            };

            if let Some(last) = cooldowns.get(&key) {
                if last.elapsed() < DELIVERY_COOLDOWN {
                    debug!(
                        workspace = %key.workspace_id,
                        event = %key.event_name,
                        "indicator alert suppressed by deduplication cooldown"
                    );
                    continue;
                }
            }

            deliver_alert(state, &key, bot_id, record).await;
            cooldowns.insert(key, std::time::Instant::now());
        }
    }
}

/// Deliver an alert to the configured webhook.
async fn deliver_alert(
    state: &AlertDeliveryState,
    key: &AlertKey,
    bot_id: uuid::Uuid,
    record: &trading_engine::DecisionRecord,
) {
    let Some(webhook_url) = &state.webhook_url else {
        info!(
            workspace = %key.workspace_id,
            event = %key.event_name,
            bot = %bot_id,
            "indicator alert fired but no webhook is configured"
        );
        return;
    };

    let payload = serde_json::json!({
        "type": "indicator_alert",
        "workspace_id": key.workspace_id,
        "revision_id": key.revision_id,
        "bot_id": bot_id,
        "event": key.event_name,
        "symbol": record.symbol,
        "price": record.price,
        "at_ns": record.at,
    });

    match state.http.post(webhook_url).json(&payload).send().await {
        Ok(response) if response.status().is_success() => {
            info!(
                workspace = %key.workspace_id,
                event = %key.event_name,
                bot = %bot_id,
                "indicator alert delivered"
            );
        }
        Ok(response) => {
            warn!(
                status = %response.status(),
                workspace = %key.workspace_id,
                event = %key.event_name,
                "indicator alert webhook rejected the delivery"
            );
        }
        Err(error) => {
            warn!(
                error = %error,
                workspace = %key.workspace_id,
                event = %key.event_name,
                "indicator alert delivery failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trading_engine::DecisionOutcome;

    #[test]
    fn alert_key_is_deduplicated_correctly() {
        let mut map: HashMap<AlertKey, std::time::Instant> = HashMap::new();
        let key = AlertKey {
            workspace_id: uuid::Uuid::new_v4(),
            revision_id: uuid::Uuid::new_v4(),
            event_name: "long_setup".into(),
        };

        map.insert(key.clone(), std::time::Instant::now());
        assert!(map.contains_key(&key));

        let other = AlertKey {
            event_name: "short_setup".into(),
            ..key.clone()
        };
        assert!(!map.contains_key(&other));
    }

    #[test]
    fn entry_queued_maps_to_first_reason() {
        let outcome = DecisionOutcome::EntryQueued {
            reasons: vec!["long_setup".into(), "delta > 0".into()],
        };
        let names = event_names_for_outcome(&outcome);
        assert_eq!(names, vec!["long_setup"]);
    }

    #[test]
    fn no_signal_produces_no_events() {
        let names = event_names_for_outcome(&DecisionOutcome::NoSignal);
        assert!(names.is_empty());
    }
}
