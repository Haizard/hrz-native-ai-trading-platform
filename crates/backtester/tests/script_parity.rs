//! docs/24 S4: the VM's in-run simulation and the backtester's
//! `ScriptStrategy` replay must produce the same trade sequence for the
//! same script over the same candles.
//!
//! The two paths share one interpreter (the same `pine_lite::run` and the
//! same intent lowering), so what can drift is the FILL model: the VM fills
//! next-open with a standing intrabar stop; the replay decides on a closed
//! bar, fills at the next open, and carries the script's stop on the entry
//! signal. These tests pin the shared surface with fixed scripts: explicit
//! carried stops (never trailing -- that is the replay's v2 axis), no
//! account-state reads, and one position at a time. Position SIZE
//! deliberately differs between the paths (the replay sizes from risk, the
//! VM from the header's equity percent), so parity means the trade
//! SEQUENCE: direction, entry bar, exit bar.

use std::collections::BTreeMap;

use analytics_core::types::{Candle, Timeframe};
use backtester::replay::{replay, ReplayConfig, ReplayInput};
use backtester::script_strategy::{script_replay_input, ScriptStrategy};
use pine_lite::{run as run_script, Inputs};
use strategy_dsl::Direction;

/// A base wave -- flat warmup, an up-trend, a sharp reversal, a recovery --
/// so every cross signal fires well inside the replay's own warmup window.
fn wave() -> Vec<Candle> {
    (0..140)
        .map(|i| {
            let close = match i {
                0..=19 => 100.0 + (i as f64) * 0.05,
                20..=59 => 101.0 + ((i - 20) as f64) * 0.72,
                60..=89 => 130.0 - ((i - 59) as f64) * 0.85,
                _ => 104.0 + ((i - 89) as f64) * 0.5,
            };
            Candle {
                symbol: "TEST".into(),
                timeframe: Timeframe::M1,
                open_time: 1_790_000_000_000_000_000i64 + (i as i64) * 60_000_000_000,
                open: close - 0.15,
                high: close + 1.0,
                low: close - 1.0,
                close,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            }
        })
        .collect()
}

/// One closed trade, normalized between the two fill models: direction and
/// the bars the entry and exit filled on.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Trade {
    long: bool,
    entry_bar: usize,
    exit_bar: usize,
}

/// The VM's own trade list: walk the simulator's orders, pairing each entry
/// with the exit or stop that closes it.
fn vm_trades(source: &str, cs: &[Candle]) -> Vec<Trade> {
    let (_, parsed) = pine_lite::vet(source)
        .ok()
        .unwrap_or_else(|| panic!("parity script must vet:\n{source}"));
    let output = run_script(&parsed, cs, &Inputs::default()).expect("the parity script runs");
    let sim = output.simulation.as_ref().expect("a strategy run simulates");
    let mut trades = Vec::new();
    let mut open: Option<(bool, usize)> = None;
    for order in &sim.orders {
        match order.kind {
            pine_lite::sim::OrderKind::Entry => open = Some((order.long, order.bar)),
            pine_lite::sim::OrderKind::Exit
            | pine_lite::sim::OrderKind::Stop
            | pine_lite::sim::OrderKind::Close => {
                if let Some((long, entry_bar)) = open.take() {
                    trades.push(Trade { long, entry_bar, exit_bar: order.bar });
                }
            }
        }
    }
    trades
}

/// The replay's trade list: the same script through `ScriptStrategy` and the
/// production replay loop, with fill bars recovered from the fill times.
fn replay_trades(source: &str, cs: &[Candle]) -> Vec<Trade> {
    let mut strategy = ScriptStrategy::new(source).expect("the parity script becomes a strategy");
    let input = ReplayInput {
        timeframes: BTreeMap::from([("entry".to_string(), Timeframe::M1)]),
        candles: script_replay_input(cs, Timeframe::M1),
    };
    let config = ReplayConfig {
        symbol: "TEST".into(),
        from: cs[0].open_time,
        to: cs[cs.len() - 1].open_time,
        simulator: strategy_runtime::SimulatorConfig {
            slippage_bps: 2.0,
            ..strategy_runtime::SimulatorConfig::default()
        },
        ..ReplayConfig::default()
    };
    let output = replay(&mut strategy, &input, &config).expect("the replay runs");
    // The replay liquidates a still-open position at the end of data
    // (`ExitTrigger::EndOfData`); the VM leaves it open and just marks the
    // equity. Parity is on CLOSED trades -- the same surface the VM's
    // `total_trades` counts -- so the forced liquidation drops out.
    let bar_of_time: BTreeMap<i64, usize> = cs
        .iter()
        .enumerate()
        .map(|(i, c)| (c.open_time, i))
        .collect();
    output
        .trades
        .iter()
        .filter(|t| t.exit_trigger != strategy_runtime::ExitTrigger::EndOfData)
        .map(|t| Trade {
            long: t.direction == Direction::Long,
            entry_bar: bar_of_time[&t.entry_time],
            exit_bar: bar_of_time[&t.exit_time],
        })
        .collect()
}

fn assert_parity(source: &str) {
    let cs = wave();
    let vm = vm_trades(source, &cs);
    let replayed = replay_trades(source, &cs);
    assert_eq!(
        vm, replayed,
        "VM sim and ScriptStrategy replay disagree on:\n{source}"
    );
}

#[test]
fn a_long_crossover_strategy_replays_identically() {
    assert_parity(concat!(
        "//@pine_lite version=1 overlay=false title=\"parity long\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1, slippage=0.02)\n",
        "fast = ta.sma(close, 10)\n",
        "slow = ta.sma(close, 30)\n",
        "if ta.crossover(fast, slow)\n",
        "    strategy.entry(\"L\", direction=\"long\")\n",
        "if ta.crossunder(fast, slow)\n",
        "    strategy.close_all()\n",
        "strategy.exit(\"xl\", stop=90.0)\n",
    ));
}

#[test]
fn a_short_crossunder_strategy_replays_identically() {
    assert_parity(concat!(
        "//@pine_lite version=1 overlay=false title=\"parity short\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1, slippage=0.02)\n",
        "fast = ta.sma(close, 10)\n",
        "slow = ta.sma(close, 30)\n",
        "if ta.crossunder(fast, slow)\n",
        "    strategy.entry(\"S\", direction=\"short\")\n",
        "if ta.crossover(fast, slow)\n",
        "    strategy.close_all()\n",
        "strategy.exit(\"xs\", stop=150.0)\n",
    ));
}

#[test]
fn a_strategy_closing_on_its_own_signal_replays_identically() {
    // No reverse cross here: the script exits on a plain bar rule, and the
    // carried wide stop never fires on this fixture -- both paths must keep
    // the trade open exactly as long.
    assert_parity(concat!(
        "//@pine_lite version=1 overlay=false title=\"parity close rule\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1, slippage=0.02)\n",
        "if bar_index == 5\n",
        "    strategy.entry(\"L\", direction=\"long\")\n",
        "if bar_index == 40\n",
        "    strategy.close_all()\n",
        "strategy.exit(\"xl\", stop=50.0)\n",
    ));
}

#[test]
fn a_strategy_that_reenters_replays_identically() {
    // Two cycles through the wave: up (long), reversal close, recovery long.
    assert_parity(concat!(
        "//@pine_lite version=1 overlay=false title=\"parity reentry\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1, slippage=0.02)\n",
        "if bar_index == 5 or bar_index == 95\n",
        "    strategy.entry(\"L\", direction=\"long\")\n",
        "if bar_index == 65\n",
        "    strategy.close_all()\n",
        "strategy.exit(\"xl\", stop=80.0)\n",
    ));
}

#[test]
fn a_strategy_that_never_trades_is_empty_on_both_paths() {
    assert_parity(concat!(
        "//@pine_lite version=1 overlay=false title=\"parity never\" strategy(initial_capital=10000, default_qty_type=\"fixed\", default_qty_value=1, slippage=0.02)\n",
        "if close > 1000000.0\n",
        "    strategy.entry(\"L\", direction=\"long\")\n",
        "strategy.exit(\"xl\", stop=50.0)\n",
    ));
}
