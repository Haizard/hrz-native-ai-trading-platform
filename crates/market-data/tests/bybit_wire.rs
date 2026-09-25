//! The Bybit decoder, against frames captured from the live socket.
//!
//! ## Why this is an integration test and not a unit test
//!
//! `bybit_codec.rs`'s unit tests already decode these frames. What they cannot
//! check is the thing that actually broke when this was first written: the
//! **serde field mapping**. A struct field named `s_side` does not deserialize
//! from a JSON key `S`, and nothing in the type system says so -- the failure is
//! a runtime missing-field error that turns every trade frame into `Malformed`.
//! A unit test written from the same misunderstanding as the code passes
//! happily, because both sides agree on the wrong thing.
//!
//! So this file drives the codec through the **public** API with a transcript
//! that was copied out of a socket, not written by hand from the same reading of
//! the docs. The frames below are verbatim; the only edits are truncation of
//! long level arrays, marked where it happens.
//!
//! ## What "verbatim" is worth here
//!
//! Three separate defects came out of actually running this against the venue,
//! and each would have passed a docs-derived test:
//!
//! 1. Bybit's documented `u == 1` reset boundary **does not exist** on
//!    `orderbook.*`. A fresh subscribe returns a full book at a real id.
//! 2. `publicTrade` frames carry **8-16 trades in an array**, not one.
//! 3. `S` is the **taker** side, so the platform's `is_buyer_maker` is inverted.
//!
//! A guard that cannot fail is worse than no guard, and all three of those
//! guards are only able to fail because the fixture came off the wire.

use market_data::{BybitCodec, Frame, Incoming, Subscription, WireCodec};

/// The ack Bybit sends for a subscribe. No `topic` field at all.
const SUBSCRIBE_ACK: &str = r#"{"success":true,"ret_msg":"subscribe","conn_id":"d9avt8sb86tr31f6ej90-d0qm8","op":"subscribe"}"#;

/// A full book, exactly as a **fresh** subscription is answered.
///
/// The load-bearing number is `u: 219167606` -- a real current id, not `1`. This
/// is the frame that disproved the planned `last_update_id = u - 1` bridge.
/// Levels truncated from 50 to 3; the ids are untouched.
const BOOK_SNAPSHOT: &str = r#"{"topic":"orderbook.50.ETHUSDT","ts":1789875210000,"type":"snapshot","data":{"s":"ETHUSDT","b":[["2191.5","3.25"],["2191.4","1.0"],["2191.0","0.5"]],"a":[["2191.6","2.0"],["2191.7","4.0"]],"u":219167606,"seq":181840388171}}"#;

/// The delta immediately after it. `u` is exactly one more.
///
/// Carries a `"0"` size, which is Bybit's way of deleting a level -- and an
/// **empty ask side**, which means "no ask change", not "no asks".
const BOOK_DELTA: &str = r#"{"topic":"orderbook.50.ETHUSDT","ts":1789875210020,"type":"delta","data":{"s":"ETHUSDT","b":[["2191.5","0.0"],["2191.2","9.0"]],"a":[],"u":219167607,"seq":181840388311}}"#;

/// Three trades in one frame: the `S` word, the string `i`, the millisecond `T`.
const TRADE_FRAME: &str = r#"{"topic":"publicTrade.BTCUSDT","ts":1789875217841,"type":"snapshot","data":[{"BT":false,"RPI":false,"S":"Sell","T":1789875217840,"i":"2290000001211680903","p":"80413.3","s":"BTCUSDT","seq":114263055846,"v":"0.06"},{"BT":false,"RPI":false,"S":"Buy","T":1789875217840,"i":"2290000001211680919","p":"80413.4","s":"BTCUSDT","seq":114263055850,"v":"0.004904"},{"BT":false,"RPI":false,"S":"Buy","T":1789875217841,"i":"2290000001211680920","p":"80413.4","s":"BTCUSDT","seq":114263055851,"v":"0.001"}]}"#;

/// What Bybit answers `{"op":"ping"}` with. No `topic`, so it must be control.
const PONG: &str =
    r#"{"success":true,"ret_msg":"pong","conn_id":"d9avt8sb86tr31f6ej90-d0qm8","op":"pong"}"#;

fn codec() -> BybitCodec {
    BybitCodec::spot()
}

#[test]
fn a_subscribe_ack_is_control_and_never_kills_the_pump() {
    assert_eq!(codec().parse_frame(SUBSCRIBE_ACK), Frame::Control);
    assert_eq!(codec().parse_frame(PONG), Frame::Control);
}

/// The snapshot is a snapshot, and it carries its own id.
#[test]
fn a_fresh_subscription_is_answered_with_a_full_book_at_a_real_id() {
    let Frame::Data(incoming) = codec().parse_frame(BOOK_SNAPSHOT) else {
        panic!("the snapshot must decode: {BOOK_SNAPSHOT}");
    };
    let Incoming::BookSnapshot {
        symbol,
        bids,
        asks,
        last_update_id,
    } = *incoming
    else {
        panic!("a `type: snapshot` frame must decode as a snapshot");
    };

    assert_eq!(symbol, "ETHUSDT");
    assert_eq!(last_update_id, 219_167_606);
    assert_ne!(
        last_update_id, 1,
        "the documented `u == 1` reset boundary does not occur on `orderbook.*`; \
         if this ever reads 1, the documented behaviour has changed and the \
         bridge arithmetic must be revisited"
    );
    assert_eq!(bids, vec![(2191.5, 3.25), (2191.4, 1.0), (2191.0, 0.5)]);
    assert_eq!(asks, vec![(2191.6, 2.0), (2191.7, 4.0)]);
}

/// The snapshot's `u` plus one is the next delta's `u`, which is what lets an
/// in-band book bridge with **no REST call**.
#[test]
fn the_delta_after_the_snapshot_continues_from_it() {
    let Frame::Data(snapshot) = codec().parse_frame(BOOK_SNAPSHOT) else {
        panic!("snapshot must decode");
    };
    let Incoming::BookSnapshot { last_update_id, .. } = *snapshot else {
        panic!("expected a snapshot");
    };

    let Frame::Data(delta) = codec().parse_frame(BOOK_DELTA) else {
        panic!("delta must decode");
    };
    let Incoming::BookDelta(diff) = *delta else {
        panic!(
            "a `type: delta` frame must NOT decode as a snapshot -- that would \
                replace a 50-level book with the two levels that moved"
        );
    };

    assert_eq!(
        diff.final_update_id,
        last_update_id + 1,
        "if these were not adjacent the book could not bridge without REST"
    );
}

/// Driven through the real synchronizer: the book must actually sync.
#[test]
fn the_in_band_snapshot_and_delta_sync_the_book_with_no_rest() {
    use market_data::{DiffOutcome, OrderBookSynchronizer};

    let Frame::Data(snapshot) = codec().parse_frame(BOOK_SNAPSHOT) else {
        panic!("snapshot must decode");
    };
    let Incoming::BookSnapshot {
        symbol,
        bids,
        asks,
        last_update_id,
    } = *snapshot
    else {
        panic!("expected a snapshot");
    };

    let mut book = OrderBookSynchronizer::new(&symbol);
    book.set_snapshot(&bids, &asks, last_update_id, 0);
    assert!(!book.is_synced(), "a snapshot alone is not a synced book");

    let Frame::Data(delta) = codec().parse_frame(BOOK_DELTA) else {
        panic!("delta must decode");
    };
    let Incoming::BookDelta(diff) = *delta else {
        panic!("expected a delta");
    };

    let outcome = book.on_diff(
        market_data::DepthDiff {
            first_update_id: diff.first_update_id,
            final_update_id: diff.final_update_id,
            bids: diff.bids,
            asks: diff.asks,
        },
        0,
    );

    assert_eq!(outcome, DiffOutcome::Applied);
    assert!(
        book.is_synced(),
        "the book is live with no REST request made"
    );
}

/// Every trade in a batched frame must arrive, and the taker side must invert.
#[test]
fn a_batched_trade_frame_yields_every_trade_with_the_taker_side_inverted() {
    let Frame::Data(incoming) = codec().parse_frame(TRADE_FRAME) else {
        panic!("the trade frame must decode: {TRADE_FRAME}");
    };
    let Incoming::Trades(trades) = *incoming else {
        panic!("`publicTrade` data is an array and must decode as trades");
    };

    assert_eq!(
        trades.len(),
        3,
        "a codec reading only `data[0]` would keep 1/3 here and 1/16 on the live \
         socket, with every field it checked still correct"
    );

    assert_eq!(trades[0].trade_id, 2_290_000_001_211_680_903);
    assert!((trades[0].price - 80413.3).abs() < 1e-9);
    assert!((trades[0].quantity - 0.06).abs() < 1e-9);
    assert_eq!(trades[0].timestamp, 1_789_875_217_840_000_000);
    assert_eq!(trades[0].symbol, "BTCUSDT");

    // `S` is the taker side: a taker Sell means the buyer was the maker.
    assert!(
        trades[0].is_buyer_maker,
        "S == \"Sell\" inverts to maker=true"
    );
    assert!(
        !trades[1].is_buyer_maker,
        "S == \"Buy\" inverts to maker=false"
    );
    assert!(!trades[2].is_buyer_maker);

    // Ids are distinct and ascending, which is what the gap detector needs.
    assert!(trades[1].trade_id > trades[0].trade_id);
    assert!(trades[2].trade_id > trades[1].trade_id);
}

/// A zero price must be refused here, before `PriceKey::new` debug-asserts it.
#[test]
fn a_zero_price_is_refused_before_it_can_reach_the_book() {
    let broken = BOOK_SNAPSHOT.replace("\"2191.0\"", "\"0\"");
    let Frame::Malformed(reason) = codec().parse_frame(&broken) else {
        panic!("a zero price must be refused, not installed");
    };
    assert!(
        reason.contains("positive"),
        "the reason should name the rule it broke, got `{reason}`"
    );
}

/// A non-numeric trade id must not be invented: a synthesised id turns the gap
/// detector into a random number generator.
#[test]
fn a_non_numeric_trade_id_is_malformed_rather_than_invented() {
    let broken = TRADE_FRAME.replace("\"2290000001211680903\"", "\"not-an-id\"");
    assert!(matches!(codec().parse_frame(&broken), Frame::Malformed(_)));
}

/// Topics we do not handle are ignored, not fatal.
#[test]
fn an_unsubscribed_topic_is_unrecognised() {
    let other = r#"{"topic":"kline.1.BTCUSDT","ts":1,"type":"snapshot","data":[]}"#;
    assert_eq!(codec().parse_frame(other), Frame::Unrecognised);
}

/// The subscribe payload is Bybit's spelling, not Binance's.
#[test]
fn the_subscribe_payload_uses_bybit_topics() {
    let raw = codec()
        .subscribe_payload(&[
            Subscription::OrderBook {
                symbol: "btcusdt".into(),
            },
            Subscription::Trades {
                symbol: "BTCUSDT".into(),
            },
        ])
        .expect("a non-empty subscribe must serialise");

    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["op"], "subscribe");
    let args: Vec<String> = serde_json::from_value(value["args"].clone()).unwrap();
    assert_eq!(args, vec!["orderbook.50.BTCUSDT", "publicTrade.BTCUSDT"]);
}

/// Bybit pings; Binance does not. The difference is the codec's answer, not the
/// collector's, so a venue that needs liveness cannot be served by a collector
/// that never sends one.
#[test]
fn bybit_pings_and_binance_does_not() {
    let bybit = BybitCodec::spot();
    assert_eq!(
        bybit.heartbeat(0).as_deref(),
        Some(r#"{"op":"ping"}"#),
        "Bybit drops a quiet socket after ten minutes"
    );

    let binance = market_data::BinanceCodec::new();
    assert_eq!(
        binance.heartbeat(0),
        None,
        "Binance pings from its own side and must not be sent an invented control frame"
    );
}
