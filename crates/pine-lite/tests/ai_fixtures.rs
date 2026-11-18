//! Acceptance fixtures distilled from the trader-transcript prompts in
//! `indicator promt test/`.
//!
//! Each fixture is the "first stage" script the AI generation loop is expected
//! to produce from a real transcript: plotted zones, no trade execution. The
//! prompt2 fixture (gold scalping) is a deliberately different stress case
//! from prompt1: a lower timeframe, an entry model (sweep -> BOS -> order
//! block, then an inverse fair value gap) instead of a pure MTF overlay.
//!
//! These tests pin the request-to-script contract at the language layer. The
//! gateway generation gate and the chart-engine projection tests live with
//! their own crates.

use analytics_core::types::{Candle, Timeframe};
use pine_lite::interp::ScriptObject;

const GOLD_SCALP_REQUEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-fixtures/light-pine/gold_scalp_zones_request.txt"
));
const GOLD_SCALP_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-fixtures/light-pine/gold_scalp_zones.pine"
));

fn bar(open_time: i64, open: f64, high: f64, low: f64, close: f64, tf: Timeframe) -> Candle {
    Candle {
        symbol: "XAUUSD".into(),
        timeframe: tf,
        open_time,
        open,
        high,
        low,
        close,
        volume: 1.0,
        buy_volume: 0.5,
        sell_volume: 0.5,
    }
}

#[test]
fn the_request_is_a_distilled_ask_not_the_transcript() {
    let request = GOLD_SCALP_REQUEST.to_lowercase();
    assert!(request.contains("15m"), "the higher timeframe is named");
    assert!(request.contains("sweep"), "the sweep is named");
    assert!(request.contains("order block"), "the order block is named");
    assert!(request.contains("inverse"), "the inverse FVG is named");
    assert!(
        GOLD_SCALP_REQUEST.len() < 2_000,
        "a request, not a pasted transcript"
    );
}

#[test]
fn gold_scalp_zones_vets_and_draws_the_htf_block_and_the_inverse_gap() {
    let errs = pine_lite::vet(GOLD_SCALP_SCRIPT);
    assert!(
        errs.is_ok(),
        "{}",
        errs.expect_err("vet")
            .iter()
            .map(|e| format!("line {} col {}: {}", e.span.line, e.span.col, e.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (_, parsed) = pine_lite::vet(GOLD_SCALP_SCRIPT).ok().unwrap();

    // 60 chart bars at 5m; a 15m pool bar therefore covers three of them.
    let start = 1_700_000_000_000_000_000i64;
    let bar_nanos = 300_000_000_000i64;
    let mut candles: Vec<Candle> = (0..60)
        .map(|i| {
            bar(
                start + i as i64 * bar_nanos,
                100.0,
                101.0,
                99.0,
                100.0,
                Timeframe::M5,
            )
        })
        .collect();
    // Region A: a bearish fair value gap at bar 12 (high below bar 10's low)
    // whose top is reclaimed by the close of bar 20.
    candles[10] = bar(
        start + 10 * bar_nanos,
        110.0,
        111.0,
        110.0,
        110.5,
        Timeframe::M5,
    );
    candles[11] = bar(
        start + 11 * bar_nanos,
        110.5,
        111.5,
        110.2,
        111.0,
        Timeframe::M5,
    );
    candles[12] = bar(
        start + 12 * bar_nanos,
        108.0,
        109.5,
        107.0,
        108.5,
        Timeframe::M5,
    );
    for i in 13..20 {
        candles[i] = bar(
            start + i as i64 * bar_nanos,
            108.0,
            108.5,
            107.0,
            108.0,
            Timeframe::M5,
        );
    }
    candles[20] = bar(
        start + 20 * bar_nanos,
        108.0,
        113.5,
        107.5,
        113.0,
        Timeframe::M5,
    );

    // Region B (15m): bars 0-2 set the prior range, bar 3 is the down-close
    // order block, bar 4 sweeps its low and closes back above it, bar 5
    // closes above the prior three highs and confirms the structure break.
    let htf: [(f64, f64, f64, f64); 20] = [
        (100.0, 102.0, 98.0, 100.5),
        (100.5, 103.0, 100.0, 101.0),
        (101.0, 103.5, 100.5, 101.5),
        (101.5, 102.0, 99.0, 100.0),
        (100.0, 101.0, 96.0, 101.0),
        (101.0, 104.5, 100.5, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
        (104.0, 105.0, 103.0, 104.0),
    ];
    let pool15: Vec<Candle> = (0..60)
        .map(|i| {
            let period = i / 3;
            let (o, h, l, c) = htf[period];
            bar(
                start + (period * 3) as i64 * bar_nanos,
                o,
                h,
                l,
                c,
                Timeframe::M15,
            )
        })
        .collect();

    let mut series_pool = std::collections::HashMap::new();
    series_pool.insert("XAUUSD@15M".to_string(), pool15);
    let inputs = pine_lite::Inputs {
        series_pool,
        ..pine_lite::Inputs::default()
    };

    let output = pine_lite::run(&parsed, &candles, &inputs).expect("the gold fixture runs");

    let boxes: Vec<&ScriptObject> = output
        .objects
        .iter()
        .filter(|o| matches!(o, ScriptObject::BoxTime { .. }))
        .collect();
    assert_eq!(
        boxes.len(),
        2,
        "one higher-timeframe block and one inverse gap: {:?}",
        output.objects
    );

    let labels: Vec<&str> = output
        .objects
        .iter()
        .filter_map(|o| match o {
            ScriptObject::LabelTime { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["HTF OB", "IFVG"],
        "each zone names its role, in detection order"
    );

    // The block is anchored at the order-block bar (15m period 3) and spans
    // that candle's high/low, not the sweep or the break bar.
    match boxes[0] {
        ScriptObject::BoxTime {
            left_nanos,
            top,
            bottom,
            ..
        } => {
            assert_eq!(
                *left_nanos as i64,
                start + 9 * bar_nanos,
                "period 3 starts at bar 9"
            );
            assert_eq!(*top, 102.0);
            assert_eq!(*bottom, 99.0);
        }
        other => panic!("expected a time-anchored box, got {other:?}"),
    }
}
