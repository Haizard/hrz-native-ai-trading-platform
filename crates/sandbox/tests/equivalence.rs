//! The Phase 4 exit criterion: the same strategy runs **identically** natively
//! and inside the WASM sandbox.
//!
//! ## Why the comparison is the backtester's own replay loop
//!
//! Both runs go through `backtester::replay`. The only difference is the
//! `Strategy` implementation handed to it -- `StrategyEngine` on one side,
//! `SandboxedStrategy` on the other. That matters: if the sandbox had a replay
//! path of its own, "identical" would mean "two implementations agree", and the
//! two could drift the next time either changed. This way there is one loop, and
//! the sandbox is a different way of driving it.
//!
//! ## Why every synthetic document must actually trade
//!
//! `assert_eq!` on two empty trade lists passes. A fixture that never fires
//! would prove nothing while looking green, so each synthetic document also
//! asserts it produced trades, and [`the_fixture_contains_the_setups_it_claims_to`]
//! checks that the series really does contain the setups the documents look for.
//!
//! The two shipped documents are compared for equality only. Whether they fire
//! depends on the data rather than on the sandbox -- and a document that does
//! *nothing* is still worth comparing, since a sandbox that invented a signal
//! the native path never emitted would otherwise go unnoticed.

use std::collections::BTreeMap;

use analytics_core::resample::resample;
use analytics_core::state::{build_market_state, MarketStateConfig};
use analytics_core::types::{Candle, Timeframe};
use backtester::replay::{replay, ReplayConfig, ReplayInput, ReplayOutput};
use sandbox::{Sandbox, SandboxedStrategy};
use strategy_dsl::{parse_and_validate, StrategyDocument, ValidatedStrategy};
use strategy_runtime::{RuntimeConfig, StrategyEngine};

/// Bars in the synthetic 5m series.
///
/// Long enough for a 1h context timeframe to have a history worth reading, and
/// for the pattern below to repeat often enough to trade. Kept in the low
/// thousands because every bar costs one crossing of the sandbox boundary, and
/// the module has to deserialize two views per bar.
const BARS: i64 = 1_500;

/// Where the synthetic series starts, in unix nanoseconds.
const EPOCH: i64 = 1_700_000_000_000_000_000;

/// The pattern repeats every 30 bars.
const CYCLE: i64 = 30;

/// A rising zig-zag that periodically wicks through its own last swing low.
///
/// ## Why not a simple climb
///
/// A monotonic series has no local maxima at all, so `analytics-core`'s
/// structure detector finds no swing highs and reports `Ranging` forever -- and
/// with rising lows and no sweeps, `liquidity.swept` never fires either. A
/// fixture like that would run 1,500 candles through the sandbox while
/// exercising almost none of the interpreter, and the equivalence test would
/// pass without meaning anything.
///
/// ## The shape
///
/// Each cycle climbs to a swing high, falls to a swing low, climbs higher, falls
/// to a *higher* low, climbs again, then takes a sharp wick below that higher low
/// and closes back above it. That last bar is the setup the Phase 3 strategy is
/// named after: liquidity swept, then reclaimed.
fn five_minute_series(bars: i64) -> Vec<Candle> {
    let width = Timeframe::M5.nanos();
    let mut candles = Vec::with_capacity(bars as usize);
    let mut price = 100.0_f64;

    for i in 0..bars {
        let open = price;
        let phase = i % CYCLE;

        // (high, low, close) relative to the open.
        let (high, low, close) = match phase {
            0..=5 => (0.9, -0.1, 0.8),
            6..=10 => (0.1, -0.9, -0.8),
            11..=16 => (0.9, -0.1, 0.8),
            17..=21 => (0.1, -0.9, -0.8),
            22..=26 => (0.9, -0.1, 0.8),
            // The wick: deep enough to clear the higher low formed at phase 21,
            // with the close back above it.
            27 => (0.2, -4.5, -0.2),
            _ => (1.2, -0.1, 1.1),
        };

        let volume = 8_000.0 + f64::from(u32::try_from(phase).unwrap_or(0)) * 200.0;
        let buy_volume = volume * 0.62;
        let candle = Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: EPOCH + i * width,
            open,
            high: open + high,
            low: open + low,
            close: open + close,
            volume,
            buy_volume,
            sell_volume: volume - buy_volume,
        };
        price = candle.close;
        candles.push(candle);
    }

    candles
}

/// Build a replay input for whatever timeframes `document` declares, by
/// resampling one 5m series up to each of them.
fn input_for(document: &StrategyDocument) -> ReplayInput {
    let base = five_minute_series(BARS);
    let mut candles = BTreeMap::new();
    for (name, timeframe) in &document.timeframes {
        candles.insert(name.clone(), resample(&base, *timeframe));
    }
    ReplayInput::new(document, candles).expect("every declared timeframe has a series")
}

fn config() -> ReplayConfig {
    ReplayConfig {
        // Sent explicitly rather than left to `Default`, and the guest is given
        // the same value, so a divergence in buffer size cannot masquerade as a
        // sandbox bug.
        runtime: RuntimeConfig::default(),
        ..ReplayConfig::default()
    }
}

/// Replay natively, keeping the engine so a failure can say *why* nothing fired.
fn replay_natively(
    document: &ValidatedStrategy,
    input: &ReplayInput,
) -> (ReplayOutput, StrategyEngine) {
    let mut engine =
        StrategyEngine::new(document, RuntimeConfig::default()).expect("the fixture is tradable");
    let output = replay(&mut engine, input, &config()).expect("replay succeeds");
    (output, engine)
}

/// The conditions that were evaluated and not met, most common first.
///
/// A fixture that never trades is a fixture that proves nothing, and "zero
/// trades" on its own does not say which condition was the blocker.
fn why_nothing_fired(engine: &StrategyEngine) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skip in engine.skips() {
        *counts
            .entry(format!("{} [{}]", skip.reason, skip.timeframe))
            .or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(6)
        .map(|(reason, count)| format!("{count}x {reason}"))
        .collect()
}

fn replay_sandboxed(
    sandbox: &Sandbox,
    document: &ValidatedStrategy,
    input: &ReplayInput,
) -> ReplayOutput {
    let mut strategy =
        SandboxedStrategy::new(sandbox, document).expect("the sandbox accepts the document");
    let output = replay(&mut strategy, input, &config()).expect("replay succeeds");

    assert!(
        strategy.is_clean(),
        "the sandboxed run was not clean: errors {:?}, denials {:?}",
        strategy.errors(),
        strategy.denials()
    );
    assert!(
        strategy.refusals().is_empty(),
        "the host refused a guest write: {:?}",
        strategy.refusals()
    );
    assert_eq!(
        strategy.evaluations(),
        output.candles_processed,
        "the sandbox was asked about a different number of candles than the replay processed"
    );

    output
}

/// Documents chosen to exercise different parts of the interpreter.
///
/// The context condition is deliberately trivial. Its job is to make the run
/// *multi-timeframe*, which is what exercises the warm-up path, the host's
/// declared-name check and the coarser view crossing the boundary -- not to test
/// `analytics-core`'s trend detector, which has its own tests and which the two
/// shipped documents exercise against real data.
const DOCUMENTS: &[(&str, &str)] = &[
    (
        "trend + vwap, stop from recent history",
        r#"{
            "name": "Trend pullback",
            "version": "1",
            "kind": "strategy",
            "market": "BTCUSDT",
            "timeframes": {"context": "1h", "entry": "5m"},
            "entry": {"all_of": [
                {"timeframe": "context", "condition": "close > threshold(0)"},
                {"timeframe": "entry", "condition": "close > vwap"},
                {"timeframe": "entry", "condition": "delta > threshold(5)"}
            ]},
            "risk": {
                "max_risk_pct": 1.0,
                "stop": {"kind": "below_recent_low", "bars": 20},
                "take_profit": {"type": "risk_multiple", "value": 1.5}
            },
            "invalidation": [{"timeframe": "entry", "condition": "close_below(vwap)"}]
        }"#,
    ),
    (
        "liquidity sweep, stop at the swept level",
        r#"{
            "name": "Sweep reclaim",
            "version": "1",
            "kind": "strategy",
            "market": "BTCUSDT",
            "timeframes": {"context": "1h", "entry": "5m"},
            "entry": {"all_of": [
                {"timeframe": "context", "condition": "close > threshold(0)"},
                {"timeframe": "entry", "condition": "liquidity.swept == \"sell_side\""},
                {"timeframe": "entry", "condition": "close > liquidity.swept_level"}
            ]},
            "risk": {
                "max_risk_pct": 1.0,
                "stop": "below_sweep_low",
                "take_profit": {"type": "risk_multiple", "value": 2.5}
            },
            "invalidation": [{"timeframe": "entry", "condition": "close_below(stop_price)"}]
        }"#,
    ),
    (
        "volume profile, ATR stop, a position-scoped invalidation",
        r#"{
            "name": "Value-area continuation",
            "version": "1",
            "kind": "strategy",
            "market": "BTCUSDT",
            "timeframes": {"context": "1h", "entry": "5m"},
            "entry": {
                "direction": "long",
                "all_of": [
                    {"timeframe": "context", "condition": "close > threshold(0)"},
                    {"timeframe": "entry", "condition": "close > poc"},
                    {"timeframe": "entry", "condition": "buy_volume > sell_volume"}
                ]
            },
            "risk": {
                "max_risk_pct": 2.0,
                "stop": {"kind": "atr", "multiple": 1.5, "period": 14}
            },
            "invalidation": [
                {"timeframe": "entry", "condition": "close_below(vwap)"},
                {"timeframe": "entry", "condition": "bars_in_trade > 12"}
            ]
        }"#,
    ),
];

#[test]
fn the_fixture_contains_the_setups_it_claims_to() {
    // A guard against the generator quietly degenerating into a series that has
    // no sweeps and no structure -- which would leave the equivalence test green
    // and hollow. This is the check that would have caught the first version of
    // this fixture, whose highs rose monotonically so `swing_highs` was empty
    // and every document was a no-op.
    let base = five_minute_series(BARS);
    let mut swept = 0;
    let mut reclaimed = 0;
    let mut structured = 0;

    for end in 500..base.len() {
        let Some(state) =
            build_market_state(&base[end - 500..=end], &[], &MarketStateConfig::default())
        else {
            continue;
        };
        if !state.swing_highs.is_empty() && !state.swing_lows.is_empty() {
            structured += 1;
        }
        let newest_swept_low = state
            .liquidity
            .iter()
            .filter(|level| level.swept && !level.kind.is_above())
            .map(|level| level.price)
            .reduce(f64::max);
        if let Some(level) = newest_swept_low {
            swept += 1;
            if state.price > level {
                reclaimed += 1;
            }
        }
    }

    assert!(
        structured > 0,
        "the fixture has no market structure to read"
    );
    assert!(swept > 0, "the fixture never sweeps sell-side liquidity");
    assert!(
        reclaimed > 0,
        "the fixture never reclaims a swept level, so the sweep documents cannot fire"
    );
}

#[test]
fn every_document_produces_identical_trades_natively_and_in_the_sandbox() {
    let sandbox = Sandbox::new().expect("the embedded guest compiles");

    for (name, source) in DOCUMENTS {
        let document = parse_and_validate(source).unwrap_or_else(|e| panic!("{name}: {e}"));
        let input = input_for(document.document());

        let (native, engine) = replay_natively(&document, &input);
        let sandboxed = replay_sandboxed(&sandbox, &document, &input);

        assert_eq!(
            native, sandboxed,
            "{name}: the sandboxed run diverged from the native one"
        );
        assert!(
            !native.trades.is_empty(),
            "{name}: the fixture never traded, so identical results prove nothing.\n  {}",
            why_nothing_fired(&engine).join("\n  ")
        );
    }
}

#[test]
fn the_shipped_phase_three_strategy_runs_identically_in_the_sandbox() {
    let source = include_str!("../../../strategies/liquidity-sweep-btcusdt-5m.yaml");
    let document = parse_and_validate(source).expect("the shipped strategy is valid");
    let input = input_for(document.document());

    let (native, _) = replay_natively(&document, &input);
    let sandboxed = replay_sandboxed(&sandbox(), &document, &input);

    assert!(
        native.candles_processed > 0,
        "the replay did nothing at all"
    );
    assert_eq!(native, sandboxed);
}

#[test]
fn the_specs_unmodified_sample_document_runs_identically_too() {
    // `strategies/liquidity-sweep.yaml` is the document from docs/06 verbatim,
    // threshold and all.
    let source = include_str!("../../../strategies/liquidity-sweep.yaml");
    let document = parse_and_validate(source).expect("the spec's document is valid");
    let input = input_for(document.document());

    let (native, _) = replay_natively(&document, &input);
    let sandboxed = replay_sandboxed(&sandbox(), &document, &input);

    assert!(
        native.candles_processed > 0,
        "the replay did nothing at all"
    );
    assert_eq!(native, sandboxed);
}

#[test]
fn a_sandboxed_run_consumes_real_fuel_and_reads_real_data() {
    // `is_clean()` on its own would also be satisfied by a sandbox that never
    // ran anything, so the run is checked for evidence that it did.
    let sandbox = Sandbox::new().expect("the embedded guest compiles");
    let (name, source) = DOCUMENTS[1];
    let document = parse_and_validate(source).expect(name);
    let input = input_for(document.document());

    let mut strategy = SandboxedStrategy::new(&sandbox, &document).expect("accepted");
    let output = replay(&mut strategy, &input, &config()).expect("replay succeeds");

    assert!(strategy.is_clean(), "{:?}", strategy.errors());
    assert_eq!(strategy.evaluations(), output.candles_processed);
    assert!(
        strategy.usage().fuel_consumed > 0,
        "a run that consumed no fuel did not execute anything"
    );
    assert!(
        strategy.signals() > 0,
        "the interpreter never emitted a signal through the host boundary"
    );
    assert!(
        strategy.usage().memory_bytes > 0,
        "the instance reported no linear memory"
    );
}

fn sandbox() -> Sandbox {
    Sandbox::new().expect("the embedded guest compiles")
}
