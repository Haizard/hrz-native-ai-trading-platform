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

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
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
    upgrade.on_upgrade(move |socket| market_loop(socket, candles, channel, symbol, timeframe))
}

async fn market_loop(
    socket: WebSocket,
    mut candles: tokio::sync::broadcast::Receiver<Candle>,
    channel: String,
    symbol: String,
    timeframe: String,
) {
    let (mut sink, mut stream) = socket.split();
    info!(%channel, "market socket opened");

    let hello = Frame::Subscribed {
        channel: &channel,
        detail: serde_json::json!({ "symbol": symbol, "timeframe": timeframe }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        return;
    }

    loop {
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
                        break;
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // Tell the client rather than letting the chart draw a gap
                    // that looks like a quiet market.
                    let frame = Frame::Lagged { dropped };
                    if send_binary(&mut sink, &frame).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = stream.next() => match incoming {
                // A market channel is one-way; the only thing worth reading is
                // a close.
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%channel, "market socket error: {e}");
                    break;
                }
            },
        }
    }
    debug!(%channel, "market socket closed");
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
    upgrade.on_upgrade(move |socket| orderbook_loop(socket, books, channel, symbol))
}

async fn orderbook_loop(
    socket: WebSocket,
    mut books: broadcast::Receiver<OrderBookSnapshot>,
    channel: String,
    symbol: String,
) {
    let (mut sink, mut stream) = socket.split();
    info!(%channel, "order book socket opened");

    let hello = Frame::Subscribed {
        channel: &channel,
        detail: serde_json::json!({ "symbol": symbol }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
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
        return;
    };
    if send_book(&mut sink, &first).await.is_err() {
        return;
    }
    debug!(%symbol, "order book live");

    loop {
        tokio::select! {
            received = books.recv() => match received {
                Ok(snapshot) => {
                    if send_book(&mut sink, &snapshot).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // A DOM redraws whole levels, so a dropped snapshot costs a
                    // frame of smoothness and nothing else -- but it must still
                    // be said, or a stalled ladder looks like a quiet market.
                    let frame = Frame::Lagged { dropped };
                    if send_binary(&mut sink, &frame).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%channel, "order book socket error: {e}");
                    break;
                }
            },
        }
    }
    debug!(%channel, "order book socket closed");
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

async fn send_book<S>(sink: &mut S, snapshot: &OrderBookSnapshot) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let frame = Frame::Data {
        payload: serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null),
    };
    send_binary(sink, &frame).await
}

/// `/ws/agent/{session_id}`
///
/// One question per inbound frame, one thesis per outbound frame. Not token
/// streaming: that needs a streaming model call, which is a change to the
/// provider rather than to this handler.
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
    info!(%session_id, user = %user.user_id, "agent socket opened");

    let hello = Frame::Subscribed {
        channel: "/ws/agent",
        detail: serde_json::json!({ "session_id": session_id }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        return;
    }

    while let Some(message) = stream.next().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            // A close or a broken socket ends the loop. Anything else (binary,
            // ping, pong) is not a question, and ignoring it is what keeps the
            // socket open.
            Ok(Message::Close(_)) | Err(_) => break,
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
                break;
            }
            continue;
        }

        let reply = match serde_json::from_str::<AgentWsRequest>(&text) {
            Ok(request) => run_agent(&state, request).await,
            Err(e) => Frame::Notice {
                message: format!("expected {{\"symbol\": \"…\", \"question\": \"…\"}}: {e}"),
            },
        };
        if send_text(&mut sink, &reply).await.is_err() {
            break;
        }
    }
    debug!(%session_id, "agent socket closed");
}

#[derive(Debug, Deserialize)]
struct AgentWsRequest {
    symbol: String,
    question: String,
    skill_id: Option<String>,
}

async fn run_agent(state: &AppState, request: AgentWsRequest) -> Frame<'static> {
    let Some(agent) = state.agent.as_ref() else {
        return Frame::Notice {
            message: "the agent is not configured".into(),
        };
    };
    let Some(db) = state.db.as_ref() else {
        return Frame::Notice {
            message: "no database configured; the agent has no market data to read".into(),
        };
    };

    let mut ask = ai_agent::AskRequest::new(&request.symbol, &request.question);
    if let Some(skill) = request.skill_id {
        ask = ask.with_skill(skill);
    }

    match agent
        .ask(&ask, &crate::market_data::DbMarketData::new(db.clone()))
        .await
    {
        // The same shape `POST /agent/ask` returns, so a client written against
        // one works against the other and the two cannot drift.
        Ok(answer) => Frame::Data {
            payload: serde_json::to_value(crate::agent_routes::to_response(answer, false))
                .unwrap_or_else(|_| serde_json::json!({ "error": "the thesis did not serialize" })),
        },
        Err(e) => Frame::Notice {
            message: e.to_string(),
        },
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
    upgrade.on_upgrade(move |socket| bot_loop(socket, events, bot_id))
}

async fn bot_loop(
    socket: WebSocket,
    mut events: tokio::sync::broadcast::Receiver<BotEvent>,
    bot_id: Uuid,
) {
    let (mut sink, mut stream) = socket.split();
    debug!(%bot_id, "bot socket opened");

    let hello = Frame::Subscribed {
        channel: "/ws/bots",
        detail: serde_json::json!({ "bot_id": bot_id }),
    };
    if send_text(&mut sink, &hello).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => {
                    // One socket per bot, so it only gets its own events. The
                    // alternative -- a socket per user with a filter -- would
                    // fan out every bot's activity to every watcher.
                    let mine = match &event {
                        BotEvent::Started { bot_id: id, .. }
                        | BotEvent::Decision { bot_id: id, .. }
                        | BotEvent::Stopped { bot_id: id, .. } => *id == bot_id,
                    };
                    if !mine {
                        continue;
                    }
                    let frame = Frame::Data {
                        payload: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
                    };
                    if send_text(&mut sink, &frame).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    let frame = Frame::Lagged { dropped };
                    if send_text(&mut sink, &frame).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(%bot_id, "bot socket error: {e}");
                    break;
                }
            },
        }
    }
    debug!(%bot_id, "bot socket closed");
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
}
