//! Phase 13 (docs/23): `line.new` / `label.new` / `box.new` — anchored
//! drawing objects on a capped heap, positioned by the engine from bar/price
//! coordinates the script chose.

use pine_lite::{run, vet, Inputs};
use analytics_core::types::{Candle, Timeframe};

fn candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let close = 100.0 + (i as f64) * 0.5;
            Candle {
                symbol: "T".into(),
                timeframe: Timeframe::M1,
                open_time: i as i64 * 60_000_000_000,
                open: close,
                high: close + 1.0,
                low: close - 1.0,
                close,
                volume: 10.0,
                buy_volume: 5.0,
                sell_volume: 5.0,
            }
        })
        .collect()
}

#[test]
fn drawing_objects_collect_in_order() {
    let src = concat!(
        "//@pine_lite version=1 overlay=true title=\"Draw\"\n",
        "if bar_index == 0\n",
        "    line.new(0, 99.0, 20, 111.0, color=color.blue, style=\"dashed\", width=2)\n",
        "    label.new(10, 106.0, \"pivot\", color=color.red)\n",
        "    box.new(5, 103.0, 15, 98.0, color=color.green)\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    assert_eq!(output.objects.len(), 3, "{:?}", output.objects);
    match &output.objects[0] {
        pine_lite::interp::ScriptObject::Line { bar1, price1, bar2, price2, style, width, .. } => {
            assert_eq!(*bar1, 0.0);
            assert!((*price1 - 99.0).abs() < 1e-9, "low at bar 0 is 99: {price1}");
            assert_eq!(*bar2, 20.0);
            assert!((*price2 - 111.0).abs() < 1e-9, "high at bar 20 is 111: {price2}");
            assert_eq!(style, "dashed");
            assert_eq!(*width, 2.0);
        }
        other => panic!("expected a line: {other:?}"),
    }
    match &output.objects[1] {
        pine_lite::interp::ScriptObject::Label { bar, price, text, .. } => {
            assert_eq!(*bar, 10.0);
            assert!((*price - 106.0).abs() < 1e-9, "high at bar 10 is 106: {price}");
            assert_eq!(text, "pivot");
        }
        other => panic!("expected a label: {other:?}"),
    }
}

#[test]
fn objects_can_draw_on_conditions_only() {
    // The intended idiom: draw when a pivot fires, not every bar.
    let src = concat!(
        "//@pine_lite version=1 overlay=true title=\"Draw\"\n",
        "big = close > 108.0\n",
        "if big\n",
        "    label.new(bar_index, high, \"big\", color=color.green)\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    // Bars 17..29 have close > 108 (close = 100 + i*0.5 -> >108 at i>16).
    assert_eq!(output.objects.len(), 13, "one label per qualifying bar");
}

#[test]
fn the_object_heap_stops_at_the_cap_without_failing() {
    let src = concat!(
        "//@pine_lite version=1 overlay=true title=\"Draw\"\n",
        "label.new(bar_index, high, \"x\", color=color.red)\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    // 100 bars -> 100 labels wanted; the first 64 render and the run SUCCEEDS
    // with the truncation flagged (TradingView's stop-drawing behavior).
    let output = run(&parsed, &candles(100), &Inputs::default()).expect("run");
    assert_eq!(output.objects.len(), 64, "exactly the cap");
    assert!(output.objects_truncated, "the scene must learn it was cut");
}
