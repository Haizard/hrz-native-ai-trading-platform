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
    // 300 bars -> 300 labels wanted; the first MAX_OBJECTS (256) render and
    // the run SUCCEEDS with the truncation flagged (TradingView's
    // stop-drawing behavior).
    let output = run(&parsed, &candles(300), &Inputs::default()).expect("run");
    assert_eq!(output.objects.len(), pine_lite::interp::MAX_OBJECTS, "exactly the cap");
    assert!(output.objects_truncated, "the scene must learn it was cut");
}

#[test]
fn a_box_border_is_distinct_from_the_fill_and_optional() {
    // docs/31: TradingView's box look -- the border is its own color, width
    // and dash style, off when unasked. Both box forms take the same knobs.
    let src = concat!(
        "//@pine_lite version=1 overlay=true title=\"Draw\"\n",
        "if bar_index == 0\n",
        "    box.new(5, 103.0, 15, 98.0, color=color.green, border_color=color.lime, border_width=2, border_style=\"dashed\")\n",
        "    box.new_time(60000000000.0, 104.0, 900000000000.0, 97.0, color=color.red)\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    assert_eq!(output.objects.len(), 2, "{:?}", output.objects);
    match &output.objects[0] {
        pine_lite::interp::ScriptObject::Box { border_color, border_width, border_style, .. } => {
            assert_eq!(*border_color, Some(0xFF_84_CC_16), "color.lime packed");
            assert_eq!(*border_width, 2.0);
            assert_eq!(border_style, "dashed");
        }
        other => panic!("expected a box: {other:?}"),
    }
    match &output.objects[1] {
        pine_lite::interp::ScriptObject::BoxTime { border_color, border_width, border_style, .. } => {
            assert_eq!(*border_color, None, "no border asked, none drawn");
            assert_eq!(*border_width, 1.0, "the default width");
            assert_eq!(border_style, "solid");
        }
        other => panic!("expected a time-anchored box: {other:?}"),
    }
}
