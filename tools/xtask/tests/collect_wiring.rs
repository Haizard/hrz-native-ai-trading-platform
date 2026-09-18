//! Every stream `xtask collect` subscribes to must have a pump draining it.
//!
//! ## Why this test exists
//!
//! `xtask collect` subscribed to trades and order books and spawned two pumps.
//! The collector, meanwhile, had always aggregated trades into all six
//! resolutions and published the closed ones to the bus -- so a collector run
//! persisted trades, persisted snapshots, and wrote **zero candles**. The
//! candle table could only ever be filled by `backfill`, which fetches from
//! REST, and Phase 1's exit criterion is explicitly about a *collector* run
//! producing that table.
//!
//! Nothing failed. There was no error, no warning, and no test: a broadcast
//! channel with no receivers is a documented no-op, `insert_candles` still had
//! a caller (`backfill`), and `market-data`'s own bus test passed because it
//! subscribes directly to the bus. The bug was in the *wiring*, and wiring is
//! not something a unit test of either side can see.
//!
//! So this parses `src/main.rs` the way `api-gateway/tests/packaging.rs` parses
//! the `Dockerfile`: it is a guard against the specific mistake, not a
//! substitute for running the collector. What it can prove is the invariant --
//! *a subscription without a pump is a stream that goes nowhere* -- and it
//! fails on a **new** stream added without one, not only on this one.

use std::path::Path;

fn source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("main.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

/// The pumps themselves, which do **not** live in `xtask`.
///
/// They were defined in `src/main.rs` when this file was written and moved to
/// `crates/db/src/pump.rs` when the gateway needed the same batching loop --
/// two copies of a writer is the `docs/19` row 20 defect. Anything here that
/// asserts on a pump's *body* has to read that file, or it silently checks the
/// wrong one: `the_candle_pump_uses_the_idempotent_upsert` passed for a while
/// after the move only because `backfill` in `main.rs` also calls
/// `insert_candles`, which is a witness for nothing about the pump.
fn pump_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates")
        .join("db")
        .join("src")
        .join("pump.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

/// The body of `fn collect`, brace-matched.
///
/// Scoped deliberately: `pump_candles` is spawned in `collect` and defined
/// outside it, so a whole-file search would pass even if the spawn were
/// deleted and only the definition left behind.
fn collect_body(src: &str) -> String {
    let start = src
        .find("async fn collect(")
        .expect("`collect` must still exist -- rename it and this guard needs updating");
    let open = src[start..]
        .find('{')
        .map(|i| start + i)
        .expect("`collect` must have a body");

    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return src[open..open + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in `collect`");
}

/// Subscription call in `collect` -> the pump that must drain it.
///
/// Adding a stream to the collector and a subscription to `collect` without
/// adding the pair here is itself a failure (see the exhaustive check below),
/// which is the point: the reminder arrives while you are writing the code,
/// not six months later from a table that stayed empty.
const PUMPS: &[(&str, &str)] = &[
    ("trade_stream", "pump_trades"),
    ("order_book_stream", "pump_books"),
    ("candle_stream", "pump_candles"),
];

#[test]
fn every_subscription_in_collect_is_drained_by_a_pump() {
    let src = source();
    let pumps = pump_source();
    let body = collect_body(&src);

    for (stream, pump) in PUMPS {
        assert!(
            body.contains(stream),
            "`collect` no longer subscribes to `{stream}`. If that is deliberate, \
             remove it from PUMPS too -- but check whether the stream is now \
             drained anywhere else, because a subscription with no reader is how \
             candles went unwritten for the whole of Phases 1-8."
        );
        assert!(
            body.contains(&format!("{pump}(")),
            "`collect` subscribes to `{stream}` but never spawns `{pump}`. A \
             subscription with no pump is a stream that goes nowhere, and it \
             fails silently -- the broadcast channel simply has no receiver."
        );
        assert!(
            pumps.contains(&format!("async fn {pump}(")),
            "`{pump}` is spawned by `collect` but not defined. It is defined in \
             `crates/db/src/pump.rs`, not in `xtask` -- if it has moved again, \
             `pump_source()` and every assertion that reads a pump body move \
             with it."
        );
    }
}

#[test]
fn no_subscription_is_missing_from_the_pump_table() {
    let src = source();
    let body = collect_body(&src);

    // Whitespace-stripped first, because the subscriptions are written as a
    // fluent chain: `collector` and `.trade_stream` are on separate lines, so a
    // literal search for `collector.` finds nothing. (This guard shipped that
    // way for one run -- it is the same class of mistake as the bug it is
    // meant to catch, which is why the "does the guard fail" step is not
    // optional.)
    let flat: String = body.split_whitespace().collect();

    // `collector.<name>(&symbol)` is the shape every subscription uses.
    let mut found: Vec<String> = Vec::new();
    for (idx, _) in flat.match_indices("collector.") {
        let rest = &flat[idx + "collector.".len()..];
        let Some(open) = rest.find('(') else { continue };
        let name = &rest[..open];
        if name.ends_with("_stream") {
            found.push(name.to_string());
        }
    }

    assert!(
        !found.is_empty(),
        "found no stream subscriptions in `collect` -- either it stopped \
         subscribing to anything, or this guard is looking at the wrong shape"
    );

    for name in found {
        assert!(
            PUMPS.iter().any(|(stream, _)| *stream == name),
            "`collect` subscribes to `{name}` but it is not in PUMPS, so nothing \
             asserts that a pump drains it. Add the pair, or the next person to \
             add a stream gets no reminder that it needs one."
        );
    }
}

/// The candle pump writes with `insert_candles`, which is an idempotent upsert.
///
/// It matters more here than for the other two pumps. A restarted collector
/// re-emits a bucket it had already closed, and `(symbol, timeframe,
/// open_time)` is the primary key -- so a non-idempotent insert would turn a
/// restart into a primary-key error rather than a no-op.
#[test]
fn the_candle_pump_uses_the_idempotent_upsert() {
    let src = pump_source();
    assert!(
        src.contains("repositories::insert_candles("),
        "the candle pump must persist through `repositories::insert_candles`, \
         which upserts on (symbol, timeframe, open_time). Asserted against \
         `crates/db/src/pump.rs`: `xtask/src/main.rs` calls `insert_candles` \
         too, in `backfill`, so checking there proves nothing about the pump."
    );
}

/// A counter is for what happened, not what was attempted.
#[test]
fn candle_writes_are_counted_only_on_success() {
    let src = pump_source();
    // The candle-specific `flush_candles` became the generic `flush_buffer`
    // plus `note` when the pumps moved into `db::pump`. `note` is where the
    // rule now lives, and it is the only place a flush outcome is recorded.
    let start = src
        .find("fn note(")
        .expect("`note` must exist -- the flush bookkeeping moved out of `xtask`");
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("`note` must terminate");
    let body = &src[start..end];

    let (before_ok, after_ok) = body
        .split_once("is_ok()")
        .expect("`note` must branch on whether the write succeeded");
    assert!(
        after_ok.contains("fetch_add"),
        "the success arm of `note` must be the one that counts. Counting the \
         attempt would let a run against an unreachable database report the \
         same number as a healthy one."
    );
    assert!(
        !before_ok.contains("fetch_add"),
        "nothing outside the success arm may count a write that did not happen"
    );
}

/// Guards against the exact shape of the original bug: a `--no-persist` run is
/// the only path where the buffer is dropped on purpose.
#[test]
fn the_collect_command_still_has_a_persist_path() {
    let src = source();
    let body = collect_body(&src);
    assert!(
        body.contains("--no-persist"),
        "`collect` should still announce when it is not writing to the database, \
         otherwise a `--no-persist` run looks identical to a run whose writes \
         are all failing"
    );
    assert!(
        body.contains("candles_written"),
        "`collect` must report candles written -- without it a soak proves only \
         that the process stayed up, which is precisely what it proved while \
         the candle table stayed empty"
    );
}
