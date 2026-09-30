//! Strategy execution (docs/24 S1): header knobs, the in-VM simulation, and
//! the two-pass account reads. The contract the preview JSON and the chart
//! will consume.

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

const LONG_STRATEGY: &str = concat!(
    "//@pine_lite version=1 overlay=false title=\"Long exit\" ",
    "strategy(initial_capital=10000, default_qty_type=\"percent_of_equity\", default_qty_value=10, commission_value=0)\n",
    "up = ta.crossover(close, ta.sma(close, 5))\n",
    "down = ta.crossunder(close, ta.sma(close, 5))\n",
    "if up\n",
    "    strategy.entry(\"long\", direction=\"long\")\n",
    "if down\n",
    "    strategy.close_all()\n",
    "plot(close)\n",
);

#[test]
fn a_strategy_script_simulates_orders_equity_and_report() {
    let (header, parsed) = vet(LONG_STRATEGY).expect("the strategy vets");
    assert!(header.strategy.is_some(), "the strategy(...) block parses");
    // A cycling fixture: a linearly rising close crosses its SMA once and
    // never again, which would make this a zero-trade run.
    let fixture: Vec<Candle> = (0..60)
        .map(|i| {
            let close = 100.0 + (i % 9) as f64;
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
        .collect();
    let output = run(&parsed, &fixture, &Inputs::default()).expect("run");
    let sim = output.simulation.expect("a strategy run carries its simulation");
    assert!(!sim.orders.is_empty(), "the crossover fires in a cycling fixture: {sim:?}");
    // Every fill is the next open after its decision bar (fixture: open == close).
    for (i, order) in sim.orders.iter().enumerate() {
        assert!(order.bar >= 1, "the first decision cannot fill on bar 0");
        if i > 0 {
            assert!(order.bar > sim.orders[i - 1].bar, "orders are chronological");
        }
    }
    assert_eq!(sim.equity.len(), 60, "one equity mark per bar");
    assert_eq!(sim.snapshots.len(), 60);
    assert!(sim.report.total_trades >= 1.0);
    assert!(sim.report.net_profit.is_finite());
    // The alternating crossover strategy trades both ways in a cycling fixture.
    assert!(sim.orders.iter().any(|o| o.pnl.is_some()), "closed trades carry pnl");
}

#[test]
fn an_indicator_script_simulates_nothing() {
    let src = "//@pine_lite version=1 overlay=false title=\"rsi\"\nplot(ta.rsi(close, 14))\n";
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(30), &Inputs::default()).expect("run");
    assert!(output.simulation.is_none(), "no intents, no simulation");
}

#[test]
fn strategy_entry_orders_can_carry_explicit_qty() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"qty\" strategy(commission_value=0)\n",
        "if bar_index == 3\n",
        "    strategy.entry(\"long\", direction=\"long\", qty=5.0)\n",
        "if bar_index == 8\n",
        "    strategy.close_all()\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(15), &Inputs::default()).expect("run");
    let sim = output.simulation.expect("sim");
    let entry = sim.orders.iter().find(|o| o.bar == 4).expect("entry fills on bar 4");
    assert_eq!(entry.qty, 5.0, "explicit qty wins over the header knobs");
}

#[test]
fn reading_strategy_state_runs_two_passes_and_sees_the_account() {
    // A breakeven read: pass 1 records the entry; pass 2 re-runs with the
    // simulated account, so `strategy.position_size` is real on and after
    // the fill bar.
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"state\" ",
        "strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=2, commission_value=0)\n",
        "if bar_index == 3\n",
        "    strategy.entry(\"long\", direction=\"long\")\n",
        "ps = strategy.position_size\n",
        "plot(ps)\n",
    );
    let (_, parsed) = vet(src).expect("the state read vets with the header block");
    let output = run(&parsed, &candles(10), &Inputs::default()).expect("run");
    let ps = &output.plots[0].values;
    assert_eq!(ps[3], 0.0, "pass-2 state is one decision late: fills on bar 4");
    assert_eq!(ps[4], 2.0, "position_size reads the simulated account");
    assert_eq!(ps[9], 2.0);
    // The simulation is pass 1's; intents must be unchanged by the rerun.
    let sim = output.simulation.expect("sim");
    assert!(sim.orders.iter().any(|o| o.bar == 4 && o.qty == 2.0));
}

#[test]
fn a_strategy_state_read_without_the_header_block_is_refused() {
    // The silent-0.0 trap: an indicator reading strategy.equity used to get
    // a number that looked real and meant nothing. Now: vet refusal.
    let src = "//@pine_lite version=1 overlay=false title=\"t\"\nplot(strategy.equity)\n";
    let errs = vet(src).expect_err("state read without the block");
    assert!(
        errs.iter().any(|e| e.message.contains("needs a strategy(...) header block")),
        "{errs:?}"
    );
}

#[test]
fn pyramiding_above_zero_is_refused_with_the_reason() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\" ",
        "strategy(pyramiding=2)\n",
        "plot(close)\n",
    );
    let errs = vet(src).expect_err("pyramiding");
    assert!(errs.iter().any(|e| e.message.contains("pyramiding > 0 is not supported")), "{errs:?}");
}

#[test]
fn unknown_strategy_knobs_are_refused_with_the_known_list() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"t\" ",
        "strategy(martingale=true)\n",
        "plot(close)\n",
    );
    let errs = vet(src).expect_err("unknown knob");
    assert!(
        errs.iter().any(|e| e.message.contains("unknown strategy(...) knob `martingale`")),
        "{errs:?}"
    );
}

#[test]
fn qty_type_fixed_sizes_in_units_not_percent() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"fixed\" ",
        "strategy(default_qty_type=\"fixed\", default_qty_value=3, commission_value=0)\n",
        "if bar_index == 2\n",
        "    strategy.entry(\"long\", direction=\"long\")\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    let output = run(&parsed, &candles(8), &Inputs::default()).expect("run");
    let sim = output.simulation.expect("sim");
    let entry = sim.orders.first().expect("one order");
    assert_eq!(entry.qty, 3.0, "fixed sizing: units of the asset");
}

#[test]
fn commission_and_slippage_arrive_from_the_header() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"fees\" ",
        "strategy(default_qty_type=\"fixed\", default_qty_value=10, commission_value=1, slippage=1)\n",
        "if bar_index == 0\n",
        "    strategy.entry(\"long\", direction=\"long\")\n",
        "if bar_index == 3\n",
        "    strategy.close_all()\n",
        "plot(close)\n",
    );
    let (_, parsed) = vet(src).expect("vet");
    // Flat at 100 so the slippage math reads cleanly.
    let fixture: Vec<Candle> = (0..8).map(|i| {
        Candle {
            symbol: "T".into(),
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
    }).collect();
    let output = run(&parsed, &fixture, &Inputs::default()).expect("run");
    let sim = output.simulation.expect("sim");
    // Entry fills bar 1's open at 101 (buy + 1% slippage), exit fills bar
    // 4's open at 99 (sell − 1%). Round-trip commission 1% of (10*101) +
    // 1% of (10*99) = 20. Gross (99−101)*10 = −20.
    assert_eq!(sim.orders[0].price, 101.0);
    let pnl = sim.orders[1].pnl.expect("pnl");
    assert!((pnl + 40.0).abs() < 1e-9, "gross -20, fees 20: {pnl}");
    assert!((sim.report.net_profit + 40.0).abs() < 1e-9);
}
