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
    let mut prev_c = 0.0_f64;
    // `sess` needs bar i to open at hour (i mod 24) UTC: t0 = 2023-11-14
    // 00:00 UTC. The others only need spacing.
    let t0 = if scenario == "sess" || scenario == "mtf" || scenario == "smt" {
        1_699_920_000_000_000_000_i64
    } else {
        1_700_000_000_000_000_000_i64
    };
    for i in 0..200i64 {
        let c = if scenario == "mtf" {
            // Chart (1h) structure for MTF tests: a CHoCH up into the rally
            // (~bar 42), a CHoCH down (~75), a CHoCH up (~105) that a 4h
            // downtrend filter must REJECT, then drift.
            match i {
                0..=39 => 100.0 + i as f64 * 0.075 + 0.3 * (i as f64 * std::f64::consts::TAU / 16.0).sin(),
                40..=68 => 103.0 + (i - 39) as f64 * 0.245,
                69..=98 => 110.1 - (i - 68) as f64 * 0.2,
                99..=130 => 104.1 + (i - 98) as f64 * 0.09,
                _ => 107.0 - (i - 130) as f64 * 0.045 + 0.3 * ((i - 130) as f64 * std::f64::consts::TAU / 20.0).sin(),
            }
        } else if scenario == "smt" {
            // BTC for SMT divergence: higher swing high at 60 (106 vs 105 at
            // 30), lower swing low at 130 (97.5 vs 98 at 100). ETH (the
            // security path) diverges: lower high at 60, higher low at 130.
            match i {
                0..=30 => 100.0 + i as f64 * 0.1667,
                31..=45 => 105.0 - (i - 30) as f64 * 0.2667,
                46..=60 => 101.0 + (i - 45) as f64 * 0.3333,
                61..=75 => 106.0 - (i - 60) as f64 * 0.2667,
                76..=100 => 102.0 - (i - 75) as f64 * 0.16,
                101..=115 => 98.0 + (i - 100) as f64 * 0.1333,
                116..=130 => 100.0 - (i - 115) as f64 * 0.1667,
                _ => 97.5 + 0.5 * ((i - 130) as f64 * std::f64::consts::TAU / 24.0).sin() + (i - 130) as f64 * 0.05,
            }
        } else if scenario == "sweep" {
            // Liquidity sweep + CHoCH: equal highs at 103.3 (bars 8, 24),
            // equal lows at 97.6 (bars 16, 28), a sweep to 105.0 at 33 that
            // closes back below, a minor low at 101.1 (bar 40), a lower
            // high, then the CHoCH close under 101.1 at ~47 and a downtrend
            // into the 97.6 pool, then chop.
            match i {
                0..=8 => 100.0 + i as f64 * 0.375,
                9..=16 => 103.0 - (i - 8) as f64 * 0.625,
                17..=24 => 98.0 + (i - 16) as f64 * 0.625,
                25..=28 => 103.0 - (i - 24) as f64 * 1.25,
                29..=31 => 98.0 + (i - 28) as f64 * 1.0,
                // Single-bar sweep: high 103.4 pokes the 103.3 pool, close
                // 103.1 falls back below it.
                32 => 102.6,
                33 => 103.1,
                34 => 102.0,
                35..=40 => 102.0 - (i - 34) as f64 * 0.15,
                41..=44 => 101.1 + (i - 40) as f64 * 0.35,
                45..=60 => 102.5 - (i - 44) as f64 * 0.35,
                61..=90 => 96.9 + 0.4 * ((i - 60) as f64 * std::f64::consts::TAU / 15.0).sin(),
                _ => 97.0 + 0.5 * ((i - 90) as f64 * std::f64::consts::TAU / 20.0).sin(),
            }
        } else if scenario == "obfvg" {
            // OB + FVG confluence: downtrend with lower highs, a sharp
            // CHoCH rally out of 99 with a 3-candle FVG right above the last
            // down candle (the order block at bar 71), a pullback into the
            // overlap at ~100.3, then the rally leg that pays 2R.
            match i {
                0..=59 => 110.0 - i as f64 * 0.17 + 1.2 * (i as f64 * std::f64::consts::TAU / 18.0).sin(),
                60..=70 => 99.5 - (i - 59) as f64 * 0.045,
                71 => 100.0,
                72 => 100.6,
                73 => 101.2,
                74..=85 => 101.2 + (i - 73) as f64 * 0.8,
                86..=100 => 110.8 - (i - 85) as f64 * 0.68,
                101..=140 => 100.6 + (i - 100) as f64 * 0.15,
                _ => 106.6 + 0.5 * ((i - 140) as f64 * std::f64::consts::TAU / 20.0).sin(),
            }
        } else if scenario == "sess" {
            // London killzone: t0 is a Monday 00:00 UTC; every 24-bar day has
            // a tight Asian range (bars 0-7), a London impulse (bars 7-10,
            // alternating up/down days), then drift. Day offset keeps days
            // distinguishable.
            let day = i / 24;
            let hod = i % 24;
            let base = 100.0 + day as f64 * 0.8;
            let up_day = day % 2 == 0;
            match hod {
                0..=6 => base + 0.2 * (hod as f64 * std::f64::consts::TAU / 7.0).sin(),
                7..=10 => {
                    let dir = if up_day { 1.0 } else { -1.0 };
                    base + dir * (0.4 + (hod - 6) as f64 * 0.45)
                }
                _ => {
                    let dir = if up_day { 1.0 } else { -1.0 };
                    base + dir * 2.2 - dir * (hod - 10) as f64 * 0.08
                }
            }
        } else if scenario == "harm" {
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
        // Alternate the open side by parity: even bars close up, odd bars
        // close down, so down-close candles exist (order blocks need them).
        // The open is the PREVIOUS close: candle direction follows the
        // local trend (down-close candles in declines, up-close in rallies),
        // which is what order-block detection reads. Tight symmetric wicks.
        let o = if i == 0 { c } else { prev_c };
        prev_c = c;
        let h = c + 0.15;
        let l = c - 0.15;
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

    // Host-side MTF plumbing, mirroring the gateway's contract: the pool
    // holds one candle per CHART bar (the pooled candle whose window covers
    // it); the second instrument aligns 1:1.
    let inputs = if scenario == "mtf" {
        let raw_4h = build_4h_pool(t0);
        let mut series_pool = std::collections::HashMap::new();
        series_pool.insert("BTCUSDT@4H".to_string(), align_pooled(&candles, &raw_4h));
        pine_lite::interp::Inputs { series_pool, ..Inputs::default() }
    } else if scenario == "smt" {
        pine_lite::interp::Inputs { security: build_eth_path(&candles), ..Inputs::default() }
    } else {
        Inputs::default()
    };
    let inputs = pine_lite::interp::Inputs { mintick: Some(0.01), ..inputs };
    let out = pine_lite::run(&script, &candles, &inputs)?;
    if out.strategy_used {
        println!("STRATEGY intents: {}", out.intents.len());
        for (bar, intent) in &out.intents {
            println!("  bar {bar}: {intent:?}");
        }
        if let Some(sim) = &out.simulation {
            println!("SIM orders: {}", sim.orders.len());
            for o in &sim.orders {
                println!(
                    "  bar {} {:?} {} qty={:.4} px={:.2} stop={:?} pnl={:?}",
                    o.bar,
                    o.kind,
                    if o.long { "LONG" } else { "SHORT" },
                    o.qty,
                    o.price,
                    o.stop,
                    o.pnl.map(|p| format!("{p:.2}"))
                );
            }
            if let Some(last) = sim.snapshots.last() {
                println!(
                    "SIM final: equity={:.2} position={:.4}",
                    last.equity, last.position_size
                );
            }
        }
    }
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

/// The gateway's `align_security_pooled`, verbatim in spirit: for each chart
/// bar, the last pooled candle whose open_time is at or before the bar's.
fn align_pooled(primary: &[Candle], pooled: &[Candle]) -> Vec<Candle> {
    let first = pooled[0].close;
    let mut out = Vec::with_capacity(primary.len());
    let mut cursor = 0usize;
    for bar in primary {
        while cursor + 1 < pooled.len() && pooled[cursor + 1].open_time <= bar.open_time {
            cursor += 1;
        }
        let usable = pooled[cursor].open_time <= bar.open_time;
        let src = if usable { &pooled[cursor] } else { &pooled[0] };
        let fill = if usable { src.close } else { first };
        out.push(Candle {
            symbol: src.symbol.clone(),
            timeframe: src.timeframe,
            open_time: bar.open_time,
            open: if usable { src.open } else { fill },
            high: if usable { src.high } else { fill },
            low: if usable { src.low } else { fill },
            close: fill,
            volume: if usable { src.volume } else { 0.0 },
            buy_volume: if usable { src.buy_volume } else { 0.0 },
            sell_volume: if usable { src.sell_volume } else { 0.0 },
        });
    }
    out
}

/// Fifty engineered 4h candles: an uptrend with a down-close order block at
/// j=10 (chart bars 40-43) before the 4h break, a downtrend from j=17, then
/// a mild recovery -- the trend-filter windows the MTF strategy is checked
/// against.
fn build_4h_pool(t0: i64) -> Vec<Candle> {
    let mut out = Vec::new();
    let mut prev = 100.0;
    for j in 0..50 {
        let c = match j {
            0..=9 => 100.0 + j as f64 * 0.5 + 0.2 * (j as f64 * std::f64::consts::TAU / 8.0).sin(),
            // The order block: down-close candle, high 104.7 / low 103.6.
            10 => 103.8,
            11..=16 => 104.2 + (j - 10) as f64 * 0.9667,
            17..=36 => 110.0 - (j - 16) as f64 * 0.35,
            _ => 103.0 + (j - 36) as f64 * 0.15,
        };
        let (o, h, l) = if j == 10 {
            (104.5, 104.7, 103.6)
        } else {
            (prev, c + 0.15, c.min(prev) - 0.15)
        };
        out.push(Candle {
            symbol: "BTCUSDT".to_string(),
            timeframe: analytics_core::Timeframe::H4,
            open_time: t0 + j as i64 * 4 * 3_600_000_000_000,
            open: o,
            high: h.max(o).max(c),
            low: l.min(o).min(c),
            close: c,
            volume: 100.0,
            buy_volume: 60.0,
            sell_volume: 40.0,
        });
        prev = c;
    }
    out
}

/// ETH, bar-aligned 1:1 with the chart: the same swings EXCEPT a LOWER high
/// at bar 60 (104.5 vs BTC's 106 -> bearish SMT) and a HIGHER low at bar
/// 130 (98.2 vs BTC's 97.5 -> bullish SMT).
fn build_eth_path(chart: &[Candle]) -> Vec<Candle> {
    chart
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let c = match i {
                0..=30 => 100.0 + i as f64 * 0.1667,
                31..=45 => 105.0 - (i - 30) as f64 * 0.3333,
                46..=60 => 100.0 + (i - 45) as f64 * 0.3,
                61..=75 => 104.5 - (i - 60) as f64 * 0.3,
                76..=100 => 100.0 - (i - 75) as f64 * 0.08,
                101..=115 => 98.0 + (i - 100) as f64 * 0.1333,
                116..=130 => 100.0 - (i - 115) as f64 * 0.12,
                _ => 98.2 + 0.4 * ((i - 130) as f64 * std::f64::consts::TAU / 24.0).sin() + (i - 130) as f64 * 0.04,
            };
            Candle {
                symbol: "ETHUSDT".to_string(),
                timeframe: analytics_core::Timeframe::H1,
                open_time: b.open_time,
                open: c - 0.05,
                high: c + 0.15,
                low: c - 0.15,
                close: c,
                volume: 100.0,
                buy_volume: 50.0,
                sell_volume: 50.0,
            }
        })
        .collect()
}
