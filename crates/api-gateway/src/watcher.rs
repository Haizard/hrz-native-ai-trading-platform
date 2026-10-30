//! The zone watcher (docs/48): the agent's drawings, watched around the clock.
//!
//! A monitor is one user's standing instruction -- "watch this chart for price
//! touching the zones the agent drew" -- and this loop is what carries it out.
//! Every tick it reads the enabled monitors, checks the newest candles against
//! the user's AI drawings, and on a touch it wakes the agent with the
//! monitor's skill pinned: the run re-reads the chart, adjusts or removes the
//! affected zone, and its thesis lands in the events feed the panel shows.
//!
//! The cost posture is deliberate, because this loop spends real money:
//! one run per monitor per tick, a floor between a monitor's runs, a per-zone
//! cooldown from the events table, and a cap on runs per tick across all
//! users. A zone price sits inside is one notification, not a subscription.

use std::sync::Arc;
use std::time::Duration;

use ai_agent::{AgentError, AskRequest, MarketDataSource};
use analytics_core::Timeframe;
use chrono::Utc;

use crate::market_data::WindowMarketData;
use crate::AppState;

/// How often the loop checks the monitors. Candle-close granularity is the
/// honest cadence for a zone watcher; finer ticks spend API quota to learn
/// the same close earlier than the bar can confirm it.
const TICK: Duration = Duration::from_secs(45);

/// The quiet time between one monitor's runs, whatever zones fire. A burst of
/// touches on one chart is one analysis, not a queue of them.
const MONITOR_FLOOR_MINUTES: i64 = 15;

/// The most runs one tick may start, across every user. Beyond it the
/// remaining hits wait for the next tick: 45 seconds late is a delay, a
/// runaway bill is an incident.
const MAX_RUNS_PER_TICK: usize = 3;

/// The thesis narrative's bound in the events feed: the panel shows the head,
/// and a whole thesis would bury the reason the event exists.
const SUMMARY_CHARS: usize = 800;

/// Start the watcher. Called once from `main`; the task lives as long as the
/// process and reports its own failures rather than dying silently.
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The watcher is useless without both: no database means no monitors,
        // no agent means a touch with nobody to wake. Say so once, plainly,
        // rather than failing the same way every tick forever.
        if state.db.is_none() {
            // The agent is deliberately NOT a gate: resolve_agent builds the
            // per-user agent at fire time (deployment default or the user's
            // own provider), and a user without either gets the honest
            // failure as their event's reason, not a silent watcher.
            tracing::warn!("zone watcher disabled: no database configured");
            return;
        }
        tracing::info!("zone watcher started ({}s tick)", TICK.as_secs());
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if let Err(err) = tick_once(&state).await {
                tracing::warn!(error = %err, "zone watcher tick failed");
            }
        }
    })
}

/// One sweep over the enabled monitors.
async fn tick_once(state: &AppState) -> Result<(), String> {
    let database = state.db.as_ref().expect("spawn checks the database");
    let pool = database.pool();
    let monitors = db::watch::list_enabled_watch_monitors(pool)
        .await
        .map_err(|e| e.to_string())?;
    if monitors.is_empty() {
        return Ok(());
    }
    let data = WindowMarketData::new(state.windows.clone());
    let mut runs = 0_usize;
    for monitor in monitors {
        if runs >= MAX_RUNS_PER_TICK {
            break;
        }
        match check_monitor(state, database, &data, &monitor).await {
            Ok(true) => runs += 1,
            Ok(false) => {}
            Err(err) => tracing::warn!(
                monitor = %monitor.id,
                symbol = %monitor.symbol,
                error = %err,
                "watch check failed"
            ),
        }
    }
    Ok(())
}

/// Check one monitor's zones against the newest candles; fire at most one run.
async fn check_monitor(
    state: &AppState,
    database: &db::Database,
    data: &WindowMarketData,
    monitor: &db::watch::WatchMonitor,
) -> Result<bool, String> {
    let pool = database.pool();
    // The floor first: it is one cheap read, and it quiets a bursting chart
    // before any deeper work happens.
    if let Some(last) = db::watch::latest_watch_event_for_monitor(pool, monitor.id)
        .await
        .map_err(|e| e.to_string())?
    {
        if (Utc::now() - last).num_minutes() < MONITOR_FLOOR_MINUTES {
            return Ok(false);
        }
    }
    let timeframe: Timeframe = monitor.timeframe.parse().map_err(|_| {
        format!(
            "monitor names a resolution the engine does not: {}",
            monitor.timeframe
        )
    })?;
    // The window: the newest candle and the one before it, because "crossed"
    // is a statement about two closes.
    let latest = data
        .latest_candle_time(&monitor.symbol, timeframe)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no candles stored for {}", monitor.symbol))?;
    let width = timeframe.nanos();
    let candles = data
        .candles(
            &monitor.symbol,
            timeframe,
            latest - 3 * width,
            latest + width,
        )
        .await
        .map_err(|e| e.to_string())?;
    if candles.len() < 2 {
        return Ok(false);
    }
    let current = &candles[candles.len() - 1];
    let previous = &candles[candles.len() - 2];

    let drawings = db::drawings::list_drawings(pool, monitor.user_id, &monitor.symbol)
        .await
        .map_err(|e| e.to_string())?;
    for drawing in &drawings {
        // Only the agent's own work: a user's hand-drawn level is their
        // analysis, and the watcher waking the agent over it would be the
        // machine grading the human's homework unasked.
        if drawing.created_by.as_deref() != Some("ai") {
            continue;
        }
        let Some(reason) = zone_touch(drawing, previous.close, current) else {
            continue;
        };
        // The zone cooldown: one touch, one event, however long price loiters.
        if let Some(last) = db::watch::latest_watch_event_for_drawing(pool, monitor.id, drawing.id)
            .await
            .map_err(|e| e.to_string())?
        {
            if (Utc::now() - last).num_minutes() < i64::from(monitor.cooldown_minutes) {
                continue;
            }
        }
        // The event is recorded BEFORE the run: a crashed run still leaves
        // the touch on record, with `failed` naming what happened next.
        let event = db::watch::create_watch_event(pool, monitor, Some(drawing.id), &reason)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(
            monitor = %monitor.id,
            symbol = %monitor.symbol,
            reason = %reason,
            "zone watcher fired"
        );
        let (status, summary) = match run_for_touch(state, monitor, drawing, &reason).await {
            Ok(summary) => ("done", Some(summary)),
            Err(err) => {
                tracing::warn!(monitor = %monitor.id, error = %err, "watch run failed");
                ("failed", Some(err))
            }
        };
        db::watch::finish_watch_event(pool, event.id, status, summary.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        // One run per monitor per tick: the run re-reads the whole chart, so
        // the second zone's news is in the first run's answer anyway.
        return Ok(true);
    }
    Ok(false)
}

/// Whether the newest candle touched this zone, phrased for the agent and the
/// events panel. `previous_close` makes "crossed" sayable; the candle's own
/// range makes "wicked into" and "closed inside" different sentences, because
/// a wick through a zone and a close inside it are different reads.
fn zone_touch(
    drawing: &db::drawings::DrawingRow,
    previous_close: f64,
    candle: &analytics_core::Candle,
) -> Option<String> {
    let label = drawing
        .label
        .as_deref()
        .unwrap_or("unnamed zone")
        .to_string();
    match drawing.kind.as_str() {
        "hline" => {
            let Some(first) = drawing.points.first() else {
                return None;
            };
            let price = first.price;
            if candle.low <= price && candle.high >= price {
                let direction = if previous_close < price && candle.close >= price {
                    "crossed up through"
                } else if previous_close > price && candle.close <= price {
                    "crossed down through"
                } else {
                    "traded into"
                };
                Some(format!(
                    "price {direction} the level {price:.2} ('{label}')"
                ))
            } else {
                None
            }
        }
        "rect" | "zone" => {
            // A zone's two prices are its first two points, in whichever order
            // they were drawn — the low/high sort below is what makes the
            // difference between a user's two click directions disappear.
            let (Some(first), Some(second)) = (drawing.points.first(), drawing.points.get(1))
            else {
                return None;
            };
            let other = second.price;
            let (low, high) = if first.price <= other {
                (first.price, other)
            } else {
                (other, first.price)
            };
            if candle.close >= low && candle.close <= high {
                Some(format!(
                    "price closed inside the zone {low:.2}–{high:.2} ('{label}')"
                ))
            } else if previous_close >= low
                && previous_close <= high
                && (candle.close < low || candle.close > high)
            {
                // The break check precedes the wick check on purpose: a candle
                // that was inside and closed out IS a break, even when its
                // range still overlaps the zone on the way out.
                let side = if candle.close < low { "below" } else { "above" };
                Some(format!(
                    "price left the zone {low:.2}–{high:.2} and closed {side} it ('{label}')"
                ))
            } else if candle.low <= high && candle.high >= low {
                Some(format!(
                    "price wicked into the zone {low:.2}–{high:.2} but closed outside it ('{label}')"
                ))
            } else {
                None
            }
        }
        // Trendlines and fibs need the shape's price AT the candle, which is
        // angle math over its anchors -- a later pass. Honest absence beats a
        // watched drawing that is watched wrong.
        _ => None,
    }
}

/// Wake the agent over a touch, with the monitor's skill pinned and the full
/// per-user capability set the socket path attaches -- drawings read AND
/// write, memory, snapshots, backtests. No chart context and no captures:
/// this run has no screen behind it, and `look_at_chart` says so honestly.
async fn run_for_touch(
    state: &AppState,
    monitor: &db::watch::WatchMonitor,
    drawing: &db::drawings::DrawingRow,
    reason: &str,
) -> Result<String, String> {
    // The run must see the user's OWN skills -- a pin naming one of them is
    // why the monitor exists -- and the user's own provider when they have
    // one. The deployment agent alone would know neither.
    let user = crate::auth::UserContext {
        user_id: monitor.user_id,
        email: String::new(),
    };
    let agent = crate::provider_routes::resolve_agent(state, &user)
        .await
        .map_err(|e| format!("the run could not be built: {e:?}"))?;
    let database = state.db.as_ref().expect("spawn checks the database");
    let question = format!(
        "The zone watcher woke you: on {} {}, {}.          Re-read the current chart under your playbook: confirm or invalidate the setup,          update or remove the affected drawing (update_drawing / delete_drawing by id {} --          never leave a stale zone standing), draw anything new the read produces,          then submit the thesis.",
        monitor.symbol, monitor.timeframe, reason, drawing.id
    );
    let mut ask = AskRequest::new(&monitor.symbol, &question);
    if let Some(skill) = &monitor.skill_id {
        ask = ask.with_skill(skill);
    }
    ask = ask.with_timeframes(vec![monitor.timeframe.clone()]);
    ask = ask.with_drawings(
        ai_agent::DrawingsContext::new(
            Arc::new(crate::market_data::DbUserDrawings::new(
                std::sync::Arc::clone(database),
            )),
            monitor.user_id.to_string(),
        )
        .with_writer(Arc::new(crate::market_data::DbDrawingWriter::new(
            std::sync::Arc::clone(database),
        ))),
    );
    ask = ask.with_memory(
        ai_agent::MemoryContext::new(
            Arc::new(crate::market_data::DbAgentMemory::new(
                std::sync::Arc::clone(database),
            )),
            monitor.user_id.to_string(),
        )
        .with_writer(Arc::new(crate::market_data::DbAgentMemory::new(
            std::sync::Arc::clone(database),
        ))),
    );
    ask = ask.with_snapshots(ai_agent::SnapshotsContext::new(
        Arc::new(crate::market_data::DbSnapshotStore::new(
            std::sync::Arc::clone(database),
        )),
        monitor.user_id.to_string(),
    ));
    let backtests: Arc<dyn ai_agent::tools::BacktestRunner> = state.research.clone();
    ask = ask.with_backtests(backtests);
    // Web search, the same grant the socket and REST paths attach: a watched
    // symbol's context is news as much as it is bars.
    if let Some(searcher) = &state.web_search {
        ask = ask.with_web_search(searcher.clone());
    }
    let (provider, symbol_class) = crate::venue_routes::market_scope();
    ask = ask.with_capabilities(ai_agent::CapabilityView::new(
        std::sync::Arc::clone(&state.capability_registry),
        provider,
        symbol_class,
    ));
    // The same claim the socket path makes: a watched symbol is a reason for
    // it to have a feed, and without one the buffer never fills.
    state.bots.ensure_feed_for(&monitor.symbol.to_uppercase());

    let data = WindowMarketData::new(state.windows.clone());
    // A watcher wake is a full agent run and costs the same credits as any
    // other; without a balance the run is skipped rather than given away.
    crate::billing::enforce_usage(
        state,
        monitor.user_id,
        crate::billing::UsageMetric::AiCredit,
        crate::billing::CREDIT_WATCHER_RUN,
    )
    .await
    .map_err(|e| format!("the run was not charged: {}", e.message()))?;
    let answer = agent
        .ask(&ask, &data)
        .await
        .map_err(|e: AgentError| e.to_string())?;
    let narrative = answer.thesis.narrative;
    Ok(if narrative.chars().count() > SUMMARY_CHARS {
        let mut cut: String = narrative.chars().take(SUMMARY_CHARS).collect();
        cut.push('…');
        cut
    } else {
        narrative
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64) -> analytics_core::Candle {
        analytics_core::Candle {
            symbol: "BTCUSDT".to_string(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn drawing(kind: &str, p1: f64, p2: Option<f64>, label: Option<&str>) -> db::drawings::DrawingRow {
        let mut points = vec![db::drawings::StoredPoint { time: 0.0, price: p1 }];
        if let Some(price) = p2 {
            points.push(db::drawings::StoredPoint { time: 1.0, price });
        }
        db::drawings::DrawingRow {
            id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".to_string(),
            kind: kind.to_string(),
            points,
            options: db::drawings::no_options(),
            label: label.map(str::to_string),
            created_by: Some("ai".to_string()),
            agent: None,
            confidence: None,
            reason: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn an_hline_reports_the_cross_direction() {
        let level = drawing("hline", 100.0, None, Some("shelf"));
        let up_bar = candle(99.0, 101.0, 98.5, 100.5);
        let reason = zone_touch(&level, 99.0, &up_bar).expect("a touch");
        assert!(reason.contains("crossed up through"), "{reason}");
        let down_bar = candle(101.0, 101.5, 98.0, 99.5);
        let down = zone_touch(&level, 101.0, &down_bar).expect("a touch");
        assert!(down.contains("crossed down through"), "{down}");
    }

    #[test]
    fn an_hline_untouched_is_silent() {
        let level = drawing("hline", 100.0, None, None);
        let bar = candle(101.0, 102.0, 100.5, 101.5);
        assert!(zone_touch(&level, 101.0, &bar).is_none());
    }

    #[test]
    fn a_rect_distinguishes_a_close_inside_from_a_wick() {
        let zone = drawing("rect", 100.0, Some(105.0), Some("demand"));
        let inside = zone_touch(&zone, 99.0, &candle(103.0, 106.0, 102.0, 104.0)).expect("inside");
        assert!(inside.contains("closed inside"), "{inside}");
        let wick = zone_touch(&zone, 99.0, &candle(97.0, 101.0, 96.0, 98.0)).expect("wick");
        assert!(wick.contains("wicked into"), "{wick}");
        assert!(zone_touch(&zone, 95.0, &candle(94.0, 96.0, 93.0, 95.0)).is_none());
    }

    #[test]
    fn a_rect_reports_a_break_out() {
        let zone = drawing("rect", 100.0, Some(105.0), None);
        let out = zone_touch(&zone, 103.0, &candle(103.0, 104.0, 98.0, 99.0)).expect("break");
        assert!(out.contains("closed below"), "{out}");
    }

    #[test]
    fn unwatched_kinds_are_silent_rather_than_watched_wrong() {
        let line = drawing("trendline", 100.0, Some(110.0), None);
        let bar = candle(100.0, 111.0, 99.0, 110.0);
        assert!(zone_touch(&line, 100.0, &bar).is_none());
    }
}
