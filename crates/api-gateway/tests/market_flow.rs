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
    //
    // Seven, not six: `1w` joined the ladder when weekly analysis became
    // end-to-end. This assertion is the only place the *route's* view of the
    // ladder is checked, and a unit test over `STANDARD_TIMEFRAMES` would not
    // have caught a route that kept sending the old list.
    assert_eq!(timeframes.len(), 7, "seven standard resolutions: {timeframes:?}");
    let offered: Vec<&str> = timeframes
        .iter()
        .filter_map(|tf| tf["timeframe"].as_str())
        .collect();
    assert_eq!(
        offered,
        vec!["1m", "5m", "15m", "1h", "4h", "1d", "1w"],
        "the ladder reads fine to coarse, and weekly is last: {timeframes:?}"
    );

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

// ---------------------------------------------------------------------------
// Symbol search and validation
//
// These routes exist because the platform's premise is that it works for any
// market, not just the one an operator configured. A search over a deployment's
// watchlist would answer "we have never heard of ETHUSDT" to a user looking at
// a venue that trades it -- wrong about a fact the platform can check.
//
// Every assertion below runs against an *empty* index, which is the state these
// tests can reach without a venue. That is deliberate: an empty index is where
// the honesty rule is easiest to break and cheapest to verify, because the lazy
// answer ("no such symbol") and the true one ("we have not looked") are
// indistinguishable in the payload unless something pins them apart.
// ---------------------------------------------------------------------------

/// An empty index must not be reported as "no such symbol".
///
/// The defect this guards against is subtle and would never look like a bug: a
/// user types a valid symbol, the index happens to be cold, and the platform
/// tells them it does not exist. The user believes it, because the platform
/// sounds certain. Both `fetched_at: null` and the `note` exist to make that
/// failure visible in the payload rather than only in the log.
#[tokio::test]
async fn an_empty_index_says_it_has_not_looked_rather_than_that_nothing_exists() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols/search?q=ETH", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The query is echoed, so a client can drop a stale answer.
    assert_eq!(body["query"], "ETH", "{body}");

    assert_eq!(body["indexed"], 0, "{body}");
    assert!(
        body["fetched_at"].is_null(),
        "an index that was never fetched must not claim a fetch time: {body}"
    );
    assert_eq!(body["stale"], true, "{body}");
    assert_eq!(
        body["results"].as_array().map(Vec::len),
        Some(0),
        "the harness never reaches a venue, so there is nothing to match: {body}"
    );

    // The note is the whole point: it separates "the venue does not offer that"
    // from "we could not check", and only the first is the user's problem.
    let note = body["note"]
        .as_str()
        .expect("an unexplained empty result is the defect this route exists to prevent");
    assert!(
        note.contains("not been fetched"),
        "the note must name the platform's own state: {note}"
    );
    assert!(
        note.contains("does not exist") || note.contains("does not"),
        "the note must explicitly deny the wrong reading: {note}"
    );
    assert!(
        note.contains("Charts still work"),
        "and it must say what still works, or the user concludes the platform is down: {note}"
    );
}

/// The echo is normalised, and a query is never required.
///
/// A client that sends a lowercase or padded symbol is not making a mistake --
/// symbols are conventionally written in caps but users type what they type.
#[tokio::test]
async fn search_normalises_its_query_and_tolerates_an_empty_one() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols/search?q=%20ethusdt%20", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["query"], "ETHUSDT",
        "padded and lowercased input is normalised, not rejected: {body}"
    );

    // No `q` at all is a listing request, not an error. It must not 400 --
    // nothing about "show me what exists" is malformed.
    let (status, body) = h.get("/symbols/search", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["query"], "", "{body}");
}

/// A missing query parameter still comes back in this API's envelope.
#[tokio::test]
async fn validation_without_a_symbol_is_a_400_in_this_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols/validate", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "QUERY_INVALID", "{body}");
}

/// With no listing, validation must say "unknown_index" -- never "unknown".
///
/// `unknown` is a verdict on the symbol. `unknown_index` is a statement about
/// the platform. Emitting the first from the second state is how a transient
/// network problem becomes a message telling the user their symbol is
/// misspelled, and it would send them editing a symbol that was correct.
#[tokio::test]
async fn an_unfetched_index_never_tells_the_user_their_symbol_is_wrong() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/symbols/validate?symbol=btcusdt", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["symbol"], "BTCUSDT", "normalised: {body}");
    assert_eq!(
        body["status"], "unknown_index",
        "an empty index cannot conclude a symbol is absent: {body}"
    );

    // Chartable even though the index is empty, because asking for a chart
    // fetches history directly and never consults the listing at all. This is
    // the assertion that keeps the two subsystems honestly decoupled.
    assert_eq!(
        body["chartable"], true,
        "a chart does not depend on the instrument listing: {body}"
    );
    assert_eq!(
        body["tradable"], false,
        "but a bot must not be started on an unverified symbol: {body}"
    );

    let note = body["note"].as_str().expect("a note");
    assert!(
        note.contains("platform") && note.contains("not a verdict"),
        "the note must attribute the uncertainty to the platform: {note}"
    );
}

// ---------------------------------------------------------------------------
// Capability and freshness reporting
//
// The endpoint exists so a user learns what a deployment cannot do *before*
// they walk into it -- otherwise the first sign is a minute-long wait followed
// by "the agent is not configured". The tests below pin the two properties that
// make it worth having: it reports capability separately from proof, and it
// reports freshness separately from capability.
// ---------------------------------------------------------------------------

/// The report describes this deployment, and does not overclaim.
#[tokio::test]
async fn the_capability_report_says_what_is_configured_without_claiming_proof() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/capabilities", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let capabilities = body["capabilities"].as_array().expect("a list");
    assert!(!capabilities.is_empty(), "{body}");

    // Every entry carries the four fields a client branches on.
    for capability in capabilities {
        assert!(capability["name"].is_string(), "{capability}");
        assert!(capability["readiness"].is_string(), "{capability}");
        assert!(capability["verified"].is_boolean(), "{capability}");
        assert!(capability["depends_on"].is_string(), "{capability}");
    }

    // The harness has a database and a signing secret and no Bedrock, so:
    let named = |name: &str| {
        capabilities
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{name} must appear in the report: {body}"))
    };

    assert_eq!(named("database")["readiness"], "ready", "{body}");
    assert_eq!(named("auth")["readiness"], "ready", "{body}");
    assert_eq!(
        named("agent")["readiness"],
        "not_configured",
        "the harness configures no model: {body}"
    );

    // The honesty rule: no capability claims to have been *exercised* on the
    // strength of its configuration. The agent in particular would otherwise
    // read as proven working, and this endpoint is what people check before
    // trusting the rest.
    assert_eq!(
        named("agent")["verified"], false,
        "a configured-but-untested capability must not claim proof: {body}"
    );
    // Market data is exercised by definition -- the platform read RAM to answer.
    assert_eq!(named("market_data")["verified"], true, "{body}");

    // And the warning is written for a user, naming what they cannot do.
    let warning = body["warning"].as_str().expect("a missing agent is worth saying");
    assert!(warning.contains("asking the AI analyst"), "{warning}");
    assert!(
        !warning.contains("AWS_BEDROCK"),
        "the variable belongs in the capability's detail field, not the warning: {warning}"
    );
}

/// Freshness is reported per symbol, and an empty buffer reports nothing.
#[tokio::test]
async fn freshness_is_reported_for_what_the_buffer_holds_and_nothing_more() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Nothing buffered: `data` must be empty rather than padded with nulls for
    // every instrument on the venue. "Not collected" and "collection stopped"
    // are different facts, and a null age cannot tell them apart.
    let (status, body) = h.get("/capabilities", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"].as_array().map(Vec::len),
        Some(0),
        "a cold buffer has no freshness to report: {body}"
    );

    // Buffer some bars and it reports their age.
    seed(&h, "BTCUSDT", "1m", 10, 60_000_000_000);
    let (status, body) = h.get("/capabilities", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let data = body["data"].as_array().expect("a list");
    let btc = data
        .iter()
        .find(|entry| entry["symbol"] == "BTCUSDT")
        .expect("BTCUSDT has bars buffered");
    assert!(btc["newest_bar_ns"].is_i64(), "{btc}");
    assert!(btc["age_seconds"].is_i64(), "{btc}");
    assert_eq!(
        btc["timeframes"].as_array().map(Vec::len),
        Some(1),
        "only the timeframe that has bars: {btc}"
    );
    // The fixture seeds bars at the epoch, so they are decades stale -- and the
    // report must say so rather than reporting a healthy feed.
    assert_eq!(
        btc["stale"], true,
        "bars from 1970 are not a live feed: {btc}"
    );
    assert!(
        body["warning"]
            .as_str()
            .is_some_and(|w| w.contains("BTCUSDT")),
        "a stale feed has to reach the warning, or the chart silently draws the past: {body}"
    );
}

/// The instrument index reports its own emptiness as the platform's state.
#[tokio::test]
async fn the_capability_report_explains_an_unfetched_instrument_index() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/capabilities", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["instruments"]["indexed"], 0, "{body}");
    assert!(
        body["instruments"]["fetched_at_ns"].is_null(),
        "an index that was never fetched must not claim a fetch time: {body}"
    );
    assert_eq!(body["instruments"]["stale"], true, "{body}");
    assert!(
        body["instruments"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("Charts are unaffected")),
        "an empty index is not a broken platform, and the report must say so: {body}"
    );

    // The feed ceiling is reported so a client can see it approaching.
    assert_eq!(
        body["feeds"]["max_active"],
        api_gateway::bots::MAX_ACTIVE_FEEDS,
        "{body}"
    );
    assert_eq!(body["feeds"]["route"], 0, "{body}");
    assert_eq!(body["feeds"]["bot"], 0, "{body}");
}

/// The capability report is public, like the rest of the platform's own state.
#[tokio::test]
async fn the_capability_report_does_not_require_a_token() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // `docs/12`: this describes the deployment, not anybody's data. Gating it
    // would mean a user has to sign in to find out that signing in is not
    // configured.
    let (status, _) = h.get("/capabilities", None).await;
    assert_eq!(status, StatusCode::OK);
}
