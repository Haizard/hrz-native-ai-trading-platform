//! `/symbols`, `/orderbook`, and the error envelope on rejections.
//!
//! ## What only a route test can show
//!
//! That the inventory reports the *span* and not just a count; that a missing
//! query parameter comes back in this API's envelope rather than axum's plain
//! text; and that a symbol with no depth says so instead of returning an empty
//! book that reads as a market with no liquidity.
//!
//! These routes are read-only, so unlike the bot tests there is nothing to
//! clean up.

mod common;

use axum::http::StatusCode;
use common::Harness;
use serde_json::json;

/// Bars for the tests that need the buffer to have something in it.
///
/// The buffer is RAM, so it starts empty on every test -- which is the whole
/// point of it, and the reason a route test cannot assume a seeded database
/// any more.
fn seed(h: &Harness, symbol: &str, timeframe: &str, count: i64, step_ns: i64) {
    let history = h.supervisor.history();
    for i in 0..count {
        history.record_closed(&analytics_core::Candle {
            symbol: symbol.to_string(),
            timeframe: timeframe.parse().expect("a known resolution"),
            open_time: i * step_ns,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        });
    }
}

#[tokio::test]
async fn the_inventory_reports_what_the_buffer_holds() {
    let Some(h) = Harness::new().await else {
        return;
    };
    seed(&h, "BTCUSDT", "1m", 10, 60_000_000_000);
    seed(&h, "BTCUSDT", "5m", 10, 300_000_000_000);

    let (status, body) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let symbols = body.as_array().expect("a list");
    let btc = symbols
        .iter()
        .find(|s| s["symbol"] == "BTCUSDT")
        .expect("BTCUSDT is on the watchlist");
    let timeframes = btc["timeframes"].as_array().expect("a list");

    // The whole standard ladder is offered, buffered or not -- a client cannot
    // ask for a resolution it was never told about.
    assert_eq!(timeframes.len(), 6, "six standard resolutions: {timeframes:?}");

    let buffered: Vec<_> = timeframes
        .iter()
        .filter(|tf| tf["candles"].as_i64().unwrap_or(0) > 0)
        .collect();
    assert_eq!(buffered.len(), 2, "two resolutions have bars buffered");

    for tf in buffered {
        assert_eq!(tf["candles"].as_i64().unwrap_or(0), 10, "{tf}");
        let first = tf["first"].as_i64().expect("a first timestamp");
        let last = tf["last"].as_i64().expect("a last timestamp");
        // The span, not just the count: "1,110 candles" is reassuring and says
        // nothing, and the whole reason this route exists is that the span is
        // what bites.
        assert!(first <= last, "{tf}");
        assert!(
            tf["missing"].as_i64().is_some(),
            "a buffered resolution must report its gaps: {tf}"
        );
    }
}

/// The deadlock this route has to break: the buffer is empty on a cold start, so
/// a `/symbols` that listed only what it had buffered would answer `[]`, and a
/// page with no instruments can never ask for the one that fills the buffer.
#[tokio::test]
async fn a_cold_start_still_offers_something_to_chart() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let symbols = body.as_array().expect("a list");
    assert!(
        !symbols.is_empty(),
        "the watchlist is offered even with nothing buffered"
    );

    let btc = symbols
        .iter()
        .find(|s| s["symbol"] == "BTCUSDT")
        .expect("BTCUSDT is the default watchlist");
    assert!(!btc["timeframes"].as_array().expect("a list").is_empty());

    // And it says that zero buffered does not mean unavailable.
    let note = btc["coverage_note"].as_str().unwrap_or_default();
    assert!(
        note.contains("chartable"),
        "zero buffered must not read as unavailable: {note}"
    );
}

/// The cold-start note has to fire in the state a real boot is actually in,
/// which is **not** the state the test above sets up.
///
/// `run_binance_feed` records the *forming* bar as soon as the feed starts, and
/// `record_forming` registers the series -- so `history.timeframes()` answers all
/// six resolutions within a second of boot while every one of them holds zero
/// closed bars. Gating the note on "no series are registered" therefore made it
/// unreachable on a live boot, and `a_cold_start_still_offers_something_to_chart`
/// could not see that, because it seeds nothing at all and so never reaches the
/// state the server is really in.
#[tokio::test]
async fn a_cold_start_with_a_forming_bar_still_says_the_symbol_is_chartable() {
    let Some(h) = Harness::new().await else {
        return;
    };

    h.supervisor.history().record_forming(&analytics_core::Candle {
        symbol: "BTCUSDT".into(),
        timeframe: "1m".parse().expect("a known resolution"),
        open_time: 1_700_000_000_000_000_000,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        buy_volume: 6.0,
        sell_volume: 4.0,
    });

    let (status, body) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let btc = body
        .as_array()
        .expect("a list")
        .iter()
        .find(|s| s["symbol"] == "BTCUSDT")
        .expect("BTCUSDT is on the watchlist");

    let note = btc["coverage_note"].as_str().unwrap_or_default();
    assert!(
        note.contains("chartable"),
        "a forming bar is not buffered history, and the note is the only thing \
         telling a client that zero bars does not mean unavailable: {note}"
    );

    // And it stops saying so once a real bar closes -- otherwise it is a banner
    // that never goes away, which is the other way to make a warning useless.
    seed(&h, "BTCUSDT", "1m", 1, 60_000_000_000);
    let (_, body) = h.get("/symbols", None).await;
    let btc = body
        .as_array()
        .expect("a list")
        .iter()
        .find(|s| s["symbol"] == "BTCUSDT")
        .expect("BTCUSDT is on the watchlist");
    assert!(
        btc["coverage_note"].is_null(),
        "the note is about having nothing buffered, so it must clear: {btc}"
    );
}

/// The point of the whole design: a chart is served from RAM, with no database
/// read and no venue call.
#[tokio::test]
async fn a_window_the_buffer_covers_costs_nothing_to_serve() {
    let Some(h) = Harness::new().await else {
        return;
    };
    seed(&h, "BTCUSDT", "1m", 20, 60_000_000_000);

    let (status, body) = h
        .get("/candles?symbol=BTCUSDT&timeframe=1m&from=0&to=1200000000000", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let candles = body["candles"].as_array().expect("a list");
    assert_eq!(candles.len(), 20, "twenty 1m bars in a 20-minute window");
    assert_eq!(body["source"]["memory"], 20, "every one of them from RAM");
    assert_eq!(body["source"]["venue"], 0, "and none from the venue");
}

/// A window reaching further back than the buffer is served by the venue, and
/// the response says which part came from where.
///
/// The venue client in the harness points at a dead port, so the fetched part
/// is empty here -- this asserts the *shape* of the merge, not that Binance is
/// up. What it proves is that the route does not fail and does not lose the
/// buffered bars just because the deeper page could not be fetched.
#[tokio::test]
async fn a_window_older_than_the_buffer_still_returns_what_ram_has() {
    let Some(h) = Harness::new().await else {
        return;
    };
    seed(&h, "BTCUSDT", "1m", 5, 60_000_000_000);

    // Starting well before the buffer's first bar, so the gap has to be asked
    // for -- and the ask fails.
    let (status, body) = h
        .get(
            "/candles?symbol=BTCUSDT&timeframe=1m&from=-600000000000&to=600000000000",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let candles = body["candles"].as_array().expect("a list");
    assert_eq!(candles.len(), 5, "the buffered bars survive a failed fetch");
    assert_eq!(body["source"]["memory"], 5);
    assert_eq!(body["source"]["venue"], 0, "the venue refused");
}

#[tokio::test]
async fn a_symbol_with_no_depth_says_so_rather_than_returning_an_empty_book() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/orderbook?symbol=BTCUSDT", None).await;
    match status {
        StatusCode::OK => {
            // If depth has been collected, the contract is a book with both
            // sides and a spread.
            assert!(!body["bids"].as_array().expect("bids").is_empty(), "{body}");
            assert!(!body["asks"].as_array().expect("asks").is_empty(), "{body}");
            assert!(body["spread"].as_f64().is_some(), "{body}");
        }
        StatusCode::NOT_FOUND => {
            // An empty book would read as "no liquidity", which is a different
            // and much more alarming claim than "no book has arrived yet".
            assert_eq!(body["error"]["code"], "NO_ORDERBOOK_DATA", "{body}");
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("feed"),
                "the message must name the live feed, not a backfill tool: {body}"
            );
        }
        other => panic!("unexpected status {other}: {body}"),
    }
}

/// The book is read from memory, not from a table nothing writes.
#[tokio::test]
async fn a_book_that_arrived_is_served_without_touching_the_database() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.supervisor.live().record_book(&analytics_core::OrderBookSnapshot {
        symbol: "BTCUSDT".into(),
        timestamp: 1_700_000_000_000_000_000,
        bids: vec![analytics_core::OrderBookLevel {
            price: 100.0,
            quantity: 1.0,
        }],
        asks: vec![analytics_core::OrderBookLevel {
            price: 101.0,
            quantity: 2.0,
        }],
    });

    let (status, body) = h.get("/orderbook?symbol=BTCUSDT", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["bids"].as_array().expect("bids").len(), 1, "{body}");
    assert_eq!(body["asks"].as_array().expect("asks").len(), 1, "{body}");
    assert_eq!(body["spread"].as_f64(), Some(1.0), "{body}");
}

/// `docs/19` row 24 closed: a book that never arrives used to be invisible --
/// the DOM was empty, `/orderbook` answered 404, and there was no number
/// anywhere for an alert to fire on. This is the number.
#[tokio::test]
async fn a_book_that_never_arrived_is_a_number_a_rule_can_fire_on() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // A feed started for this symbol and no book ever came. Nothing is
    // recorded -- that is the case being measured.
    h.supervisor.live().expect_book("BTCUSDT", 0);
    api_gateway::metrics::publish_book_ages(&h.supervisor, &h.metrics, 90_000_000_000);

    let rendered = h.metrics.render();
    assert!(
        rendered.contains(r#"market_data_book_age_seconds{symbol="BTCUSDT"} 90"#),
        "the writer for `Rule::StaleBook` must publish the age of a book that \
         never arrived, or the rule is inert:\n{rendered}"
    );

    // Once one does arrive the age drops, so the rule clears.
    h.supervisor.live().record_book(&analytics_core::OrderBookSnapshot {
        symbol: "BTCUSDT".into(),
        timestamp: 90_000_000_000,
        bids: vec![],
        asks: vec![],
    });
    api_gateway::metrics::publish_book_ages(&h.supervisor, &h.metrics, 95_000_000_000);
    assert!(
        h.metrics
            .render()
            .contains(r#"market_data_book_age_seconds{symbol="BTCUSDT"} 5"#),
        "a book that arrives must reset the age, or a healthy symbol pages forever"
    );
}

/// A footprint is the one chart with a hard limit under this design, and the
/// route has to say what the limit is rather than returning an empty ladder.
#[tokio::test]
async fn a_footprint_says_what_window_the_tape_actually_covers() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Nothing on the tape: 404, and the message names the reason.
    let (status, body) = h.get("/footprint/coverage?symbol=BTCUSDT", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "NO_TICK_DATA", "{body}");

    // Put trades on the tape and it answers with their span.
    let minute = 60_000_000_000i64;
    let live = h.supervisor.live();
    for i in 0..10 {
        live.record_trade(&analytics_core::Trade {
            symbol: "BTCUSDT".into(),
            trade_id: i as u64,
            price: 100.0 + i as f64,
            quantity: 1.0,
            is_buyer_maker: false,
            timestamp: 1_700_000_000_000_000_000 + i * minute,
        });
    }

    let (status, body) = h.get("/footprint/coverage?symbol=BTCUSDT", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["trades"], 10, "{body}");
    assert_eq!(body["minutes"], 9, "{body}");
}

#[tokio::test]
async fn a_missing_query_parameter_comes_back_in_this_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Axum's own rejection is plain text, which a client cannot branch on.
    let (status, body) = h.get("/orderbook", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "QUERY_INVALID");
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn a_malformed_body_comes_back_in_this_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Wrong shape: JSON, but not the shape asked for.
    let (status, body) = h.post("/auth/login", json!({ "email": 42 }), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "BODY_INVALID");

    // Not JSON at all: a syntax error, which is a 400 rather than a 422.
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/auth/login")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("not json"))
        .expect("request");
    let response = tower::ServiceExt::oneshot(h.app.clone(), request)
        .await
        .expect("the router must answer");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_market_routes_are_public() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // docs/12: market data is not user data, so anonymous chart viewing is
    // allowed. Everything that belongs to somebody is not.
    let (status, _) = h.get("/symbols", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .get("/candles?symbol=BTCUSDT&timeframe=5m&limit=5", None)
        .await;
    assert_eq!(status, StatusCode::OK);
}
