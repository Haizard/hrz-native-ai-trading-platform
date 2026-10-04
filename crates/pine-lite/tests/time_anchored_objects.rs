//! docs/28: time-anchored drawing objects — `line.new_time` /
//! `label.new_time` / `box.new_time`, plus `request.time()` — let a script
//! that read a pooled higher timeframe (Phase 11) DRAW what it found there
//! onto this chart's time axis. Anchors are unix-nanos timestamps riding in
//! f64, the same convention as the builtin `time` series.

use analytics_core::types::{Candle, Timeframe};
use pine_lite::{run, vet, Inputs};

fn chart_candle(i: usize) -> Candle {
    Candle {
        symbol: "BTCUSDT".into(),
        timeframe: Timeframe::M1,
        open_time: i as i64 * 60_000_000_000,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.0,
        volume: 10.0,
        buy_volume: 5.0,
        sell_volume: 5.0,
    }
}

#[test]
fn the_time_variants_vet_and_arity_is_checked() {
    let src = concat!(
        "//@pine_lite version=1 overlay=true\n",
        "if bar_index == 0\n",
        "    box.new_time(time, 104.0, time + 1000000000000000.0, 102.0, color=color.teal)\n",
        "    line.new_time(time, 100.0, time + 60000000000.0, 101.0, color=color.blue)\n",
        "    label.new_time(time, 105.0, \"t\", color=color.red)\n",
        "plot(close)\n",
    );
    assert!(vet(src).is_ok(), "the _time builtins must vet");
    let short = concat!(
        "//@pine_lite version=1 overlay=true\n",
        "if bar_index == 0\n",
        "    box.new_time(time, 104.0, 102.0)\n",
        "plot(close)\n",
    );
    assert!(vet(short).is_err(), "three args is not a box");
}

#[test]
fn request_time_reads_the_aligned_instruments_open_time() {
    // sec= aligns a second instrument onto this chart's bars; its open_time
    // is NOT the chart's. A change in request.time() is the other market's
    // new bar -- the boundary detector the MTF idiom is built on.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"T\" sec=\"ETHUSDT\"\n",
        "plot(request.time())\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let candles: Vec<Candle> = (0..10).map(chart_candle).collect();
    // The aligned instrument's bars are 5 chart bars wide.
    let security: Vec<Candle> = (0..10)
        .map(|i| {
            let mut c = chart_candle(i);
            c.symbol = "ETHUSDT".into();
            c.open_time = (i as i64 / 5) * 5 * 60_000_000_000;
            c
        })
        .collect();
    let inputs = Inputs { security, ..Inputs::default() };
    let output = run(&parsed, &candles, &inputs).expect("run");
    let t5 = output.plots[0].values[5];
    let t6 = output.plots[0].values[6];
    assert_eq!(t5, 5.0 * 60_000_000_000.0, "bar 5 sits in the aligned bar that starts at 5");
    assert_eq!(t6, t5, "bar 6 is the same aligned bar");
    assert_ne!(output.plots[0].values[4], t5, "bar 4 is the previous aligned bar");
}

#[test]
fn the_htf_fvg_idiom_draws_one_time_anchored_box() {
    // The taught idiom (docs/28): read the pooled 1h series, detect its bar
    // boundaries by the change in its own time, keep the last two closed
    // bars' levels in vars, and when the just-closed bar gaps above the high
    // two closed bars back, draw the gap from the closing bar's open time to
    // a far-future right edge.
    //
    // The pool arrives aligned: one slot per CHART bar, the covering 1h
    // bar's values -- the host's carry-forward, constructed here by hand.
    // 1h bars B0..B3 cover chart bars 0-9, 10-19, 20-29, 30-39.
    //   B0: high 100, low 90
    //   B1: high 110, low 95   (the impulse)
    //   B2: high 115, low 105  (gaps above B0's high -> bullish FVG)
    //   B3: whatever follows
    let src = concat!(
        "//@pine_lite version=1 overlay=true title=\"HTF FVG\"\n",
        "ht = request.security(\"BTCUSDT\", \"1h\", time)\n",
        "hh = request.security(\"BTCUSDT\", \"1h\", high)\n",
        "hl = request.security(\"BTCUSDT\", \"1h\", low)\n",
        "newbar = ht != ht[1]\n",
        "var ph1 = na\n",
        "var ph2 = na\n",
        "if newbar and not na(ph2)\n",
        "    if hl[1] > ph2\n",
        "        box.new_time(ht[1], hl[1], ht + 10000000000000000.0, ph2, color=color.teal)\n",
        "if newbar\n",
        "    ph2 = ph1\n",
        "    ph1 = hh[1]\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let candles: Vec<Candle> = (0..40).map(chart_candle).collect();
    let pool_levels: [(f64, f64); 4] = [(100.0, 90.0), (110.0, 95.0), (115.0, 105.0), (120.0, 110.0)];
    let pool: Vec<Candle> = (0..40)
        .map(|i| {
            let b = i / 10;
            let (high, low) = pool_levels[b];
            let mut c = chart_candle(i);
            c.open_time = (b as i64) * 10 * 60_000_000_000;
            c.high = high;
            c.low = low;
            c
        })
        .collect();
    let mut series_pool = std::collections::HashMap::new();
    series_pool.insert("BTCUSDT@1H".to_string(), pool);
    let inputs = Inputs { series_pool, ..Inputs::default() };
    let output = run(&parsed, &candles, &inputs).expect("run");
    let boxes: Vec<_> = output
        .objects
        .iter()
        .filter_map(|o| match o {
            pine_lite::interp::ScriptObject::BoxTime { left_nanos, top, right_nanos, bottom, .. } => {
                Some((*left_nanos, *top, *right_nanos, *bottom))
            }
            _ => None,
        })
        .collect();
    assert_eq!(boxes.len(), 1, "exactly one HTF gap qualifies: {boxes:?}");
    let (left, top, right, bottom) = boxes[0];
    // The zone spans B2's birth (chart bar 20's 1h bar open = 20 chart bars
    // of nanos) to a far-future edge, at [B0.high, B2.low].
    assert_eq!(left, 20.0 * 60_000_000_000.0, "the zone is born with the gapping 1h bar");
    assert_eq!(top, 105.0, "the gap's top is the closing bar's low");
    assert_eq!(bottom, 100.0, "the gap's bottom is the high two closed bars back");
    assert!(right > 1e16, "the right edge extends into the future: {right}");
}
