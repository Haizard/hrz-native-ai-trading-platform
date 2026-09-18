//! WebSocket channels (`docs/12-API-GATEWAY.md`).
//!
//! | Channel | Source | Auth |
//! |---|---|---|
//! | `/ws/market/{symbol}/{timeframe}` | the supervisor's market bus | none -- market data is public |
//! | `/ws/orderbook/{symbol}` | the supervisor's market bus (depth is maintained in `market-data`) | none -- market data is public |
//! | `/ws/agent/{session_id}` | the Phase 5 agent | token in the query string |
//! | `/ws/bots/{bot_id}` | the supervisor's event broadcast | token in the query string |
//!
//! ## Backpressure, per connection
//!
//! `docs/12`: "WebSocket fan-out must apply backpressure per-connection (drop or
//! coalesce ticks for a slow client) rather than allowing one slow consumer to
//! back up the whole broadcast pipeline."
//!
//! The bus is a `tokio::sync::broadcast`, which is exactly that: a watcher that
//! stops reading is *lagged* and told so, and the publisher never blocks. Each
//! handler turns a lag into a `lagged` frame and carries on, so a stalled client
//! loses frames rather than stalling the feed for everyone else.
//!
//! ## The token is in the query string, and that is a real cost
//!
//! A browser cannot set an `Authorization` header on a WebSocket handshake --
//! the API has no way to express it. The standard workaround is a query
//! parameter, and it has a genuine downside: query strings land in access logs,
//! in `Referer` headers and in browser history. So it is accepted **only here**,
//! never on a REST route, and the token is the same short-lived one the REST
//! side issues.
//!
//! ## Framing
//!
//! `docs/12` asks for binary framing on the high-frequency market channel and
//! allows JSON on the agent and bot channels. The market channel therefore
//! sends **binary** frames -- but the payload inside them is still JSON. A
//! MessagePack or protobuf payload would need a decoder in the shell, and the
//! saving is only real at rates far above one 5m candle every five minutes. The
//! framing is what the spec asks for; the payload is the next step, and it is
//! taken when there is a measurement rather than a guess.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use observability::metrics::{
    Labels, Registry, AGENT_LATENCY, AGENT_PROVIDER_ERRORS, AGENT_REQUESTS, AGENT_THESES,
    AGENT_TOOL_CALLS, WS_CONNECTIONS, WS_DROPS, WS_OPENS,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};
use uuid::Uuid;

use analytics_core::types::{Candle, OrderBookSnapshot};

use crate::auth::UserContext;
use crate::bots::BotEvent;
use crate::error::ApiError;
use crate::AppState;

/// How long a connection has been open, and how much it missed.
///
/// ## Why this is a guard and not two calls
///
/// Each loop below has five ways to exit -- a failed hello, a dead socket, a
/// `Lagged`, a `Closed`, a close frame -- and one of them is a `return` rather
/// than a `break`. An increment at the top and a decrement before each exit is
/// five places to forget, and forgetting one leaks a connection count upward
/// for the life of the process: the gauge says 40 sockets are open when the
/// process is holding three, and the one alarm that would have caught the real
/// leak is now permanently firing. `Drop` cannot be forgotten, including on the
/// early `return`s.
///
/// The label is the channel *kind*, not the symbol or the session id. A gauge
/// per symbol would multiply the series by every market the platform touches,
/// and "connections are growing" is a question about the channel, not about
/// BTCUSDT.
struct Connection {
    metrics: Arc<Registry>,
    channel: &'static str,
    /// When the socket was accepted. Reported on close, because "opened and
    /// closed again in 200ms" and "stayed for six hours" print as the same two
    /// lines until something says how long the connection actually lived.
    opened: Instant,
}

impl Connection {
    /// Count a connection in.
    fn open(metrics: Arc<Registry>, channel: &'static str) -> Self {
        let labels = Labels::new(&[("channel", channel)]);
        metrics.add_gauge(
            WS_CONNECTIONS,
            "WebSocket connections currently open",
            &labels,
            1.0,
        );
        // The gauge says how many are open; the counter says how many opened.
        // A client that reconnects every minute leaves the first flat and moves
        // the second, and that difference is the whole signal: a flat gauge on
        // a climbing counter is churn, a climbing gauge on a flat counter is a
        // leak, and a gauge alone cannot tell you which one you are looking at.
        metrics.count(WS_OPENS, "WebSocket connections accepted, total", &labels);
        Self {
            metrics,
            channel,
            opened: Instant::now(),
        }
    }

    /// Record that this connection was handed a stream with holes in it.
    ///
    /// Counted as a *drop* rather than as lag: the socket still works, but the
    /// client was told `lagged` and a chart drew a gap. A number that climbs is
    /// the signal that a consumer cannot keep up, which is a different incident
    /// from a connection that died.
    fn missed(&self, dropped: u64) {
        self.metrics.add_gauge(
            WS_DROPS,
            "Messages a WebSocket client was too slow to receive",
            &Labels::new(&[("channel", self.channel)]),
            dropped as f64,
        );
    }

    /// Write the closing line: one `info`, carrying why it ended and how long
    /// it lasted.
    ///
    /// Why a method and not a line at the bottom of each loop: every loop below
    /// exits in more than one place, and some of those exits are `return`s that
    /// never reach the bottom. A close recorded in one place is missing from the
    /// others, which is how a reconnect storm ends up in the log as seven
    /// "opened" lines with no closes anywhere beside them -- the log this was
    /// read from had exactly that shape.
    ///
    /// `detail` is whatever identifies the one connection: the full channel, the
    /// session id, the bot id. The gauge is deliberately keyed only on the
    /// channel *kind*, so this field is where the symbol and timeframe survive.
    fn closed(&self, detail: &impl std::fmt::Display, reason: &'static str) {
        info!(
            socket = self.channel,
            %detail,
            reason,
            open_for = ?self.opened.elapsed(),
            "socket closed"
        );
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.metrics.add_gauge(
            WS_CONNECTIONS,
            "WebSocket connections currently open",
            &Labels::new(&[("channel", self.channel)]),
            -1.0,
        );
    }
}

/// Query parameters a WebSocket handshake may carry.
#[derive(Debug, Deserialize)]
pub struct WsAuth {
    /// The session token, because a handshake cannot carry a header.
    pub token: Option<String>,
}

/// How long a DOM waits for its first book before the channel explains itself.
///
/// Long enough to cover the collector's own publish interval (one second by
/// default) plus the REST snapshot that bootstraps the book; short enough that
/// a panel which will never have depth finds out while it still looks like a
/// connection problem rather than an empty market.
const DEPTH_GRACE: Duration = Duration::from_secs(5);

/// A frame every channel may send, so a client can tell a notice from data.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame<'a> {
    /// Sent once on connect: what this socket is now watching.
    Subscribed {
        channel: &'a str,
        detail: serde_json::Value,
    },
    /// Data.
    Data { payload: serde_json::Value },
    /// A step the agent took on its way to an answer.
    ///
    /// Its own type rather than `data` because it is not an answer: a client
    /// that treated it as one would draw a thesis out of the agent's shopping
    /// list. Only `/ws/agent` sends it, and only while a question is in flight.
    Progress { payload: serde_json::Value },
    /// The client fell behind and frames were dropped.
    ///
    /// Explicit rather than silent: a chart that missed candles looks like a
    /// quiet market, and the two must not be confusable.
    Lagged { dropped: u64 },
    /// Something went wrong that the client should know about but which does
    /// not close the socket.
    Notice { message: String },
}

fn json_frame(frame: &Frame<'_>) -> String {
    serde_json::to_string(frame).unwrap_or_else(|e| {
        // Unreachable for these types, but a panic in a socket task would take
        // the connection down with no explanation.
        format!(r#"{{"type":"notice","message":"could not serialize a frame: {e}"}}"#)
    })
}

/// `/ws/market/{symbol}/{timeframe}`
///
/// Public: market data is not user data (`docs/12`'s explicit decision).
pub async fn market(
    State(state): State<AppState>,
    Path((symbol, timeframe)): Path<(String, String)>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let symbol = symbol.to_uppercase();

    // A chart should start the feed, not wait for a bot to happen to. Otherwise
    // "the chart is empty" has two possible causes again.
    state.bots.ensure_feed_for(&symbol);
    let candles = state.bots.subscribe_candles(&symbol);

    let channel = format!("/ws/market/{symbol}/{timeframe}");
    let metrics = Arc::clone(&state.metrics);
    // Read once, at connect, and used only to *word* the notice below: a feed
    // that is off is the likely cause of a channel that stays quiet, so it is
    // worth naming -- exactly as the order book names `MARKET_FEED`.
    let feed_off = state.bots.feed_mode() != crate::bots::FeedMode::Binance;
    upgrade.on_upgrade(move |socket| {
        market_loop(
            socket, candles, channel, symbol, timeframe, metrics, feed_off,
        )
    })
}

async fn market_loop(
    socket: WebSocket,
    mut candles: tokio::sync::broadcast::Receiver<Candle>,
    channel: String,
    symbol: String,
    timeframe: String,
    metrics: Arc<Registry>,
    feed_off: bool,
) {
    let (mut sink, mut stream) = socket.split();
    let connection = Connection::open(metrics, "market");
    info!(%channel, "market socket opened");

    let hello = Frame::Subscribed {
        channel: &channel,
        detail: serde_json::json!({ "symbol": symbol, "timeframe": timeframe }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        connection.closed(&channel, "hello send failed");
        return;
    }

    // An empty channel and a quiet market look identical from the client, which
    // is why this says which one it is -- the same argument as the order book's
    // no-book notice. It does **not** close the socket, and the reason is worth
    // writing down because the first version did.
    //
    // The order book waits for a first book and closes if none arrives. That
    // pattern does not transfer: a book publishes every second, so "nothing
    // within five seconds" is evidence, while a candle closes every five minutes,
    // so waiting long enough to be evidence would mean minutes of silence before
    // an explanation. So this one is decided from configuration instead -- and
    // `FeedMode::Off` means "the gateway will not open a feed", not "no candle
    // will ever be published": `feed_candle` publishes into the same bus from
    // outside the feed, and `FeedMode::Off`'s own documentation says a bot
    // receives whatever else publishes. Closing here would refuse data that
    // exists, and it made the resolution filter below unreachable in the tests
    // that witness it -- a property that stops being observable is the defect
    // this codebase keeps finding. The notice explains the silence; the channel
    // keeps its contract.
    if feed_off {
        let notice = Frame::Notice {
            message: format!(
                "no market feed is configured (MARKET_FEED is not `binance`), so this gateway \
                 will not publish candles into this channel and the chart will not move on its \
                 own. The chart can still draw stored candles from \
                 GET /candles?symbol={symbol}, and anything else that publishes into the bus \
                 will still reach it. Set MARKET_FEED=binance and restart the gateway."
            ),
        };
        if send_text(&mut sink, &notice).await.is_err() {
            connection.closed(&channel, "notice send failed");
            return;
        }
        debug!(%channel, "market socket explained: no feed is configured");
    }

    let reason = loop {
        tokio::select! {
            received = candles.recv() => match received {
                Ok(candle) => {
                    // Only this resolution: the feed builds several, and a
                    // 5m chart has no use for the 1h series.
                    if candle.timeframe.to_string() != timeframe {
                        continue;
                    }
                    let frame = Frame::Data {
                        payload: serde_json::to_value(&candle).unwrap_or(serde_json::Value::Null),
                    };
                    if send_binary(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // Tell the client rather than letting the chart draw a gap
                    // that looks like a quiet market.
                    connection.missed(dropped);
                    let frame = Frame::Lagged { dropped };
                    if send_binary(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Closed) => break "bus closed",
            },
            incoming = stream.next() => match incoming {
                // A market channel is one-way; the only thing worth reading is
                // a close.
                Some(Ok(Message::Close(_))) => break "client closed",
                None => break "stream ended",
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%channel, "market socket error: {e}");
                    break "socket error";
                }
            },
        }
    };
    connection.closed(&channel, reason);
}

/// `/ws/orderbook/{symbol}`
///
/// The book is maintained in `market-data` (REST snapshot, then bridged diffs)
/// and published into the bus by the collector; this is the read end. The
/// socket opens immediately, because a DOM that waited for the first book
/// before completing its handshake would hang on every reconnect.
///
/// What it will *not* do is stream nothing forever. An empty ladder is
/// indistinguishable from a market with no liquidity, so if no book arrives
/// within [`DEPTH_GRACE`] the channel says why and closes.
pub async fn orderbook(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let symbol = symbol.to_uppercase();
    // A DOM should start the feed, not wait for a bot to happen to -- and the
    // feed subscribes depth alongside trades on the same connection.
    state.bots.ensure_feed_for(&symbol);
    let books = state.bots.subscribe_orderbook(&symbol);

    let channel = format!("/ws/orderbook/{symbol}");
    let metrics = Arc::clone(&state.metrics);
    upgrade.on_upgrade(move |socket| orderbook_loop(socket, books, channel, symbol, metrics))
}

async fn orderbook_loop(
    socket: WebSocket,
    mut books: broadcast::Receiver<OrderBookSnapshot>,
    channel: String,
    symbol: String,
    metrics: Arc<Registry>,
) {
    let (mut sink, mut stream) = socket.split();
    let connection = Connection::open(metrics, "orderbook");
    info!(%channel, "order book socket opened");

    let hello = Frame::Subscribed {
        channel: &channel,
        detail: serde_json::json!({ "symbol": symbol }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        connection.closed(&channel, "hello send failed");
        return;
    }

    let Some(first) = first_book(&mut books).await else {
        let notice = Frame::Notice {
            message: format!(
                "no order book for {symbol} arrived within {}s. The feed subscribes depth \
                 alongside trades, so this means either no market feed is configured \
                 (MARKET_FEED) or the book has not finished syncing. The last stored \
                 snapshot is at GET /orderbook?symbol={symbol}.",
                DEPTH_GRACE.as_secs()
            ),
        };
        let _ = send_text(&mut sink, &notice).await;
        let _ = sink.close().await;
        connection.closed(&channel, "no book within grace");
        return;
    };
    if send_book(&mut sink, &first).await.is_err() {
        connection.closed(&channel, "send failed");
        return;
    }
    debug!(%symbol, "order book live");

    let reason = loop {
        tokio::select! {
            received = books.recv() => match received {
                Ok(snapshot) => {
                    if send_book(&mut sink, &snapshot).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // A DOM redraws whole levels, so a dropped snapshot costs a
                    // frame of smoothness and nothing else -- but it must still
                    // be said, or a stalled ladder looks like a quiet market.
                    connection.missed(dropped);
                    let frame = Frame::Lagged { dropped };
                    if send_binary(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Closed) => break "bus closed",
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) => break "client closed",
                None => break "stream ended",
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%channel, "order book socket error: {e}");
                    break "socket error";
                }
            },
        }
    };
    connection.closed(&channel, reason);
}

/// The first book, or `None` if the grace period expired.
///
/// Lags are skipped rather than reported: a consumer that was already behind
/// before it started reading should get the *newest* book, not an apology for
/// books it never saw.
async fn first_book(
    books: &mut broadcast::Receiver<OrderBookSnapshot>,
) -> Option<OrderBookSnapshot> {
    let deadline = tokio::time::Instant::now() + DEPTH_GRACE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, books.recv()).await {
            Ok(Ok(snapshot)) => return Some(snapshot),
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) | Err(_) => return None,
        }
    }
}

/// Send one book.
///
/// What goes out is a `dom::Ladder`, not the raw snapshot: the panel needs each
/// level's cumulative size and a bar width, and both are arithmetic over market
/// data, which `docs/14` keeps in Rust. The ladder is a superset of the
/// snapshot, so a client that only wanted prices is unaffected.
async fn send_book<S>(sink: &mut S, snapshot: &OrderBookSnapshot) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let frame = Frame::Data {
        payload: serde_json::to_value(crate::dom::ladder(snapshot))
            .unwrap_or(serde_json::Value::Null),
    };
    send_binary(sink, &frame).await
}

/// `/ws/agent/{session_id}`
///
/// One question per inbound frame, progress frames while it runs, then one
/// thesis. Not token streaming, and deliberately: the answer is a
/// `submit_thesis` **tool call** rather than prose, so there are no answer
/// tokens to stream. What the socket streams instead is the work -- see
/// [`ai_agent::Progress`].
pub async fn agent(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(auth): Query<WsAuth>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Ok(user) = crate::auth::authenticate_token(&state, auth.token.as_deref()) else {
        return ApiError::unauthorized("a `?token=` query parameter is required").into_response();
    };
    if state.agent.is_none() {
        return ApiError::unavailable(
            "the agent is not configured: set AWS_BEDROCK_REGION, AWS_BEDROCK_MODEL_ID and AWS \
             credentials",
        )
        .into_response();
    }
    upgrade.on_upgrade(move |socket| agent_loop(socket, state, user, session_id))
}

async fn agent_loop(socket: WebSocket, state: AppState, user: UserContext, session_id: String) {
    let (mut sink, mut stream) = socket.split();
    // Bound for its `Drop` and now for its closing line. The agent channel has
    // no `broadcast` receiver, so there is nothing to count as a drop -- but the
    // connection itself still has to be counted, because an agent socket is the
    // longest-lived one on the platform (a user leaves the panel open) and a
    // leak there is the one that actually shows up in a connection graph.
    let connection = Connection::open(Arc::clone(&state.metrics), "agent");
    info!(%session_id, user = %user.user_id, "agent socket opened");

    let hello = Frame::Subscribed {
        channel: "/ws/agent",
        detail: serde_json::json!({ "session_id": session_id }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        connection.closed(&session_id, "hello send failed");
        return;
    }

    let reason = loop {
        let Some(message) = stream.next().await else {
            break "stream ended";
        };
        let text = match message {
            Ok(Message::Text(text)) => text,
            // A close or a broken socket ends the loop. Anything else (binary,
            // ping, pong) is not a question, and ignoring it is what keeps the
            // socket open.
            Ok(Message::Close(_)) => break "client closed",
            Err(_) => break "socket error",
            Ok(_) => continue,
        };

        // The limit applies here too: this is the same paid call as
        // `POST /agent/ask`, and a socket must not be a way around it.
        if let Err(refusal) = state
            .agent_limits
            .check(user.user_id, crate::auth::now_seconds() as f64)
        {
            let frame = Frame::Notice {
                message: format!(
                    "rate limited; try again in {} seconds",
                    refusal.retry_after_seconds
                ),
            };
            if send_text(&mut sink, &frame).await.is_err() {
                break "send failed";
            }
            continue;
        }

        let reply = match serde_json::from_str::<AgentWsRequest>(&text) {
            Ok(request) => run_agent(&state, request, &mut sink, &state.metrics).await,
            Err(e) => Some(Frame::Notice {
                message: format!("expected {{\"symbol\": \"…\", \"question\": \"…\"}}: {e}"),
            }),
        };
        // `None` means the socket died while the run was reporting progress,
        // and there is nothing left to write to.
        let Some(reply) = reply else {
            break "send failed";
        };
        if send_text(&mut sink, &reply).await.is_err() {
            break "send failed";
        }
    };
    connection.closed(&session_id, reason);
}

/// The same fields `POST /agent/ask` accepts, minus `include_trace`: a socket
/// is for watching, not for pulling a full audit payload.
#[derive(Debug, Deserialize)]
struct AgentWsRequest {
    symbol: String,
    question: String,
    skill_id: Option<String>,
    timeframes: Option<Vec<String>>,
}

/// Hands the agent's steps to a channel, so they can be written to the socket
/// while the run is still going.
///
/// A channel rather than writing to the socket directly because
/// [`ai_agent::ProgressSink::report`] is synchronous and the socket needs an
/// await. The run reports into this; the loop below drains it.
struct ProgressToChannel(tokio::sync::mpsc::UnboundedSender<ai_agent::Progress>);

impl ai_agent::ProgressSink for ProgressToChannel {
    fn report(&self, progress: ai_agent::Progress) {
        // A closed receiver means the client went away mid-run. The run itself
        // is not worth aborting over a missing audience -- it may still be
        // writing a thesis to the database -- so the step is dropped.
        let _ = self.0.send(progress);
    }
}

/// Run one question, writing progress frames as they happen.
///
/// Returns the frame to send when the run finishes, or `None` if the socket
/// died while progress was being written -- in which case the caller has
/// nothing left to send to.
///
/// The two are interleaved rather than sequenced because the alternative is a
/// minute of silence followed by everything at once, which is what the socket
/// used to do.
async fn run_agent<S>(
    state: &AppState,
    request: AgentWsRequest,
    sink: &mut S,
    metrics: &Arc<Registry>,
) -> Option<Frame<'static>>
where
    S: SinkExt<Message> + Unpin,
{
    let Some(agent) = state.agent.as_ref() else {
        return Some(Frame::Notice {
            message: "the agent is not configured".into(),
        });
    };
    let Some(db) = state.db.as_ref() else {
        return Some(Frame::Notice {
            message: "no database configured; the agent has no market data to read".into(),
        });
    };

    // Timed from here, so the number covers the whole paid run: the market
    // reads, every model turn, and every tool. A latency measured around the
    // provider call alone would miss the four database round trips that
    // `ReadingMarket` exists to make visible.
    let started = std::time::Instant::now();
    metrics.count(AGENT_REQUESTS, "Agent runs started", &Labels::none());

    let mut ask = ai_agent::AskRequest::new(&request.symbol, &request.question);
    if let Some(skill) = request.skill_id {
        ask = ask.with_skill(skill);
    }
    if let Some(timeframes) = request.timeframes {
        ask = ask.with_timeframes(timeframes);
    }

    let data = crate::market_data::DbMarketData::new(db.clone());
    let (tx, mut steps) = tokio::sync::mpsc::unbounded_channel();
    // Bound rather than inlined: the run borrows this for its whole life.
    let progress = ProgressToChannel(tx);
    let running = agent.ask_with_progress(&ask, &data, &progress);
    tokio::pin!(running);

    let answer = loop {
        tokio::select! {
            Some(step) = steps.recv() => {
                count_step(metrics, &step);
                let frame = Frame::Progress {
                    payload: serde_json::to_value(&step).unwrap_or(serde_json::Value::Null),
                };
                if send_text(sink, &frame).await.is_err() {
                    return None;
                }
            }
            result = &mut running => break result,
        }
    };

    // A step reported on the final turn can still be queued: the run finished
    // before the loop got to it. Draining first keeps the panel's last line in
    // order rather than losing it to that race.
    while let Ok(step) = steps.try_recv() {
        count_step(metrics, &step);
        let frame = Frame::Progress {
            payload: serde_json::to_value(&step).unwrap_or(serde_json::Value::Null),
        };
        if send_text(sink, &frame).await.is_err() {
            return None;
        }
    }

    metrics.observe(
        AGENT_LATENCY,
        "Agent run duration in seconds",
        &Labels::none(),
        started.elapsed().as_secs_f64(),
    );

    Some(match answer {
        // The same shape `POST /agent/ask` returns, so a client written against
        // one works against the other and the two cannot drift.
        Ok(answer) => {
            metrics.count(AGENT_THESES, "Theses the agent produced", &Labels::none());
            Frame::Data {
                payload: serde_json::to_value(crate::agent_routes::to_response(answer, false))
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "the thesis did not serialize" }),
                    ),
            }
        }
        Err(e) => {
            // Message first, because the conversion below consumes the error.
            // The label reuses `ApiError`'s mapping rather than matching the
            // variants again here: a second match is a second place to forget a
            // new variant, and the two would drift into different names for the
            // same failure.
            let message = e.to_string();
            let kind = crate::error::ApiError::from(e).code().to_string();
            metrics.count(
                AGENT_PROVIDER_ERRORS,
                "Agent runs that ended in an error",
                &Labels::new(&[("kind", &kind)]),
            );
            Frame::Notice { message }
        }
    })
}

/// Count what a single progress step says happened.
///
/// Only `Tool` is counted, not `ToolDone`: a tool that was called and a tool
/// that returned are the same event for "how much is the model reaching for the
/// market", and counting both would double every number. `ToolDone` carries
/// `ok`, which is a *failure* signal -- and that one is worth having, so it is
/// counted separately rather than folded in.
fn count_step(metrics: &Registry, step: &ai_agent::Progress) {
    match step {
        ai_agent::Progress::Tool { name } => metrics.count(
            AGENT_TOOL_CALLS,
            "Tools the agent called",
            &Labels::new(&[("tool", name)]),
        ),
        ai_agent::Progress::ToolDone { name, ok: false } => metrics.count(
            AGENT_PROVIDER_ERRORS,
            "Agent runs that ended in an error",
            &Labels::new(&[("kind", "tool_failed"), ("tool", name)]),
        ),
        _ => {}
    }
}

/// `/ws/bots/{bot_id}`
pub async fn bot(
    State(state): State<AppState>,
    Path(bot_id): Path<String>,
    Query(auth): Query<WsAuth>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Ok(user) = crate::auth::authenticate_token(&state, auth.token.as_deref()) else {
        return ApiError::unauthorized("a `?token=` query parameter is required").into_response();
    };
    let Ok(bot_id) = Uuid::parse_str(&bot_id) else {
        return ApiError::bad_request("ID_INVALID", "the bot id is not a uuid").into_response();
    };
    let Some(db) = state.db.as_ref() else {
        return ApiError::unavailable("no database configured").into_response();
    };

    // Ownership before the upgrade: a socket that opens and then closes is a
    // worse answer than a 404.
    match db::bots::get_bot(db.pool(), user.user_id, bot_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("no such bot").into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    }

    let events = state.bots.subscribe_events();
    let metrics = Arc::clone(&state.metrics);
    upgrade.on_upgrade(move |socket| bot_loop(socket, events, bot_id, metrics))
}

async fn bot_loop(
    socket: WebSocket,
    mut events: tokio::sync::broadcast::Receiver<BotEvent>,
    bot_id: Uuid,
    metrics: Arc<Registry>,
) {
    let (mut sink, mut stream) = socket.split();
    let connection = Connection::open(metrics, "bots");
    debug!(%bot_id, "bot socket opened");

    let hello = Frame::Subscribed {
        channel: "/ws/bots",
        detail: serde_json::json!({ "bot_id": bot_id }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        connection.closed(&bot_id, "hello send failed");
        return;
    }

    let reason = loop {
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => {
                    // One socket per bot, so it only gets its own events. The
                    // alternative -- a socket per user with a filter -- would
                    // fan out every bot's activity to every watcher.
                    if event.bot_id() != bot_id {
                        continue;
                    }
                    let frame = Frame::Data {
                        payload: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
                    };
                    if send_text(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    connection.missed(dropped);
                    let frame = Frame::Lagged { dropped };
                    if send_text(&mut sink, &frame).await.is_err() {
                        break "send failed";
                    }
                }
                Err(RecvError::Closed) => break "bus closed",
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) => break "client closed",
                None => break "stream ended",
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%bot_id, "bot socket error: {e}");
                    break "socket error";
                }
            },
        }
    };
    connection.closed(&bot_id, reason);
}

async fn send_text<S>(sink: &mut S, frame: &Frame<'_>) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    sink.send(Message::Text(json_frame(frame).into()))
        .await
        .map_err(|_| ())
}

async fn send_binary<S>(sink: &mut S, frame: &Frame<'_>) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    sink.send(Message::Binary(json_frame(frame).into_bytes().into()))
        .await
        .map_err(|_| ())
}

/// Log a socket that failed to upgrade, rather than dropping it silently.
#[allow(dead_code)]
fn note_upgrade_failure(channel: &str, error: &str) {
    warn!(channel, "websocket upgrade failed: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_carries_its_type_so_a_client_can_branch_on_it() {
        let frame = Frame::Subscribed {
            channel: "/ws/market",
            detail: serde_json::json!({ "symbol": "BTCUSDT" }),
        };
        let rendered: serde_json::Value = serde_json::from_str(&json_frame(&frame)).unwrap();
        assert_eq!(rendered["type"], "subscribed");
        assert_eq!(rendered["detail"]["symbol"], "BTCUSDT");
    }

    #[test]
    fn a_lag_frame_names_how_much_was_dropped() {
        // Silence would let a chart draw a gap that looks like a quiet market.
        let frame = Frame::Lagged { dropped: 7 };
        let rendered: serde_json::Value = serde_json::from_str(&json_frame(&frame)).unwrap();
        assert_eq!(rendered["type"], "lagged");
        assert_eq!(rendered["dropped"], 7);
    }

    #[test]
    fn data_frames_are_tagged_too() {
        let frame = Frame::Data {
            payload: serde_json::json!({ "close": 1.0 }),
        };
        let rendered: serde_json::Value = serde_json::from_str(&json_frame(&frame)).unwrap();
        assert_eq!(rendered["type"], "data");
        assert_eq!(rendered["payload"]["close"], 1.0);
    }

    #[test]
    fn progress_is_its_own_frame_type_and_not_data() {
        // A client that mistook a progress step for an answer would try to draw
        // a thesis out of the agent's shopping list, so the two must not share
        // a tag.
        let frame = Frame::Progress {
            payload: serde_json::to_value(ai_agent::Progress::Tool {
                name: "analyze_timeframe".into(),
            })
            .unwrap(),
        };
        let rendered: serde_json::Value = serde_json::from_str(&json_frame(&frame)).unwrap();
        assert_eq!(rendered["type"], "progress");
        assert_eq!(rendered["payload"]["stage"], "tool");
        assert_eq!(rendered["payload"]["name"], "analyze_timeframe");
    }

    #[test]
    fn the_agent_request_accepts_the_same_ladder_override_as_the_post_route() {
        // The socket and `POST /agent/ask` are two doors to one call, so a
        // client written against one must work against the other.
        let request: AgentWsRequest = serde_json::from_str(
            r#"{"symbol":"BTCUSDT","question":"long setup?","timeframes":["4h","5m"]}"#,
        )
        .expect("the post body's fields must parse here too");
        assert_eq!(request.timeframes, Some(vec!["4h".into(), "5m".into()]));
        assert_eq!(request.skill_id, None);
    }

    #[test]
    fn an_open_moves_the_gauge_and_the_counter_but_only_the_gauge_comes_back() {
        // The two numbers together are what separates churn from a leak, and
        // neither is worth much alone: a gauge that returns to zero says nothing
        // about how many times it got there. This pins the gap -- the counter
        // keeps the close, the gauge does not.
        let metrics = Arc::new(Registry::new());
        let labels = Labels::new(&[("channel", "market")]);

        let connection = Connection::open(Arc::clone(&metrics), "market");
        assert_eq!(metrics.gauge(WS_CONNECTIONS, &labels), Some(1.0));
        assert_eq!(metrics.counter(WS_OPENS, &labels), 1);

        drop(connection);
        assert_eq!(metrics.gauge(WS_CONNECTIONS, &labels), Some(0.0));
        assert_eq!(
            metrics.counter(WS_OPENS, &labels),
            1,
            "the open outlives the close, and that gap is the churn signal"
        );
    }
}
