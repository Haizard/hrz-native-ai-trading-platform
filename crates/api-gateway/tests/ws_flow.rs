//! The WebSocket channels, over a real socket.
//!
//! ## Why these tests open a TCP connection
//!
//! `tower::oneshot` drives a request through the router, which is enough for
//! every REST route -- but it cannot upgrade a connection. A WebSocket test that
//! never performs a handshake tests the handler's argument list and nothing
//! about the channel. So these spawn the real router on a real port and connect
//! with a real client.
//!
//! ## The seam is the same one the collector uses
//!
//! Candles are published with `BotSupervisor::feed_candle` and books with
//! `feed_orderbook` -- exactly what the live Binance feed calls. So both market
//! channels are exercised on the real path, with no socket to an exchange.

mod common;

use std::time::Duration;

use analytics_core::types::{Candle, OrderBookLevel, OrderBookSnapshot, Timeframe};
use common::Harness;
use futures::{SinkExt, StreamExt};
use observability::metrics::{Labels, WS_CONNECTIONS, WS_OPENS};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// Connect, and read frames until one satisfies `until` or the deadline passes.
async fn read_until<F>(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut until: F,
) -> Option<serde_json::Value>
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        // Wait out the *whole* remaining budget, not a fixed slice of it: a
        // channel that speaks after a grace period (the order book does, at
        // five seconds) must not be missed because this read happened to end
        // first.
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(remaining, socket.next()).await;
        let Ok(Some(Ok(message))) = next else {
            return None;
        };
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            _ => continue,
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if until(&value) {
            return Some(value);
        }
    }
    None
}

async fn connect(
    base: &str,
    path: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{base}{path}"))
        .await
        .expect("the handshake must succeed");
    socket
}

fn candle(open_time: i64, close: f64, timeframe: Timeframe) -> Candle {
    Candle {
        symbol: "BTCUSDT".into(),
        timeframe,
        open_time,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1.0,
        buy_volume: 0.6,
        sell_volume: 0.4,
    }
}

#[tokio::test]
async fn the_market_channel_says_what_it_is_watching_then_forwards_candles() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/market/BTCUSDT/5m").await;

    let hello = read_until(&mut socket, |v| v["type"] == "subscribed")
        .await
        .expect("a subscribed frame on connect");
    assert_eq!(hello["channel"], "/ws/market/BTCUSDT/5m");
    assert_eq!(hello["detail"]["symbol"], "BTCUSDT");

    // The same call the live collector makes.
    h.supervisor
        .feed_candle(&candle(1_000_000, 100.0, Timeframe::M5));

    let data = read_until(&mut socket, |v| v["type"] == "data")
        .await
        .expect("the candle must arrive");
    assert_eq!(data["payload"]["close"], 100.0);
    assert_eq!(data["payload"]["symbol"], "BTCUSDT");
}

#[tokio::test]
async fn the_market_channel_only_forwards_the_subscribed_resolution() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/market/BTCUSDT/5m").await;
    read_until(&mut socket, |v| v["type"] == "subscribed").await;

    // The feed builds several resolutions; a 5m chart has no use for the 1h
    // series, and forwarding it would make the chart draw the wrong bars.
    h.supervisor
        .feed_candle(&candle(2_000_000, 999.0, Timeframe::H1));
    h.supervisor
        .feed_candle(&candle(3_000_000, 101.0, Timeframe::M5));

    let data = read_until(&mut socket, |v| v["type"] == "data")
        .await
        .expect("a candle must arrive");
    assert_eq!(
        data["payload"]["close"], 101.0,
        "the 1h candle must have been filtered out"
    );
}

/// The notice is about the gateway's own feed, not about the channel's contract.
///
/// Written because the first version of that notice **closed the socket**, which
/// reads as a sensible way to make the problem obvious and is not: `FeedMode::Off`
/// means this gateway will not open a feed, not that no candle will ever be
/// published -- `feed_candle` publishes into the same bus from outside the feed.
/// Closing refused data that exists, and it put the resolution filter in
/// `market_loop` beyond the reach of the two tests above, which are the only
/// witnesses it has. A property that stops being observable is worse than one
/// that is missing, because the suite still reads as coverage.
#[tokio::test]
async fn a_channel_that_has_been_told_there_is_no_feed_still_forwards_a_candle() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/market/BTCUSDT/5m").await;
    read_until(&mut socket, |v| v["type"] == "subscribed").await;

    // The harness runs with `MARKET_FEED` unset, so the channel explains itself.
    let notice = read_until(&mut socket, |v| v["type"] == "notice")
        .await
        .expect("the channel must explain its silence");
    assert!(
        notice["message"]
            .as_str()
            .unwrap_or_default()
            .contains("MARKET_FEED"),
        "it must name the likely cause: {notice}"
    );

    // And it must still be a channel. Something outside the feed publishes a
    // candle, and it has to reach the client.
    h.supervisor
        .feed_candle(&candle(4_000_000, 102.0, Timeframe::M5));

    let data = read_until(&mut socket, |v| v["type"] == "data")
        .await
        .expect("a candle published by something else must still arrive");
    assert_eq!(data["payload"]["close"], 102.0);
}

#[tokio::test]
async fn a_market_socket_closes_cleanly() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/market/BTCUSDT/5m").await;
    read_until(&mut socket, |v| v["type"] == "subscribed").await;

    // A close must not leave the task spinning.
    socket.send(Message::Close(None)).await.ok();
    let closed = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    assert!(closed.is_ok(), "the socket should close, not hang");
}

fn book(timestamp: i64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        symbol: "BTCUSDT".into(),
        timestamp,
        bids: vec![OrderBookLevel {
            price: 100.0,
            quantity: 2.0,
        }],
        asks: vec![OrderBookLevel {
            price: 101.0,
            quantity: 3.0,
        }],
    }
}

#[tokio::test]
async fn the_orderbook_channel_says_what_it_is_watching_then_forwards_the_book() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/orderbook/BTCUSDT").await;

    let hello = read_until(&mut socket, |v| v["type"] == "subscribed")
        .await
        .expect("a subscribed frame on connect");
    assert_eq!(hello["channel"], "/ws/orderbook/BTCUSDT");
    assert_eq!(hello["detail"]["symbol"], "BTCUSDT");

    // The same call the live collector makes.
    h.supervisor.feed_orderbook(&book(1_000_000));

    let data = read_until(&mut socket, |v| v["type"] == "data")
        .await
        .expect("the book must arrive");
    assert_eq!(data["payload"]["symbol"], "BTCUSDT");
    assert_eq!(data["payload"]["bids"][0]["price"], 100.0);
    assert_eq!(data["payload"]["asks"][0]["quantity"], 3.0);

    // A ladder, not a bare snapshot: the panel gets each level's running
    // total and a bar width, so it never sums market data itself.
    assert_eq!(data["payload"]["bids"][0]["cumulative"], 2.0);
    assert_eq!(data["payload"]["asks"][0]["cumulative"], 3.0);
    // Asks hold more, so the ask side is the deepest and the bid bar is
    // shorter -- scaled against one denominator, not one per side.
    assert_eq!(data["payload"]["asks"][0]["bar_pct"], 100.0);
    assert!(
        data["payload"]["bids"][0]["bar_pct"].as_f64().unwrap() < 100.0,
        "a thinner side must not draw a full bar: {data}"
    );
}

#[tokio::test]
async fn an_orderbook_channel_with_no_book_says_so_rather_than_streaming_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/orderbook/BTCUSDT").await;
    read_until(&mut socket, |v| v["type"] == "subscribed").await;

    // No market feed is configured in the harness, so no book will ever
    // arrive. An empty ladder is indistinguishable from a market with no
    // liquidity, so the channel has to say which of the two it is -- and a
    // socket that simply never sends looks like a broken client.
    let notice = read_until(&mut socket, |v| v["type"] == "notice")
        .await
        .expect("the channel must explain itself");
    let message = notice["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("BTCUSDT"),
        "it must name the symbol: {message}"
    );
    assert!(
        message.contains("MARKET_FEED"),
        "it must name the likely cause: {message}"
    );
}

#[tokio::test]
async fn the_agent_and_bot_channels_need_a_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;

    // A browser cannot set a header on a handshake, so the token goes in the
    // query string -- and its absence must be refused, not ignored.
    for path in [
        "/ws/agent/session-1",
        "/ws/bots/00000000-0000-0000-0000-000000000000",
    ] {
        let error = tokio_tungstenite::connect_async(format!("ws://{base}{path}"))
            .await
            .expect_err(&format!("{path} must be refused"));
        let rendered = error.to_string();
        assert!(
            rendered.contains("401") || rendered.contains("Unauthorized"),
            "{path}: expected a 401, got {rendered}"
        );
    }
}

#[tokio::test]
async fn a_bad_token_is_refused_on_the_socket_too() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;

    let error = tokio_tungstenite::connect_async(format!(
        "ws://{base}/ws/agent/session-1?token=not-a-real-token"
    ))
    .await
    .expect_err("a forged token must be refused");
    let rendered = error.to_string();
    assert!(
        rendered.contains("401") || rendered.contains("Unauthorized"),
        "expected a 401, got {rendered}"
    );
}

#[tokio::test]
async fn the_agent_endpoints_are_authenticated_and_rate_limited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = h.register().await;

    // Unauthenticated: refused before anything else happens.
    let (status, body) = h
        .post(
            "/agent/ask",
            json!({ "symbol": "BTCUSDT", "question": "hi" }),
            None,
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    // Authenticated, with no Bedrock configured: the capability is missing, so
    // 503 -- but the limit is spent first, because it is the cheaper check and
    // it is the one that protects the paid path.
    let limit = h.limits.limit();
    let burst = limit.burst as usize;
    for i in 0..burst {
        let (status, body) = h
            .post(
                "/agent/ask",
                json!({ "symbol": "BTCUSDT", "question": "hi" }),
                Some(&user.token),
            )
            .await;
        assert_eq!(
            status,
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "request {i} should have reached the missing-agent check: {body}"
        );
    }

    // And the next one is refused by the limiter.
    let (status, body) = h
        .post(
            "/agent/ask",
            json!({ "symbol": "BTCUSDT", "question": "hi" }),
            Some(&user.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
    assert!(
        body["error"]["details"]["retry_after_seconds"].is_null()
            || body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Try again in"),
        "the refusal must say when to retry: {body}"
    );

    // Another user is unaffected, which is the point of a per-user limit.
    let other = h.register().await;
    let (status, _) = h
        .post(
            "/agent/ask",
            json!({ "symbol": "BTCUSDT", "question": "hi" }),
            Some(&other.token),
        )
        .await;
    assert_ne!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);

    user.cleanup(&h.database).await;
    other.cleanup(&h.database).await;
}

/// A socket that opens and then closes must leave a trace in **both** numbers.
///
/// ## Why this is a test over a real socket
///
/// The unit test covers the guard arithmetic; this covers the thing the guard
/// was added for -- that a client really connecting, really closing, and really
/// being reaped moves the gauge back and leaves the counter where it was. That
/// gap is the entire churn signal: a gauge alone would show `1` and then `0`
/// again, which is indistinguishable from a socket that was never opened.
///
/// It is the regression witness for a log that showed seven "socket opened"
/// lines in seven minutes with no closes beside them, because closes were
/// logged at `debug` and every early `return` skipped them entirely.
#[tokio::test]
async fn a_closed_socket_returns_the_gauge_but_not_the_counter() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let labels = Labels::new(&[("channel", "market")]);

    let mut socket = connect(&base, "/ws/market/BTCUSDT/5m").await;
    read_until(&mut socket, |v| v["type"] == "subscribed")
        .await
        .expect("a subscribed frame on connect");

    assert_eq!(
        h.metrics.gauge(WS_CONNECTIONS, &labels),
        Some(1.0),
        "one socket is open"
    );
    assert_eq!(h.metrics.counter(WS_OPENS, &labels), 1, "and one opened");

    let _ = socket.close(None).await;

    // The close has to travel to the server and the task has to be reaped, so
    // this waits rather than assuming -- but it waits on a deadline, because a
    // test that hangs is worse than one that fails.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut released = false;
    while tokio::time::Instant::now() < deadline {
        if h.metrics.gauge(WS_CONNECTIONS, &labels) == Some(0.0) {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(released, "the connection must be released on close");

    assert_eq!(
        h.metrics.counter(WS_OPENS, &labels),
        1,
        "the open outlives the close: that gap is what makes churn visible"
    );
}

/// The whole point of the chart lane: a bar that is still forming reaches a
/// subscribed chart without waiting for its bucket to close.
///
/// Written against the defect this lane exists to fix -- before it, the market
/// channel forwarded only closed candles, so a `1d` chart received one frame
/// per day and sat frozen the rest of the time, reading as a broken feed.
/// `feed_forming_candle` is the recorder's once-a-second snapshot path; a bot
/// never takes it, because bots subscribe the closed lane.
#[tokio::test]
async fn the_market_channel_forwards_a_forming_candle_to_the_chart() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let base = h.serve().await;
    let mut socket = connect(&base, "/ws/market/BTCUSDT/1d").await;
    read_until(&mut socket, |v| v["type"] == "subscribed").await;

    // Today's bar, still open: published straight onto the chart lane, the
    // same publish the recorder's one-second tick makes.
    h.supervisor
        .feed_forming_candle(&candle(4_000_000, 105.5, Timeframe::D1));

    let data = read_until(&mut socket, |v| v["type"] == "data")
        .await
        .expect("the forming candle must reach the chart");
    assert_eq!(data["payload"]["close"], 105.5);
    assert_eq!(data["payload"]["timeframe"], "1d");
}

/// A forming bar must never reach a *bot*: strategies subscribe the closed
/// lane, and a decision on a bar that has not closed would trade a price that
/// never existed as a close.
#[tokio::test]
async fn a_forming_candle_stays_off_the_lane_bots_subscribe() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mut closed = h.supervisor.subscribe_candles("BTCUSDT");

    h.supervisor
        .feed_forming_candle(&candle(5_000_000, 106.0, Timeframe::M5));

    // The forming frame has had every chance to arrive (the publish is
    // synchronous); an empty lane is the assertion.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        closed.try_recv().is_err(),
        "a forming candle reached the closed-candle lane"
    );
}
