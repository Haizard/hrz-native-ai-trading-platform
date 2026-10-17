//! Verification harness for generated pine-lite scripts: runs a script over
//! synthetic candles with KNOWN structure and prints the drawing heap, plots
//! and shapes, so generated geometry can be checked without a chart.
//!
//! Usage: `cargo run -p pine-lite --example run_generated <script.pine> [bos|tl]`
//!   bos (default): range + breakout + crash -- horizontal levels get broken.
//!   tl: rising support line broken down, falling resistance line broken up.

use analytics_core::types::Candle;
use pine_lite::Inputs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: run_generated <script-file> [bos|tl]");
    let scenario = std::env::args().nth(2).unwrap_or_else(|| "bos".to_string());
    let source = std::fs::read_to_string(path)?;
    let (_header, script) = pine_lite::vet(&source).map_err(|errs| {
        errs.iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    // 200 bars with deliberate structure:
    //   0..49   uptrend 90 -> 100
    //   50..99  clean range 100..105 (distinct pivots at 105 and 100)
    //   100..109 breakout to 111  -> crosses the 105 swing high (bull BOS)
    //   110..129 pullback, pivots low at 103
    //   130..149 crash to 95 -> crosses the 103 and 100 swing lows (bear breaks)
    //   150..199 recovery to 107 -> another bull break
    // Second scenario ("tl"): a rising trend with ascending swing lows, a
    // sharp dip THROUGH the rising low-line, and a recovery that reclaims
    // it -- engineered so a trendline-breakout script actually fires.
    let mut candles = Vec::new();
    let t0 = 1_700_000_000_000_000_000_i64;
    for i in 0..200i64 {
        let c = if scenario == "harm" {
            // Monotone XABCD zigzag (no wiggle, so pivots form exactly at the
            // turning points): X low at 20 (89.85), A high at 40 (110.15),
            // B low at 60 (97.49 -- 61.8% of XA), C high at 80 (105.43 --
            // 61.8% of AB), D low at 100 (94.13 -- 78.6% of XA): a textbook
            // bullish Gartley, then a rally so D can confirm.
            match i {
                0..=20 => 100.0 - i as f64 * 0.5,
                21..=40 => 90.0 + (i - 20) as f64 * 1.0,
                41..=60 => 110.0 - (i - 40) as f64 * 0.618,
                61..=80 => 97.64 + (i - 60) as f64 * 0.382,
                81..=100 => 105.28 - (i - 80) as f64 * 0.55,
                _ => 94.28 + (i - 100) as f64 * 0.4,
            }
        } else if scenario == "tl" {
            match i {
                // Rising trend, ascending swing lows (a support line forms).
                0..=99 => 90.0 + i as f64 * 0.35 + 2.5 * (i as f64 * std::f64::consts::TAU / 22.0).sin(),
                // Shallow dip that breaks DOWN through the rising line.
                100..=119 => 124.0 - (i - 99) as f64 * 0.8,
                // Downtrend, descending swing highs (a resistance line forms).
                120..=159 => 108.0 - (i - 119) as f64 * 0.4 + 2.5 * ((i - 119) as f64 * std::f64::consts::TAU / 22.0).sin(),
                // Rally that breaks UP through the falling line.
                _ => 92.0 + (i - 159) as f64 * 1.1,
            }
        } else {
            match i {
                0..=49 => 90.0 + i as f64 * 0.2,
                50..=99 => 102.5 + 2.5 * ((i - 50) as f64 * std::f64::consts::TAU / 16.0).sin(),
                100..=109 => 100.0 + (i - 99) as f64 * 1.2,
                110..=129 => 111.0 - (i - 109) as f64 * 0.4,
                130..=139 => 103.0 - (i - 129) as f64 * 0.9,
                _ => 95.0 + (i - 139) as f64 * 0.2,
            }
        };
        let o = c - 0.1;
        let h = c.max(o) + 0.3;
        let l = c.min(o) - 0.3;
        candles.push(Candle {
            symbol: "BTCUSDT".to_string(),
            timeframe: analytics_core::Timeframe::H1,
            open_time: t0 + i * 3_600_000_000_000,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 100.0 + 80.0 * (i as f64 * std::f64::consts::TAU / 17.0).sin().abs(),
            buy_volume: 60.0 + 50.0 * (i as f64 * std::f64::consts::TAU / 17.0).sin().abs(),
            sell_volume: 40.0 + 30.0 * (i as f64 * std::f64::consts::TAU / 11.0).cos().abs(),
        });
    }

    let out = pine_lite::run(&script, &candles, &Inputs::default())?;
    println!("objects: {} (truncated: {})", out.objects.len(), out.objects_truncated);
    for (i, obj) in out.objects.iter().enumerate() {
        match obj {
            pine_lite::ScriptObject::Line { bar1, price1, bar2, price2, .. } => {
                println!("[{i}] LINE ({bar1}, {price1:.2}) -> ({bar2}, {price2:.2})");
            }
            pine_lite::ScriptObject::Label { bar, price, text, .. } => {
                println!("[{i}] LABEL ({bar}, {price:.2}) \"{text}\"");
            }
            pine_lite::ScriptObject::Box { left, top, right, bottom, .. } => {
                println!("[{i}] BOX ({left}, {top:.2}) -> ({right}, {bottom:.2})");
            }
            other => println!("[{i}] {other:?}"),
        }
    }
    for plot in &out.plots {
        let sample: Vec<String> = [58usize, 100, 103, 104, 105, 190]
            .iter()
            .map(|&b| format!("{}={:.2}", b, plot.values.get(b).copied().unwrap_or(f64::NAN)))
            .collect();
        println!("PLOT '{}' samples: {}", plot.title, sample.join(" "));
    }
    println!("plotshape glyphs across window: {}", out.shapes.len());
    for shape in out.shapes.iter() {
        println!("  SHAPE bar={} value={:.2} glyph={}", shape.bar, shape.value, shape.glyph);
    }
    Ok(())
}
