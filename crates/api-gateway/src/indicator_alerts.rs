//! Indicator workspace alert delivery worker.
//!
//! ## What this does
//!
//! Two event sources feed the same preference table:
//!
//! * **Bot decisions** -- a running bot fires `EntryQueued` with
//!   `long_setup` reasons, and a preference for that event name delivers.
//! * **Derived market events** -- the event engine detects a sweep, a
//!   structural break or a fresh gap on a live candle (`fvg_created`,
//!   `liquidity_sweep`, ...), and a preference for that kind delivers even
//!   though no bot is running. This is the source that makes a
//!   `kind: indicator` workspace -- which has no entry logic by design and
//!   can never produce a bot decision -- alertable at all.
//!
//! ## Killzone gating
//!
//! A preference's `channels` JSON may carry `"session": "london"` (or any
//! name the session module knows). A gated alert only delivers while the
//! event's timestamp is inside that session window -- "tell me about fresh
//! London gaps" is a different instruction from "tell me about gaps", and a
//! filter that fired around the clock would not honour it.
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

/// Spawn the indicator alert delivery worker: bot decisions and derived
/// market events into one preference table, one cooldown map.
pub fn spawn_indicator_alert_delivery(
    state: AlertDeliveryState,
    mut events: broadcast::Receiver<BotEvent>,
    market_events: broadcast::Receiver<analytics_core::events::MarketEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cooldowns: HashMap<AlertKey, std::time::Instant> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        let mut market = market_events;
        ticker.tick().await;

        loop {
            tokio::select! {
                Ok(event) = events.recv() => {
                    if let BotEvent::Decision { bot_id, record } = &event {
                        handle_decision(&state, &mut cooldowns, *bot_id, record).await;
                    }
                }
                Ok(market_event) = market.recv() => {
                    handle_market_event(&state, &mut cooldowns, &market_event).await;
                }
                _ = ticker.tick() => {
                    let now = std::time::Instant::now();
                    cooldowns.retain(|_, last| now.duration_since(*last) < DELIVERY_COOLDOWN);
                }
            }
        }
    })
}

/// Which session a preference is gated to, read out of its `channels` JSON.
///
/// `channels` is a free-form object the shell writes (`{"webhook": true}`);
/// an optional `"session"` key narrows delivery to that window. Unknown or
/// misspelled names return `None` -- and `None` means *ungated*, which is why
/// the read is strict about what it accepts rather than best-effort.
fn gated_session(channels: &serde_json::Value) -> Option<analytics_core::SessionKind> {
    let name = channels.get("session")?.as_str()?;
    match name {
        "asia" => Some(analytics_core::SessionKind::Asia),
        "london" => Some(analytics_core::SessionKind::London),
        "new_york" => Some(analytics_core::SessionKind::NewYork),
        _ => None,
    }
}

/// Whether an event timestamp passes the preference's session gate.
fn passes_session_gate(
    channels: &serde_json::Value,
    at_ns: i64,
    windows: &[analytics_core::SessionWindow],
) -> bool {
    match gated_session(channels) {
        // Ungated: every event passes.
        None => true,
        Some(wanted) => matches!(
            analytics_core::session_of(at_ns, windows),
            Some(kind) if kind == wanted
        ),
    }
}

/// Process one derived market event and deliver matching alerts.
async fn handle_market_event(
    state: &AlertDeliveryState,
    cooldowns: &mut HashMap<AlertKey, std::time::Instant>,
    event: &analytics_core::events::MarketEvent,
) {
    let event_name = event.kind.name();
    let rows = match db::list_enabled_indicator_alert_preferences(state.db.pool()).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "could not read indicator alert preferences");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }

    for row in &rows {
        if row.event_name != event_name {
            continue;
        }
        if !passes_session_gate(&row.channels, event.bar_time, &analytics_core::SessionWindow::defaults()) {
            debug!(
                workspace = %row.workspace_id,
                event = %event_name,
                "market-event alert suppressed outside its gated session"
            );
            continue;
        }

        let key = AlertKey {
            workspace_id: row.workspace_id,
            revision_id: row.revision_id,
            event_name: row.event_name.clone(),
        };
        if let Some(last) = cooldowns.get(&key) {
            if last.elapsed() < DELIVERY_COOLDOWN {
                continue;
            }
        }
        deliver_market_alert(state, &key, event).await;
        cooldowns.insert(key, std::time::Instant::now());
    }
}

/// Deliver a derived-market-event alert.
async fn deliver_market_alert(
    state: &AlertDeliveryState,
    key: &AlertKey,
    event: &analytics_core::events::MarketEvent,
) {
    let Some(webhook_url) = &state.webhook_url else {
        info!(
            workspace = %key.workspace_id,
            event = %key.event_name,
            "market-event alert fired but no webhook is configured"
        );
        return;
    };

    let payload = serde_json::json!({
        "type": "market_event_alert",
        "workspace_id": key.workspace_id,
        "revision_id": key.revision_id,
        "event": key.event_name,
        "symbol": event.symbol,
        "timeframe": event.timeframe.to_string(),
        "price": event.price,
        "bar_time_ns": event.bar_time,
        "side": event.side.map(|s| s.name()),
    });

    match state.http.post(webhook_url).json(&payload).send().await {
        Ok(response) if response.status().is_success() => {
            info!(
                workspace = %key.workspace_id,
                event = %key.event_name,
                symbol = %event.symbol,
                "market-event alert delivered"
            );
        }
        Ok(response) => {
            warn!(
                status = %response.status(),
                workspace = %key.workspace_id,
                event = %key.event_name,
                "market-event alert webhook rejected the delivery"
            );
        }
        Err(error) => {
            warn!(
                error = %error,
                workspace = %key.workspace_id,
                event = %key.event_name,
                "market-event alert delivery failed"
            );
        }
    }
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

    #[test]
    fn a_session_gate_reads_the_channels_json_and_refuses_unknown_names() {
        let json = serde_json::json!({"webhook": true, "session": "london"});
        assert_eq!(gated_session(&json), Some(analytics_core::SessionKind::London));
        let json = serde_json::json!({"session": "new_york"});
        assert_eq!(gated_session(&json), Some(analytics_core::SessionKind::NewYork));
        // Unknown names are None -- ungated -- because a typo'd gate silently
        // deleting all deliveries would be worse than delivering ungated.
        assert_eq!(gated_session(&serde_json::json!({"session": "londoner"})), None);
        // And a preference with no session key at all is simply ungated.
        assert_eq!(gated_session(&serde_json::json!({"webhook": true})), None);
    }

    #[test]
    fn a_killzone_gate_passes_only_inside_its_window() {
        let gate = serde_json::json!({"session": "london"});
        let windows = analytics_core::SessionWindow::defaults();
        let ns_per_min = 60 * 1_000_000_000i64;
        // 08:30 UTC is inside London 07:00-10:00.
        assert!(passes_session_gate(&gate, 8 * 60 * ns_per_min + 30 * ns_per_min, &windows));
        // 16:00 is outside every window.
        assert!(!passes_session_gate(&gate, 16 * 60 * ns_per_min, &windows));
        // And an ungated preference passes everywhere, which is the default.
        let ungated = serde_json::json!({});
        assert!(passes_session_gate(&ungated, 16 * 60 * ns_per_min, &windows));
    }

    #[test]
    fn derived_market_event_kinds_are_the_preference_names() {
        // The preference's event_name is the event kind's canonical name, so
        // a shell can offer the menu from `EventKind::ALL` and the worker
        // matches on exactly those strings.
        assert_eq!(analytics_core::events::EventKind::FvgCreated.name(), "fvg_created");
        assert_eq!(
            analytics_core::events::EventKind::LiquiditySweep.name(),
            "liquidity_sweep"
        );
    }
}
