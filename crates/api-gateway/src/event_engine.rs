//! The derived-event lane: structure, sweeps and gaps, from closed candles
//! (`docs/09`'s event-driven posture, made live).
//!
//! ## What this is
//!
//! The market bus (`market_data::MarketEventBus`) carries **raw** lanes —
//! trades, candles, books. Nothing on it says "the equal highs at 104 100 were
//! just swept" or "a bullish fair value gap opened on this bar", because those
//! are *derived* facts: they exist only after a detector compares the newest
//! bar against what came before it. [`analytics_core::events::detect_events`]
//! is that comparison, and this module is its live host: one watcher per
//! symbol, a bounded recent-events buffer for late joiners, and a read surface
//! (`/events`, `/ws/events/{symbol}`) the shell and the agent both consume.
//!
//! ## The watcher belongs to the symbol, not to a feed
//!
//! The first version tied each watcher to the feed that spawned it and aborted
//! the pair together — which reads as tidy and is wrong. Candles reach a
//! symbol's history through paths that outlive any one feed (`feed_candle`
//! from an out-of-process publisher, a backfill, a test) and through
//! deployments where `MARKET_FEED` is off entirely. A feed-shaped watcher
//! makes those candles close in silence: nothing was ever derived from them,
//! and nothing said why. So the watcher is spawned **once per symbol for the
//! life of the process**, idempotently, the first time anything — a feed
//! opening, a socket connecting, a REST read — asks for the symbol's lane. It
//! reads *history*, never the feed, which is why it can outlive what produced
//! the bars: backfill, a replaying collector and a live feed all produce
//! events through exactly one path.
//!
//! ## The forming bar is stripped, and that is the dedup
//!
//! `detect_events` reports what the **newest** candle produced, and it is the
//! caller's job to feed it each closed bar exactly once. The history buffer
//! makes that easy to get wrong: [`latest()`](market_data::CandleHistory::latest)
//! counts a forming bar as the newest, and the forming bar is rebuilt once a
//! second — a watcher that trusted it would re-report the same sweep every
//! tick. So the window here is `latest(window)` with any trailing forming bar
//! removed: the newest candle is then closed, it becomes the forming bar when
//! the next tick rebuilds it, and it re-enters the window once — as a *closed*
//! bar — only when its bucket actually ends. Each bar therefore causes exactly
//! one detection pass in which it is the newest, which is the whole dedup.
//! The ring's `(kind, bar_time)` repeat check is the second line of defence,
//! not the first.
//!
//! ## Reads, and why both shapes exist
//!
//! A socket is for "tell me as it happens"; the REST route with `since` is for
//! "what did I miss while the tab was asleep", which a socket cannot answer
//! after a reconnect. Both read the same ring buffer, so they cannot disagree
//! — the same one-source rule `windows` states for candles.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use analytics_core::events::{EventConfig, MarketEvent};

use crate::error::ApiError;
use crate::AppState;

/// How many recent events are kept per symbol, across all its resolutions.
///
/// The events that matter arrive in bursts around structure breaks; 512 is
/// several sessions of ordinary activity on one symbol, and a client that
/// falls further behind than that is better served by "ask for the window
/// again" than by an unbounded queue.
const EVENT_BUFFER: usize = 512;

/// How long a watcher waits before re-reading the buffer without a candle.
///
/// The watcher is tick-driven rather than publish-driven on purpose: the diff
/// is idempotent (stripping the forming bar is the dedup; the ring's repeat
/// check is the backup), so a quiet market costs one lock and one emptiness
/// check per tick, while subscribing the candle bus would need a second task
/// per symbol to guard against lag — and *that* task would be the one that
/// could silently die.
const IDLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// The window the watcher diffs, in bars.
///
/// Large enough for the detectors' lookbacks with room for the swings they
/// confirm; small enough that one watcher per symbol costs nothing. The same
/// number the pure module defaults to, on purpose: a replayed window and a
/// live window should be the *same* window.
const DEFAULT_WINDOW: usize = analytics_core::events::DEFAULT_WINDOW;

/// The events and the broadcast for one symbol.
pub struct SymbolEvents {
    /// Newest last. Capped at [`EVENT_BUFFER`]; a ring by truncation, which
    /// is honest about what a buffer is.
    recent: Mutex<Vec<MarketEvent>>,
    tx: broadcast::Sender<MarketEvent>,
}

impl SymbolEvents {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            recent: Mutex::new(Vec::new()),
            tx,
        }
    }

    /// Record events from one detection pass, newest last, and broadcast them.
    ///
    /// A repeat that arrives through lag or a replay is dropped by bar time:
    /// an event is identified by *what bar caused it* and *what it says about
    /// that bar*, not by where it sits in the queue. A detector change that
    /// re-fires an old bar's sweep is therefore still a no-op — which is the
    /// property a replaying consumer needs.
    fn record(&self, events: Vec<MarketEvent>) {
        if events.is_empty() {
            return;
        }
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for event in events {
            let repeat = recent
                .iter()
                .rev()
                .any(|seen| seen.kind == event.kind && seen.bar_time == event.bar_time);
            if repeat {
                continue;
            }
            // `send` fails only when nothing is subscribed; the buffer write
            // below is what makes the event reachable anyway.
            let _ = self.tx.send(event.clone());
            recent.push(event);
        }
        let excess = recent.len().saturating_sub(EVENT_BUFFER);
        if excess > 0 {
            recent.drain(..excess);
        }
    }

    /// Events at or after `since_ns` (bar-open comparison), oldest first.
    fn since(&self, since_ns: Option<i64>) -> Vec<MarketEvent> {
        let recent = self
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match since_ns {
            Some(since) => recent
                .iter()
                .filter(|event| event.bar_time >= since)
                .cloned()
                .collect(),
            None => recent.clone(),
        }
    }

    /// Subscribe to everything from here on.
    fn subscribe(&self) -> broadcast::Receiver<MarketEvent> {
        self.tx.subscribe()
    }
}

/// The engine: every symbol that has ever been watched here, with its events.
///
/// Built once and shared through [`AppState`]; entries are never removed, so a
/// reconnecting client finds its symbol's history for the life of the process.
/// A deployment that watched ten thousand symbols would hold at most ten
/// thousand small vectors — the reaper bounds the *feeds*, not the record of
/// what they saw.
#[derive(Default)]
pub struct EventEngine {
    symbols: Mutex<std::collections::HashMap<String, SymbolLane>>,
}

/// One symbol's lane plus the watcher that feeds it, when one was spawned.
struct SymbolLane {
    events: Arc<SymbolEvents>,
    /// `Some` once the symbol is being diffed. Kept rather than detached: a
    /// watcher alive here is *the* record that this symbol is watched, and a
    /// bare `Option` is what makes [`EventEngine::watch`] idempotent.
    watcher: Option<tokio::task::AbortHandle>,
}

impl EventEngine {
    /// An engine that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lane for one symbol, creating the buffer if this is the first ask.
    ///
    /// Pure access — no task is spawned here. A route that only *reads* must
    /// not conjure a watcher as a side effect (the first version did, and the
    /// watcher it conjured had no history registry to read, so it sat parked
    /// forever); spawning is [`Self::watch`]'s job and carries the registry
    /// with it.
    #[must_use]
    pub fn lane(&self, symbol: &str) -> Arc<SymbolEvents> {
        let symbol = symbol.to_uppercase();
        let mut symbols = self
            .symbols
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        symbols
            .entry(symbol)
            .or_insert_with(|| SymbolLane {
                events: Arc::new(SymbolEvents::new()),
                watcher: None,
            })
            .events
            .clone()
    }

    /// Ensure the symbol is being diffed, and return its lane.
    ///
    /// Idempotent per symbol: the feed path, the socket route and the REST
    /// route may all call it in any order and land on one watcher. This is
    /// also why the watcher is per-symbol for the life of the process rather
    /// than tied to a feed: candles reach a symbol's history through paths
    /// that outlive any one feed (a backfill, an out-of-process publisher via
    /// `feed_candle`, a test), and a feed-shaped watcher makes those bars
    /// close in silence.
    pub fn watch(
        &self,
        symbol: &str,
        history: Arc<market_data::HistoryRegistry>,
    ) -> Arc<SymbolEvents> {
        let symbol = symbol.to_uppercase();
        let mut symbols = self
            .symbols
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lane = symbols.entry(symbol.clone()).or_insert_with(|| SymbolLane {
            events: Arc::new(SymbolEvents::new()),
            watcher: None,
        });
        if lane.watcher.is_none() {
            lane.watcher = Some(spawn_watcher(symbol, history, Arc::clone(&lane.events)));
        }
        Arc::clone(&lane.events)
    }

    /// Symbols with at least one event recorded, sorted.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let symbols = self
            .symbols
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out: Vec<String> = symbols
            .iter()
            .filter(|(_, lane)| !lane.events.since(None).is_empty())
            .map(|(symbol, _)| symbol.clone())
            .collect();
        out.sort();
        out
    }
}

/// The loop that turns one symbol's closed candles into events, forever.
fn spawn_watcher(
    symbol: String,
    history: Arc<market_data::HistoryRegistry>,
    lane: Arc<SymbolEvents>,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(IDLE_RECHECK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            for timeframe in history.timeframes(&symbol) {
                let series = history.series(&symbol, timeframe);
                let mut window = series.latest(DEFAULT_WINDOW);
                // The forming bar is rebuilt once a second; treating it as the
                // newest candle would re-run the same detection on every tick
                // and re-report every partial-bar event. See the module doc:
                // stripping it *is* the dedup, because each bar then becomes
                // the newest exactly once — when it closes.
                if let Some(last) = series.forming() {
                    if window
                        .last()
                        .is_some_and(|newest| newest.open_time == last.open_time)
                    {
                        window.pop();
                    }
                }
                if window.is_empty() {
                    continue;
                }
                let events = analytics_core::events::detect_events(
                    &window,
                    &symbol,
                    &EventConfig::default(),
                );
                if !events.is_empty() {
                    debug!(
                        symbol = %symbol,
                        %timeframe,
                        count = events.len(),
                        "the event watcher recorded derived events"
                    );
                    lane.record(events);
                }
            }
        }
    });
    handle.abort_handle()
}

/// `GET /events?symbol=...&since=...`
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Instrument, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Bar-open time (unix nanos) to start from, inclusive. Absent means
    /// "everything buffered", which is what a fresh tab wants.
    pub since: Option<i64>,
}

/// `GET /events` — the response body.
#[derive(Debug, Serialize)]
pub struct EventsResponse {
    /// The symbol these belong to.
    pub symbol: String,
    /// The events, oldest first.
    pub events: Vec<MarketEvent>,
}

/// `GET /events` — the symbol's recent derived events.
///
/// # Errors
/// The read answers from RAM alone; there is no database on this path.
pub async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let symbol = query.symbol.to_uppercase();
    // Reading is also ensuring: a tab that wakes up and asks "what did I
    // miss" must not depend on somebody else having opened a socket first —
    // the watcher it starts here picks the recent window up on its first
    // (immediate) tick.
    let lane = state.events.watch(&symbol, state.bots.history());
    Ok(Json(EventsResponse {
        symbol,
        events: lane.since(query.since),
    }))
}

/// `/ws/events/{symbol}` — live derived events, one JSON frame each.
///
/// Public like the other market channels: an event is market data once it is
/// derived, and nothing on this lane names a user.
pub async fn stream(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> axum::response::Response {
    let symbol = symbol.to_uppercase();
    // Connecting starts the pipeline, not just the subscription: the feed (so
    // candles exist) and the watcher (so they are derived) both come alive
    // here, which is why a fresh socket sees events without anyone opening a
    // chart first.
    state.bots.ensure_feed_for(&symbol);
    let lane = state.events.watch(&symbol, state.bots.history());
    let rx = lane.subscribe();
    let channel = format!("/ws/events/{symbol}");
    let metrics = Arc::clone(&state.metrics);
    upgrade.on_upgrade(move |socket| event_loop(socket, rx, channel, symbol, metrics))
}

async fn event_loop(
    socket: axum::extract::ws::WebSocket,
    mut events: broadcast::Receiver<MarketEvent>,
    channel: String,
    symbol: String,
    metrics: Arc<observability::metrics::Registry>,
) {
    use axum::extract::ws::Message;
    // `SinkExt` is deliberately not imported: the sends go through
    // `ws::send_text`/`send_binary`, which bound it themselves.
    use futures::StreamExt;

    let (mut sink, mut stream) = socket.split();
    let connection = crate::ws::Connection::open(metrics, "events");
    let hello = crate::ws::Frame::Subscribed {
        channel: &channel,
        detail: serde_json::json!({ "symbol": symbol }),
    };
    if crate::ws::send_text(&mut sink, &hello).await.is_err() {
        connection.closed(&channel, "hello send failed");
        return;
    }

    let reason = loop {
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => {
                    let frame = crate::ws::Frame::Data {
                        payload: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
                    };
                    if crate::ws::send_binary(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // Events are rare enough that a lag here means the client
                    // stopped reading for a while; tell it rather than let a
                    // gap read as "nothing happened".
                    connection.missed(dropped);
                    let frame = crate::ws::Frame::Lagged { dropped };
                    if crate::ws::send_binary(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Closed) => break "engine dropped the lane",
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) => break "client closed",
                None => break "stream ended",
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%channel, "events socket error: {e}");
                    break "socket error";
                }
            },
        }
    };
    connection.closed(&channel, reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::events::{EventKind, MarketEvent};
    use analytics_core::types::{Side, Timeframe};

    fn event(kind: EventKind, bar_time: i64) -> MarketEvent {
        MarketEvent {
            kind,
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            bar_time,
            close: 100.0,
            price: 101.0,
            side: Some(Side::Sell),
            zone_low: None,
            zone_high: None,
            magnitude: None,
            formed_at: None,
        }
    }

    #[test]
    fn a_replayed_bar_time_is_not_recorded_twice() {
        // The ring's dedup contract: the watcher re-diffs its window on every
        // idle tick, and a lag can hand it the same bar twice. Both arrive as
        // "the sweep that bar caused" — only the first may land.
        let lane = SymbolEvents::new();
        lane.record(vec![event(EventKind::LiquiditySweep, 1_000)]);
        lane.record(vec![event(EventKind::LiquiditySweep, 1_000)]);
        assert_eq!(lane.since(None).len(), 1);
    }

    #[test]
    fn the_same_bar_may_cause_two_different_events() {
        // Dedup is per (kind, bar), not per bar: one candle can sweep a level
        // and open a gap, and dropping the second would be a lost fact.
        let lane = SymbolEvents::new();
        lane.record(vec![event(EventKind::LiquiditySweep, 1_000)]);
        lane.record(vec![event(EventKind::FvgCreated, 1_000)]);
        assert_eq!(lane.since(None).len(), 2);
    }

    #[test]
    fn the_buffer_is_a_ring_not_a_leak() {
        let lane = SymbolEvents::new();
        for i in 0..(EVENT_BUFFER + 128) {
            lane.record(vec![event(EventKind::VolumeSpike, i as i64)]);
        }
        assert_eq!(lane.since(None).len(), EVENT_BUFFER);
        // The survivors are the newest ones, in order.
        let kept = lane.since(None);
        assert_eq!(kept.first().unwrap().bar_time, 128);
    }

    #[test]
    fn since_is_an_inclusive_lower_bound_on_bar_time() {
        let lane = SymbolEvents::new();
        lane.record(vec![event(EventKind::VolumeSpike, 100)]);
        lane.record(vec![event(EventKind::VolumeSpike, 200)]);
        // Inclusive on purpose: a reconnecting client asks "from the last one
        // I already have", and an exclusive bound would skip exactly that one
        // if it landed between the read and the disconnect.
        let since = lane.since(Some(100));
        assert_eq!(since.len(), 2);
        assert_eq!(since.first().unwrap().bar_time, 100);
        // One past the boundary drops the first event only.
        let later = lane.since(Some(101));
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].bar_time, 200);
    }

    #[tokio::test]
    async fn a_recorded_event_reaches_a_subscriber() {
        let lane = SymbolEvents::new();
        let mut rx = lane.subscribe();
        lane.record(vec![event(EventKind::Bos, 5_000)]);
        let seen = rx.recv().await.expect("the event is broadcast");
        assert_eq!(seen.kind, EventKind::Bos);
        assert_eq!(seen.bar_time, 5_000);
    }

    #[tokio::test]
    async fn watching_twice_spawns_one_watcher_on_one_lane() {
        // Idempotence is the contract every caller relies on: feed, socket and
        // REST route may arrive in any order and must land on one watcher.
        let engine = EventEngine::new();
        let history = Arc::new(market_data::HistoryRegistry::new());
        let first = engine.watch("btcusdt", Arc::clone(&history));
        let second = engine.watch("BTCUSDT", Arc::clone(&history));
        assert!(Arc::ptr_eq(&first, &second), "one lane, whatever the case");
        // And the watcher really runs: bars recorded before the watch are
        // picked up by the interval's immediate first tick.
        let base = 1_760_000_000_000_000_000i64;
        let min = 60_000_000_000i64;
        for (i, (high, low, close)) in [
            (10.0, 8.0, 9.0),
            (10.5, 9.0, 9.5),
            (10.0, 9.0, 9.5),
            (12.0, 9.0, 11.0),
            (10.5, 9.0, 9.5),
            (10.0, 9.0, 9.5),
            (10.5, 9.0, 9.5),
            (12.0, 9.0, 11.5),
            (10.5, 9.0, 9.5),
            (10.0, 9.0, 9.5),
            (10.5, 9.0, 9.5),
            (14.0, 10.8, 11.0),
        ]
        .into_iter()
        .enumerate()
        {
            history.record_closed(&analytics_core::types::Candle {
                symbol: "BTCUSDT".into(),
                timeframe: analytics_core::types::Timeframe::M1,
                open_time: base + i as i64 * min,
                open: close,
                high,
                low,
                close,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        let lane = engine.watch("BTCUSDT", Arc::clone(&history));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while lane.since(None).is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the watcher never recorded the sweep"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            lane.since(None)
                .iter()
                .any(|e| e.kind == EventKind::LiquiditySweep),
            "the equal-highs sweep is the event the shape must produce"
        );
    }

    #[tokio::test]
    async fn a_symbol_with_events_is_listed_and_a_fresh_lane_is_not() {
        // Async because `lane` spawns the watcher task — even the parked one a
        // registry-less ask gets — and spawning needs a runtime.
        let engine = EventEngine::new();
        let lane = engine.lane("BTCUSDT");
        assert!(engine.symbols().is_empty(), "a lane exists but is empty");
        lane.record(vec![event(EventKind::Choch, 42)]);
        assert_eq!(engine.symbols(), vec!["BTCUSDT".to_string()]);
    }
}
