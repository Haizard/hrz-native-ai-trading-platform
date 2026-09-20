//! The adversarial suite `docs/08-SANDBOX-WASM.md` requires before Phase 4 can
//! exit: intentionally malformed or resource-exhausting inputs must fail
//! **safely**.
//!
//! ## What "safely" means here
//!
//! Not "returns an error". Every test in this file ends with the host still
//! holding a working process and a still-usable sandbox, and several of them
//! assert exactly that by building a second sandbox afterwards. A sandbox whose
//! failure mode is "the host is now in an unknown state" is worse than no
//! sandbox, because it looks like one.
//!
//! ## Why hand-written WASM
//!
//! The modules below are things a Rust compiler will not produce: one that
//! imports `wasi_snapshot_preview1`, one that loops forever, one that grows
//! memory until something stops it. Writing them in WAT is the only way to test
//! the boundary against the shapes an attacker would actually send, rather than
//! against the shapes our own compiler happens to emit.
//!
//! ## The four kinds of attack, and where each is stopped
//!
//! | Attack | Stopped by | Test |
//! |---|---|---|
//! | Import something forbidden | import-section check, before instantiation | [`forbidden_imports`] |
//! | Run forever | fuel metering | [`an_infinite_loop_is_halted_by_fuel`] |
//! | Consume the host's memory | wasmtime's limiter | [`a_memory_bomb_is_stopped_by_the_limiter`] |
//! | Crash the host | `panic = "abort"` turns a panic into a trap | [`a_guest_trap_does_not_take_the_host_with_it`] |
//!
//! The remaining tests cover the cheap rejections that happen before the sandbox
//! is involved at all, and the ways a module can be malformed rather than
//! hostile.

use std::collections::BTreeMap;

use analytics_core::state::{MarketStateConfig, build_market_state};
use analytics_core::types::{Candle, Timeframe};
use sandbox::{Sandbox, SandboxError, SandboxLimits};
use strategy_dsl::{MAX_DOCUMENT_BYTES, parse_and_validate};
use strategy_runtime::context::MarketContext;

/// The smallest module that satisfies the ABI and does nothing.
///
/// Every hostile fixture is this plus one change, so the tests read as "the
/// normal module, but it also ..." rather than as opaque WAT.
const INERT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "sbx_abi_version") (result i32) (i32.const 1))
  (func (export "sbx_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "sbx_free") (param i32 i32))
  (func (export "sbx_init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_eval") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_error_ptr") (result i32) (i32.const 0))
  (func (export "sbx_error_len") (result i32) (i32.const 0))
)
"#;

/// Build a sandbox from a WAT fixture, expecting the import check to pass.
fn sandbox_of(wat: &str) -> Sandbox {
    Sandbox::from_wasm(wat.as_bytes(), SandboxLimits::default())
        .unwrap_or_else(|e| panic!("the fixture should have been accepted: {e}"))
}

/// Build a sandbox from a WAT fixture, expecting the import check to refuse it.
fn refusal(wat: &str) -> SandboxError {
    match Sandbox::from_wasm(wat.as_bytes(), SandboxLimits::default()) {
        Ok(_) => panic!("the fixture should have been refused"),
        Err(error) => error,
    }
}

/// The `INERT` module with `sbx_eval` replaced.
fn with_eval(body: &str) -> String {
    let inert_eval = r#"(func (export "sbx_eval") (param i32 i32) (result i32) (i32.const 0))"#;
    let replacement = format!(r#"(func (export "sbx_eval") (param i32 i32) (result i32) {body})"#);
    INERT.replace(inert_eval, &replacement)
}

/// The `INERT` module with an extra import prepended.
fn importing(module: &str, field: &str) -> String {
    let import = format!(r#"(import "{module}" "{field}" (func (param i32 i32) (result i32)))"#);
    INERT.replace("(module", &format!("(module {import}"))
}

/// Bytes that are not a module at all, expecting the load to refuse them.
///
/// Spelled as a `match` rather than `expect_err` because `Sandbox` deliberately
/// has no `Debug` impl: it owns an engine and a ticker thread, and printing
/// either would be noise. `expect_err` demands `Debug` on the `Ok` type, so the
/// assertion is written out instead.
fn refused_bytes(bytes: &[u8]) -> SandboxError {
    match Sandbox::from_wasm(bytes, SandboxLimits::default()) {
        Ok(_) => panic!("those bytes are not a module, but they were accepted"),
        Err(error) => error,
    }
}

/// A valid document, for the tests that need to get as far as loading one.
const DOCUMENT: &str = r#"{
    "name": "Adversarial probe",
    "version": "1",
    "kind": "strategy",
    "market": "BTCUSDT",
    "timeframes": {"entry": "5m"},
    "entry": {"all_of": [
        {"timeframe": "entry", "condition": "close > threshold(0)"}
    ]},
    "risk": {"max_risk_pct": 1.0, "stop": {"kind": "below_recent_low", "bars": 5}},
    "invalidation": [{"timeframe": "entry", "condition": "close_below(vwap)"}]
}"#;

/// A one-timeframe context, enough for any of these tests.
fn context() -> MarketContext {
    let timeframe = Timeframe::M5;
    let candles: Vec<Candle> = (0..40)
        .map(|i| {
            let price = 100.0 + f64::from(i);
            Candle {
                symbol: "BTCUSDT".into(),
                timeframe,
                open_time: i64::from(i) * timeframe.nanos(),
                open: price,
                high: price + 1.0,
                low: price - 1.0,
                close: price,
                volume: 10.0,
                buy_volume: 6.0,
                sell_volume: 4.0,
            }
        })
        .collect();

    let state = build_market_state(&candles, &[], &MarketStateConfig::default())
        .expect("the fixture has enough candles");
    let view = strategy_runtime::context::TimeframeView {
        name: "entry".into(),
        timeframe,
        candle: candles[candles.len() - 1].clone(),
        state,
        previous: None,
        history: candles,
    };

    let mut timeframes = BTreeMap::new();
    timeframes.insert("entry".to_string(), view);

    MarketContext {
        symbol: "BTCUSDT".into(),
        now: 40 * timeframe.nanos(),
        decision_timeframe: "entry".into(),
        timeframes,
        position: None,
        equity: 10_000.0,
    }
}

// ---------------------------------------------------------------------------
// 1. Forbidden imports: stopped before anything is instantiated.
// ---------------------------------------------------------------------------

mod forbidden_imports {
    use super::{SandboxError, importing, parse_and_validate, refusal};

    /// Every one of these is a capability the spec's deny list names.
    const FORBIDDEN: &[(&str, &str, &str)] = &[
        ("filesystem", "env", "fs_read"),
        ("network", "env", "net_connect"),
        ("shell", "env", "exec"),
        ("environment variables", "env", "getenv"),
        (
            "WASI's own entry points",
            "wasi_snapshot_preview1",
            "fd_write",
        ),
        (
            "WASI process control",
            "wasi_snapshot_preview1",
            "proc_exit",
        ),
        (
            "a plausible but unlisted host function",
            "env",
            "host_steal_secrets",
        ),
        (
            "a plausible but unlisted module",
            "wasi",
            "host_emit_signal",
        ),
    ];

    #[test]
    fn every_denied_capability_is_refused_by_name() {
        for (what, module, field) in FORBIDDEN {
            let error = refusal(&importing(module, field));
            match error {
                SandboxError::CapabilityDenied(message) => assert!(
                    message.contains(field),
                    "{what}: the refusal should name `{field}`, said: {message}"
                ),
                other => panic!("{what}: expected a capability denial, got {other}"),
            }
        }
    }

    #[test]
    fn the_refusal_happens_before_instantiation_so_nothing_is_reachable() {
        // A module that imports `fd_write` *and* exports a working `sbx_eval`.
        // If the check ran at call time instead of load time, this module would
        // be live and the refusal would be a runtime concern.
        let wat = importing("wasi_snapshot_preview1", "fd_write");
        assert!(
            wat.contains("sbx_eval"),
            "the fixture should still export the ABI"
        );

        let error = refusal(&wat);
        assert!(matches!(error, SandboxError::CapabilityDenied(_)));

        // And the host is fine: a real sandbox still works right afterwards.
        let sandbox = super::sandbox_of(super::INERT);
        let document = parse_and_validate(super::DOCUMENT).unwrap();
        assert!(sandbox.start(&document).is_ok());
    }
}

// ---------------------------------------------------------------------------
// 2. Resource exhaustion: stopped by limits enforced outside the guest.
// ---------------------------------------------------------------------------

#[test]
fn an_infinite_loop_is_halted_by_fuel() {
    // `(loop $spin (br $spin))` -- a `while true {}` with nothing in it.
    let wat = with_eval("(loop $spin (br $spin)) (i32.const 0)");
    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();

    let mut session = sandbox
        .start(&document)
        .expect("the module satisfies the ABI");
    let started = std::time::Instant::now();
    let error = session
        .evaluate(&context())
        .expect_err("an endless loop must not succeed");
    let elapsed = started.elapsed();

    match error {
        SandboxError::FuelExhausted { used, budget } => {
            assert_eq!(budget, SandboxLimits::default().fuel);
            assert!(used > 0, "the loop should have burned instructions");
        }
        other => panic!("expected a fuel exhaustion, got {other}"),
    }
    // Deterministic, and fast: fuel runs out long before the wall-clock
    // deadline would have.
    assert!(
        elapsed < SandboxLimits::default().timeout * 20,
        "fuel should stop this in well under a second, took {elapsed:?}"
    );
}

#[test]
fn a_tiny_fuel_budget_is_enforced_exactly() {
    // The same loop with a budget too small for even the setup. The point is
    // that the ceiling is the configured one and not some internal default.
    let wat = with_eval("(loop $spin (br $spin)) (i32.const 0)");
    let limits = SandboxLimits::default().with_fuel(1_000);
    let sandbox = Sandbox::from_wasm(wat.as_bytes(), limits).expect("accepted");

    let document = parse_and_validate(DOCUMENT).unwrap();
    let mut session = sandbox.start(&document).expect("ABI is satisfied");
    let error = session
        .evaluate(&context())
        .expect_err("budget is too small");

    match error {
        SandboxError::FuelExhausted { budget, .. } => assert_eq!(budget, 1_000),
        other => panic!("expected a fuel exhaustion, got {other}"),
    }
}

#[test]
fn a_memory_bomb_is_stopped_by_the_limiter() {
    // Grow a page at a time until wasmtime says no, then report how far it got.
    let wat = with_eval(
        r#"(block $full
             (loop $grow
               (br_if $full (i32.eq (memory.grow (i32.const 1)) (i32.const -1)))
               (br $grow)))
           (i32.const 0)"#,
    );
    let limits = SandboxLimits::default().with_memory(2 * 1024 * 1024);
    let sandbox = Sandbox::from_wasm(wat.as_bytes(), limits).expect("accepted");
    let document = parse_and_validate(DOCUMENT).unwrap();

    let mut session = sandbox.start(&document).expect("ABI is satisfied");
    let _ = session.evaluate(&context());

    let held = session.memory_bytes();
    assert!(
        held <= limits.max_memory_bytes,
        "the module holds {held} bytes, over the {} byte ceiling",
        limits.max_memory_bytes
    );
    assert!(held > 0, "the module should hold at least its initial page");
}

// ---------------------------------------------------------------------------
// 3. Traps: the guest dies, the host does not.
// ---------------------------------------------------------------------------

#[test]
fn a_guest_trap_does_not_take_the_host_with_it() {
    // `unreachable` is what a Rust panic compiles to under `panic = "abort"`,
    // so this is the shape a panic inside the interpreter would take.
    let wat = with_eval("unreachable");
    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();

    let mut session = sandbox.start(&document).expect("ABI is satisfied");
    let error = session
        .evaluate(&context())
        .expect_err("a trap must surface");

    match error {
        SandboxError::Trap(_) => {}
        other => panic!("expected a trap, got {other}"),
    }

    // The host is intact and a *different* sandbox still works, which is the
    // claim that matters: one hostile module cannot poison the process.
    let good = Sandbox::new().expect("the shipped guest still compiles");
    let mut strategy = sandbox::SandboxedStrategy::new(&good, &document).expect("accepted");
    let signals = strategy_eval(&mut strategy);
    assert!(signals, "the healthy sandbox should still emit its signal");
}

/// Evaluate one context through a `SandboxedStrategy`, reporting whether it
/// emitted anything.
fn strategy_eval(strategy: &mut sandbox::SandboxedStrategy) -> bool {
    use strategy_runtime::engine::Strategy as _;
    strategy.on_candle(&context()).is_some()
}

#[test]
fn a_module_that_refuses_every_document_says_why() {
    // A guest that returns -1 with a message, which is how the real interpreter
    // reports a document it cannot accept.
    let message = b"not today";
    let wat = format!(
        r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 2048) "{data}")
  (func (export "sbx_abi_version") (result i32) (i32.const 1))
  (func (export "sbx_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "sbx_free") (param i32 i32))
  (func (export "sbx_init") (param i32 i32) (result i32) (i32.const -1))
  (func (export "sbx_eval") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_error_ptr") (result i32) (i32.const 2048))
  (func (export "sbx_error_len") (result i32) (i32.const {len}))
)
"#,
        data = escaped(message),
        len = message.len(),
    );

    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();
    let error = sandbox
        .start(&document)
        .expect_err("the guest refused the document");

    match error {
        SandboxError::Guest(text) => assert_eq!(text, "not today"),
        other => panic!("expected the guest's own message, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// 4. Malformed modules: refused, not trusted.
// ---------------------------------------------------------------------------

#[test]
fn truncated_module_bytes_are_refused() {
    let error = refused_bytes(b"\0asm\x01\0\0");
    assert!(matches!(error, SandboxError::Instantiation(_)), "{error}");
}

#[test]
fn arbitrary_bytes_are_refused() {
    let error = refused_bytes(&[0xff; 512]);
    assert!(matches!(error, SandboxError::Instantiation(_)), "{error}");
}

#[test]
fn a_module_speaking_another_abi_version_is_refused() {
    let wat = INERT.replace("(i32.const 1)", "(i32.const 999)");
    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();

    match sandbox.start(&document) {
        Err(SandboxError::AbiMismatch { found, expected }) => {
            assert_eq!(found, 999);
            assert_eq!(expected, sandbox::ABI_VERSION);
        }
        other => panic!("expected an ABI mismatch, got {other:?}"),
    }
}

#[test]
fn a_module_missing_an_export_is_refused_by_name() {
    let wat = INERT.replace(
        r#"(func (export "sbx_eval") (param i32 i32) (result i32) (i32.const 0))"#,
        "",
    );
    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();

    match sandbox.start(&document) {
        Err(SandboxError::MissingExport(name)) => assert!(name.contains("sbx_eval")),
        other => panic!("expected a missing export, got {other:?}"),
    }
}

#[test]
fn a_module_that_exports_an_allowlisted_name_as_a_global_is_refused() {
    let error = refusal(&importing("env", "host_emit_signal").replace(
        r#"(import "env" "host_emit_signal" (func (param i32 i32) (result i32)))"#,
        r#"(import "env" "host_emit_signal" (global i32))"#,
    ));
    assert!(
        matches!(error, SandboxError::CapabilityDenied(_)),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// 5. Cheap rejections: things that never reach the sandbox at all.
// ---------------------------------------------------------------------------

mod rejected_before_the_sandbox {
    use super::{DOCUMENT, MAX_DOCUMENT_BYTES, parse_and_validate};

    #[test]
    fn an_oversized_document_is_refused_by_size() {
        let huge = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(parse_and_validate(&huge).is_err());
    }

    #[test]
    fn a_document_with_too_many_conditions_is_refused() {
        // 200 conditions against a ceiling of 64. The spec calls this out by
        // name: "extremely large timeframes/condition list designed to exhaust
        // memory must be rejected by the validator's size limits before
        // reaching the sandbox".
        let conditions: Vec<String> = (0..200)
            .map(|i| format!(r#"{{"timeframe": "entry", "condition": "close > threshold({i})"}}"#))
            .collect();
        let document = DOCUMENT.replace(
            r#"{"timeframe": "entry", "condition": "close > threshold(0)"}"#,
            &conditions.join(","),
        );
        let error = parse_and_validate(&document).expect_err("too many conditions");
        assert!(error.to_string().contains("64"), "{error}");
    }

    #[test]
    fn a_document_declaring_too_many_timeframes_is_refused() {
        let declarations: Vec<String> = ["1m", "5m", "15m", "1h", "4h", "1d"]
            .iter()
            .enumerate()
            .map(|(i, tf)| format!(r#""tf{i}": "{tf}""#))
            .collect();
        let document = DOCUMENT.replace(r#""entry": "5m""#, &declarations.join(","));
        assert!(parse_and_validate(&document).is_err());
    }

    #[test]
    fn a_risk_ceiling_above_the_hard_limit_is_refused() {
        let document = DOCUMENT.replace("\"max_risk_pct\": 1.0", "\"max_risk_pct\": 80.0");
        let error = parse_and_validate(&document).expect_err("80% risk");
        assert!(error.to_string().contains("max_risk_pct"), "{error}");
    }

    #[test]
    fn non_finite_numbers_cannot_get_in() {
        // JSON has no NaN or Infinity literal, so the parse must fail rather
        // than produce a non-finite f64 that later reaches position sizing.
        for literal in ["NaN", "Infinity", "-Infinity", "1e400", "-1e400"] {
            let document = DOCUMENT.replace(
                r#""condition": "close > threshold(0)""#,
                &format!(r#""condition": "close > threshold({literal})""#),
            );
            assert!(
                parse_and_validate(&document).is_err(),
                "`{literal}` should not be an acceptable number"
            );
        }
    }

    #[test]
    fn a_condition_naming_an_undeclared_timeframe_is_refused() {
        let document = DOCUMENT.replace(
            r#""timeframe": "entry", "condition": "close > threshold(0)""#,
            r#""timeframe": "nowhere", "condition": "close > threshold(0)""#,
        );
        assert!(parse_and_validate(&document).is_err());
    }

    #[test]
    fn a_hallucinated_field_is_refused_rather_than_ignored() {
        // `deny_unknown_fields` is what turns an LLM's invented key into a
        // refusal instead of a silently ignored line.
        let document =
            DOCUMENT.replace(r#""version": "1","#, r#""version": "1", "moon_phase": 7,"#);
        assert!(parse_and_validate(&document).is_err());
    }
}

// ---------------------------------------------------------------------------
// 6. The channels a guest writes through are bounded.
// ---------------------------------------------------------------------------

#[test]
fn a_guest_cannot_flood_the_host_with_signals() {
    let signal = serde_json::to_vec(&sample_signal()).expect("serializable");
    let wat = format!(
        r#"
(module
  (import "env" "host_emit_signal" (func $emit (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 2048) "{data}")
  (func (export "sbx_abi_version") (result i32) (i32.const 1))
  (func (export "sbx_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "sbx_free") (param i32 i32))
  (func (export "sbx_init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_eval") (param i32 i32) (result i32)
    (local $i i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_u (local.get $i) (i32.const 50)))
        (drop (call $emit (i32.const 2048) (i32.const {len})))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (i32.const 0))
  (func (export "sbx_error_ptr") (result i32) (i32.const 0))
  (func (export "sbx_error_len") (result i32) (i32.const 0))
)
"#,
        data = escaped(&signal),
        len = signal.len(),
    );

    let limits = SandboxLimits::default();
    let sandbox = Sandbox::from_wasm(wat.as_bytes(), limits).expect("accepted");
    let document = parse_and_validate(DOCUMENT).unwrap();
    let mut session = sandbox.start(&document).expect("ABI is satisfied");

    let signals = session
        .evaluate(&context())
        .expect("the module returns cleanly");

    assert_eq!(
        signals.len(),
        limits.max_signals,
        "the host should keep exactly its ceiling of signals"
    );
    assert!(
        !session.refusals().is_empty(),
        "the flood should have been recorded, not silently dropped"
    );
}

#[test]
fn a_guest_can_use_the_scoped_store_and_cannot_see_another_instances() {
    let wat = format!(
        r#"
(module
  (import "env" "host_state_set" (func $set (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 2048) "{key}{value}")
  (func (export "sbx_abi_version") (result i32) (i32.const 1))
  (func (export "sbx_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "sbx_free") (param i32 i32))
  (func (export "sbx_init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_eval") (param i32 i32) (result i32)
    (drop (call $set (i32.const 2048) (i32.const {key_len})
                     (i32.const {value_at}) (i32.const {value_len})))
    (i32.const 0))
  (func (export "sbx_error_ptr") (result i32) (i32.const 0))
  (func (export "sbx_error_len") (result i32) (i32.const 0))
)
"#,
        key = escaped(b"probe"),
        value = escaped(b"value"),
        key_len = 5,
        value_at = 2048 + 5,
        value_len = 5,
    );

    let sandbox = sandbox_of(&wat);
    let document = parse_and_validate(DOCUMENT).unwrap();

    let mut first = sandbox.start(&document).expect("ABI is satisfied");
    first.evaluate(&context()).expect("the write is allowed");
    assert_eq!(
        first.scoped_state().get("probe").map(String::as_str),
        Some("value"),
        "the allowlisted scoped store should be reachable"
    );

    // A second instance starts empty: the store is scoped to the strategy, not
    // shared between them.
    let second = sandbox.start(&document).expect("ABI is satisfied");
    assert!(
        second.scoped_state().is_empty(),
        "one instance's state leaked into another"
    );
}

#[test]
fn the_scoped_store_is_bounded() {
    let wat = format!(
        r#"
(module
  (import "env" "host_state_set" (func $set (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 2048) "{value}")
  (func (export "sbx_abi_version") (result i32) (i32.const 1))
  (func (export "sbx_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "sbx_free") (param i32 i32))
  (func (export "sbx_init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "sbx_eval") (param i32 i32) (result i32)
    (local $i i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_u (local.get $i) (i32.const 10)))
        (drop (call $set (i32.const 2048) (i32.const 1)
                         (i32.const 2048) (i32.const {len})))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (i32.const 0))
  (func (export "sbx_error_ptr") (result i32) (i32.const 0))
  (func (export "sbx_error_len") (result i32) (i32.const 0))
)
"#,
        // 8 KiB, twice the per-value ceiling.
        value = escaped(&vec![b'x'; 8192]),
        len = 8192,
    );

    let limits = SandboxLimits::default();
    let sandbox = Sandbox::from_wasm(wat.as_bytes(), limits).expect("accepted");
    let document = parse_and_validate(DOCUMENT).unwrap();
    let mut session = sandbox.start(&document).expect("ABI is satisfied");

    session
        .evaluate(&context())
        .expect("the module returns cleanly");

    assert!(
        session.scoped_state().is_empty(),
        "an oversized value should have been refused, not stored"
    );
    assert!(
        session.refusals().iter().any(|r| r.contains("exceeds")),
        "the refusal should say which ceiling was hit: {:?}",
        session.refusals()
    );
}

/// A signal the host will accept, so the flood test fails on the ceiling rather
/// than on a parse error.
fn sample_signal() -> strategy_runtime::signal::Signal {
    use strategy_runtime::signal::{EnterSignal, Signal};
    Signal::Enter(EnterSignal {
        direction: strategy_dsl::Direction::Long,
        reference_price: 100.0,
        stop_price: 99.0,
        take_profit_price: None,
        max_risk_pct: 1.0,
        reasons: vec!["probe".into()],
    })
}

/// Render bytes as a WAT string body, one `\xx` escape per byte.
///
/// Necessary because a signal's JSON contains quotes and braces, and WAT string
/// literals have their own escaping rules.
fn escaped(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 4);
    for byte in bytes {
        let _ = write!(out, "\\{byte:02x}");
    }
    out
}
