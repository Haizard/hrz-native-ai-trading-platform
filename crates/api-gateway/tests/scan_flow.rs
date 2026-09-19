//! `GET /scan` end to end, against the real router.
//!
//! ## Why this cannot be a unit test
//!
//! The scanner's own tests prove the arithmetic and the permit logic, and
//! `scan_routes`' unit tests prove the list parsing and the wire shape. Neither
//! can prove the route is **wired into the router** -- a helper that is correct
//! and unreachable passes both. That is the defect this repo keeps finding: a
//! thing that exists on paper and cannot fire.
//!
//! ## No test reaches a venue
//!
//! `Harness` builds its `WindowService` over `http://127.0.0.1:1`, which refuses
//! instantly. Symbols are seated in RAM through `record_closed`, which is the
//! same buffer the bots' feed fills in production -- so a scan here reads
//! exactly the path a scan in production reads, with the venue replaced by a
//! buffer that already holds the bars.

mod common;

use axum::http::StatusCode;
use common::Harness;

/// Seat `count` closed bars for `symbol` at `timeframe`, ending at the current
/// hour, so the scan's `[now - 300 bars, now]` window actually contains them.
///
/// ## Why the bars are anchored to now
///
/// The first version of this file seated bars at `i * step` starting from the
/// epoch -- which put them in January 1970. The scan asks for a window ending
/// *now*, `WindowService` filters RAM by `range(from_ns, to_ns)` in absolute
/// time, so not one of those bars was inside the window and every symbol came
/// back "the venue returned no bars". The scanner was right; the fixture was
/// building a market that happened 56 years ago.
/// `closes` is a slice rather than one flat price because RSI is a claim about
/// a *sequence*: a series that only ever rises has RSI 100 and one that only
/// falls has RSI 0, and a test that used a constant price would be asserting
/// about a number that came out of a numerically degenerate input.
fn seat(h: &Harness, symbol: &str, timeframe: &str, closes: &[f64]) {
    let parsed: analytics_core::types::Timeframe = timeframe.parse().expect("a known resolution");
    let step_ns = parsed.nanos();
    let history = h.supervisor.history();

    // The newest bar opens at the start of the current bucket, so the series
    // ends "now" rather than at some fixed instant the test would have to
    // remember to update. `site`'s clock rather than a local `SystemTime` call:
    // the route reads the same one, and two clocks in one test is two chances
    // for the fixture to be an hour off the window it is meant to fill.
    let now_ns = market_data::symbols::now_ns();
    let newest_open = (now_ns / step_ns) * step_ns;
    let count = i64::try_from(closes.len()).expect("a bar count");

    for (i, close) in closes.iter().enumerate() {
        let i = i64::try_from(i).expect("a bar index");
        let open_time = newest_open - (count - 1 - i) * step_ns;
        history.record_closed(&analytics_core::Candle {
            symbol: symbol.to_string(),
            timeframe: parsed,
            open_time,
            open: *close,
            high: *close + 1.0,
            low: *close - 1.0,
            close: *close,
            volume: 10.0,
            buy_volume: 6.0,
            sell_volume: 4.0,
        });
    }
}

/// A series that rises, so RSI sits at the top of its range.
fn rising(count: usize, start: f64) -> Vec<f64> {
    (0..count).map(|i| start + (i as f64)).collect()
}

/// A series that falls, so RSI sits at the bottom of its range.
fn falling(count: usize, start: f64) -> Vec<f64> {
    (0..count).map(|i| start - (i as f64) * 0.05).collect()
}

#[tokio::test]
async fn the_scan_route_answers_and_ranks_what_was_named() {
    let Some(h) = Harness::new().await else {
        return;
    };

    // Bars are recorded under the symbol the *scan* will ask for, at the
    // timeframe the scan defaults to. A mismatch here is the whole test failing
    // for a reason that has nothing to do with the scanner.
    seat(&h, "UPUSDT", "1h", &rising(60, 100.0));
    seat(&h, "DOWNUSDT", "1h", &falling(60, 100.0));

    let (status, body) = h
        .get("/scan?metric=rsi&symbols=UPUSDT,DOWNUSDT", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["metric"], "rsi");
    assert_eq!(body["timeframe"], "1h");
    assert_eq!(body["universe"], "explicit");
    assert_eq!(body["complete"], true);
    assert_eq!(body["requested"], 2);

    let rows = body["rows"].as_array().expect("rows is an array");
    assert_eq!(rows.len(), 2, "{body}");

    // The claim that matters: the rising series leads. If the sort were missing
    // or inverted, the route would still answer 200 with two rows.
    assert_eq!(rows[0]["symbol"], "UPUSDT", "{body}");
    assert_eq!(rows[1]["symbol"], "DOWNUSDT", "{body}");

    let top = rows[0]["value"].as_f64().expect("a number");
    let bottom = rows[1]["value"].as_f64().expect("a number");
    assert!(top > bottom, "a rising series must outrank a falling one: {body}");
    assert!(top > 50.0, "an unbroken rise is overbought: {body}");
    assert!(bottom < 50.0, "an unbroken fall is oversold: {body}");
}

#[tokio::test]
async fn a_scanned_row_says_which_bar_its_number_is_from() {
    let Some(h) = Harness::new().await else {
        return;
    };
    seat(&h, "UPUSDT", "1h", &rising(60, 100.0));

    let (status, body) = h.get("/scan?symbols=UPUSDT", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let row = &body["rows"][0];
    // Milliseconds on the wire, and it is the bar the fixture seated newest.
    // Asserting the *value* rather than "is not null" is the point: a timestamp
    // returned in nanoseconds would render as a date in 1970 and read as stale
    // data rather than as a unit bug, and one returned as the *oldest* bar
    // would pass a null check while being a lie about a ranking.
    let step_ns: i64 = 3_600_000_000_000;
    let latest_bucket = (market_data::symbols::now_ns() / step_ns) * step_ns;
    let as_of_ms = row["as_of_ms"].as_i64().expect("a timestamp");
    let expected_ms = latest_bucket / 1_000_000;
    // Within one bar of the newest bucket, so a run that straddles an hour
    // boundary does not fail for a reason that has nothing to do with the code
    // under test -- while still being far tighter than "any number at all",
    // which is what a nanosecond/millisecond swap would sail through.
    assert!(
        (as_of_ms - expected_ms).abs() <= step_ns / 1_000_000,
        "as_of_ms {as_of_ms} should be the newest bar, near {expected_ms}"
    );
    assert_eq!(row["bars"], 60);
}

#[tokio::test]
async fn a_symbol_with_too_little_history_is_a_failure_not_a_zero() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Three bars cannot produce a 14-period RSI. Reporting 0 would put this
    // symbol at the *bottom* of a ranking -- which reads as "nothing happening"
    // when the truth is "we could not look".
    seat(&h, "THINUSDT", "1h", &rising(3, 100.0));
    seat(&h, "UPUSDT", "1h", &rising(60, 100.0));

    let (status, body) = h.get("/scan?symbols=THINUSDT,UPUSDT", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let rows = body["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "only one symbol is measurable: {body}");
    assert_eq!(rows[0]["symbol"], "UPUSDT");

    let failures = body["failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "{body}");
    assert_eq!(failures[0]["symbol"], "THINUSDT");
    assert_eq!(failures[0]["value"], serde_json::Value::Null);
    assert!(
        failures[0]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "a failure must say why: {body}"
    );

    // The count is honest: one of two, both named.
    assert_eq!(body["complete"], false, "{body}");
    let summary = body["summary"].as_str().expect("a summary");
    assert!(summary.contains("1 of 2"), "{summary}");
}

#[tokio::test]
async fn the_scan_ranks_by_change_percent_when_asked() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Same shape, different verdicts: a metric the route ignored would give the
    // same answer as RSI here, so the input is chosen so it cannot.
    seat(&h, "UPUSDT", "1h", &rising(60, 100.0));
    seat(&h, "DOWNUSDT", "1h", &falling(60, 100.0));

    let (status, body) = h
        .get("/scan?metric=change_percent&symbols=UPUSDT,DOWNUSDT", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["metric"], "change_percent");

    let rows = body["rows"].as_array().expect("rows");
    assert_eq!(rows[0]["symbol"], "UPUSDT", "{body}");
    assert!(
        rows[0]["value"].as_f64().expect("a number") > 0.0,
        "the metric is echoed but a rising series must score positive: {body}"
    );
    assert!(
        rows[1]["value"].as_f64().expect("a number") < 0.0,
        "{body}"
    );
}

#[tokio::test]
async fn an_unknown_metric_is_refused_with_the_ones_that_work() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/scan?metric=gann_angle&symbols=BTCUSDT", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A refusal a user cannot act on is a dead end, so the message names the
    // vocabulary -- and it must name it completely, or the list is the bug.
    let message = format!("{body}");
    for metric in ["rsi", "atr_percent", "change_percent"] {
        assert!(message.contains(metric), "{message} omits `{metric}`");
    }
}

#[tokio::test]
async fn an_unknown_timeframe_is_refused_with_the_ones_that_work() {
    let Some(h) = Harness::new().await else {
        return;
    };

    let (status, body) = h.get("/scan?timeframe=3h&symbols=BTCUSDT", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let message = format!("{body}");
    // `1w` in particular: it is the resolution this platform most recently
    // learned, and a hand-written list of "known timeframes" is exactly what
    // would still be missing it.
    for timeframe in ["1m", "5m", "15m", "1h", "4h", "1d", "1w"] {
        assert!(message.contains(timeframe), "{message} omits `{timeframe}`");
    }
}

#[tokio::test]
async fn a_scan_with_an_empty_universe_says_so_rather_than_answering_empty() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // The harness's symbol index is deliberately empty and never refreshed, so
    // this is the "no symbols named, and the venue index has nothing" case. A
    // 200 with `rows: []` would be indistinguishable from a quiet market.
    let (status, body) = h.get("/scan", None).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        format!("{body}").to_lowercase().contains("nothing to scan"),
        "{body}"
    );
}

#[tokio::test]
async fn the_scan_is_public_for_the_same_reason_the_chart_is() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // docs/12: market data is not user data, so market reads are public. A scan
    // is a market read -- it is how a user finds a symbol to chart.
    seat(&h, "UPUSDT", "1h", &rising(60, 100.0));
    let (status, _) = h.get("/scan?symbols=UPUSDT", None).await;
    assert_eq!(status, StatusCode::OK);
}
