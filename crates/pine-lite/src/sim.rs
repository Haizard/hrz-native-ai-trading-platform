//! The strategy simulator (docs/24 S1): one fill model, two consumers.
//!
//! The VM runs a strategy script bar by bar and records `strategy.*`
//! *intents*; this module turns those intents into trades, an equity curve
//! and a report. The backtester's `ScriptStrategy` adapter lowers the same
//! intents into replay signals, so its fill rules and this module's must
//! agree by construction — the shared helpers below
//! ([`default_stop`], [`fill_price`], [`commission`]) are that by-construction
//! agreement, and the S4 parity test pins the rest.
//!
//! Fill semantics (Pine's, and the replay's): an order decided on a closed
//! bar fills at the NEXT bar's open; slippage moves the fill against the
//! trader; a stop fills at its level unless the bar's open gapped through it.
//! One position at a time — pyramiding is refused upstream, and an entry
//! intent while a position is open is skipped, exactly like the replay
//! adapter refuses the signal. An exit/close intent on the same bar as an
//! entry wins (the replay's one-decision-per-bar rule).

use crate::interp::Intent;
use analytics_core::types::Candle;

/// Commission model, from the `strategy(...)` header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommissionType {
    /// Percent of notional per side.
    Percent,
    /// Fixed currency per side.
    Absolute,
}

/// Position sizing, from the `strategy(...)` header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QtyType {
    /// `default_qty_value` is a percent of equity, valued at the decision
    /// bar's close mark.
    PercentOfEquity,
    /// `default_qty_value` is units of the asset.
    Fixed,
}

/// The `strategy(...)` header's knobs, with Pine-compatible defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyHeader {
    pub initial_capital: f64,
    pub default_qty_type: QtyType,
    pub default_qty_value: f64,
    pub commission_type: CommissionType,
    pub commission_value: f64,
    /// Slippage in percent, applied against the trader on every fill.
    pub slippage_pct: f64,
}

impl Default for StrategyHeader {
    fn default() -> Self {
        Self {
            initial_capital: 10_000.0,
            default_qty_type: QtyType::PercentOfEquity,
            default_qty_value: 10.0,
            commission_type: CommissionType::Percent,
            commission_value: 0.04,
            slippage_pct: 0.0,
        }
    }
}

/// One simulated fill, chronological. `bar` is the DECISION bar; the fill
/// happened at the next bar's open (or intrabar, for stops). `pnl` is set on
/// the order that closed a trade, net of both sides' commission.
#[derive(Debug, Clone, PartialEq)]
pub struct SimOrder {
    pub bar: usize,
    pub kind: OrderKind,
    pub long: bool,
    pub qty: f64,
    pub price: f64,
    pub stop: Option<f64>,
    pub pnl: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderKind {
    Entry,
    Exit,
    Stop,
    Close,
}

/// The account as of one bar's close — what the strategy state builtins read
/// in the interpreter's second pass.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SimSnapshot {
    pub position_size: f64,
    pub avg_price: f64,
    pub equity: f64,
    pub openprofit: f64,
    pub closedtrades: f64,
    pub wintrades: f64,
}

/// The report: the same numbers as the DSL backtest report's headline.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SimStats {
    pub net_profit: f64,
    pub total_trades: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
}

/// Everything one run's simulation produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Simulation {
    pub orders: Vec<SimOrder>,
    /// Per-bar equity, marked at each bar's close; len == candles len.
    pub equity: Vec<f64>,
    /// Per-bar account state for the strategy state builtins.
    pub snapshots: Vec<SimSnapshot>,
    pub report: SimStats,
}

struct Position {
    long: bool,
    qty: f64,
    entry_price: f64,
    entry_bar: usize,
    stop: Option<f64>,
}

/// Pine's stop-fill rule, shared with the replay adapter: a long's stop
/// fills at the stop level, or at the open when the bar GAPPED through it
/// (the worse price).
#[must_use]
pub fn stop_fill_price(long: bool, open: f64, stop: f64) -> f64 {
    if long {
        open.min(stop)
    } else {
        open.max(stop)
    }
}

/// A stop a script never declared: 2 × ATR(14) from the decision close —
/// the replay adapter's default, shared so the two paths cannot drift.
/// Falls back to 2% of price while ATR is still warming up.
#[must_use]
pub fn default_stop(candles: &[Candle], bar: usize, long: bool) -> Option<f64> {
    let candle = candles.get(bar)?;
    let atr14 = crate::ta::ta_atr(candles, 14.0);
    let atr = atr14.get(bar).copied().unwrap_or(f64::NAN);
    let distance = if atr.is_finite() { 2.0 * atr } else { candle.close * 0.02 };
    Some(if long { candle.close - distance } else { candle.close + distance })
}

/// Fill price for a market order at `open`: slippage moves against the
/// trader. Shared with the replay adapter.
#[must_use]
pub fn fill_price(open: f64, long: bool, slippage_pct: f64) -> f64 {
    if long {
        open * (1.0 + slippage_pct / 100.0)
    } else {
        open * (1.0 - slippage_pct / 100.0)
    }
}

/// One side's commission on `notional`, in currency. Shared with the replay
/// adapter.
#[must_use]
pub fn commission(knobs: &StrategyHeader, notional: f64) -> f64 {
    match knobs.commission_type {
        CommissionType::Percent => notional * knobs.commission_value / 100.0,
        CommissionType::Absolute => knobs.commission_value,
    }
}

fn open_pnl(pos: &Position, price: f64) -> f64 {
    let dir = if pos.long { 1.0 } else { -1.0 };
    (price - pos.entry_price) * pos.qty * dir
}

/// Simulate one run's intents over the candles. Deterministic, order-only —
/// no user code runs here, so no fuel is metered.
#[must_use]
pub fn simulate(intents: &[(usize, Intent)], candles: &[Candle], knobs: &StrategyHeader) -> Simulation {
    let mut sim = Simulation::default();
    let mut position: Option<Position> = None;
    let mut realized = 0.0;
    let mut closed = 0.0f64;
    let mut wins = 0.0f64;
    let mut gross_win = 0.0f64;
    let mut gross_loss = 0.0f64;
    let mut peak = knobs.initial_capital;
    let mut max_dd = 0.0f64;
    sim.equity = Vec::with_capacity(candles.len());
    sim.snapshots = Vec::with_capacity(candles.len());
    // The fill an intent decided on bar b queues for bar b+1: one pending
    // market order at a time (the one-decision-per-bar rule).
    let mut pending: Option<Intent> = None;
    let mut pending_stop: Option<f64> = None;

    for bar in 0..candles.len() {
        let c = &candles[bar];
        // 1. Fills queued by the previous bar's intents, at THIS open.
        if let Some(intent) = pending.take() {
            match intent {
                Intent::Entry { long, qty, .. } if position.is_none() => {
                    let price = fill_price(c.open, long, knobs.slippage_pct);
                    let equity_mark = knobs.initial_capital + realized;
                    let qty = qty.unwrap_or_else(|| match knobs.default_qty_type {
                        QtyType::Fixed => knobs.default_qty_value,
                        QtyType::PercentOfEquity => equity_mark * knobs.default_qty_value / 100.0 / price,
                    });
                    if qty > 0.0 && price > 0.0 {
                        position = Some(Position {
                            long,
                            qty,
                            entry_price: price,
                            entry_bar: bar,
                            // A stop declared BEFORE the entry (bracket style:
                            // `strategy.exit(stop=...)` the same bar) arms the
                            // position the moment it fills.
                            stop: pending_stop.take(),
                        });
                        sim.orders.push(SimOrder {
                            bar,
                            kind: OrderKind::Entry,
                            long,
                            qty,
                            price,
                            stop: None,
                            pnl: None,
                        });
                    }
                }
                Intent::Exit { .. } | Intent::Close { .. } => {
                    if let Some(pos) = position.take() {
                        let exit_ref = pending_stop.take().unwrap_or(c.open);
                        // The CLOSING trade's side: exiting a long is a sell
                        // (fill slips down), exiting a short is a buy.
                        let price = fill_price(exit_ref, !pos.long, knobs.slippage_pct);
                        let gross = open_pnl(&pos, price);
                        let fees = commission(knobs, pos.qty * pos.entry_price)
                            + commission(knobs, pos.qty * price);
                        let pnl = gross - fees;
                        realized += pnl;
                        closed += 1.0;
                        if pnl > 0.0 {
                            wins += 1.0;
                            gross_win += pnl;
                        } else {
                            gross_loss -= pnl;
                        }
                        sim.orders.push(SimOrder {
                            bar,
                            kind: OrderKind::Exit,
                            long: pos.long,
                            qty: pos.qty,
                            price,
                            stop: pos.stop,
                            pnl: Some(pnl),
                        });
                    }
                }
                _ => {}
            }
        }

        // 2. Intrabar stop on the active position (a stop updated by this
        //    bar's queued intent activates NEXT bar, like any order).
        if let Some(pos) = &position {
            if let Some(stop) = pos.stop {
                let hit = if pos.long { c.low <= stop } else { c.high >= stop };
                if hit {
                    let pos = position.take().expect("checked above");
                    let price = stop_fill_price(pos.long, c.open, stop);
                    let gross = open_pnl(&pos, price);
                    let fees = commission(knobs, pos.qty * pos.entry_price)
                        + commission(knobs, pos.qty * price);
                    let pnl = gross - fees;
                    realized += pnl;
                    closed += 1.0;
                    if pnl > 0.0 {
                        wins += 1.0;
                        gross_win += pnl;
                    } else {
                        gross_loss -= pnl;
                    }
                    sim.orders.push(SimOrder {
                        bar,
                        kind: OrderKind::Stop,
                        long: pos.long,
                        qty: pos.qty,
                        price,
                        stop: pos.stop,
                        pnl: Some(pnl),
                    });
                }
            }
        }

        // 3. Mark the account at this close.
        let open_pnl_mark = position.as_ref().map_or(0.0, |p| open_pnl(p, c.close));
        let equity = knobs.initial_capital + realized + open_pnl_mark;
        peak = peak.max(equity);
        if peak > 0.0 {
            max_dd = max_dd.max((peak - equity) / peak);
        }
        let (pos_size, avg, op) = position.as_ref().map_or((0.0, 0.0, open_pnl_mark), |p| {
            (if p.long { p.qty } else { -p.qty }, p.entry_price, open_pnl_mark)
        });
        sim.equity.push(equity);
        sim.snapshots.push(SimSnapshot {
            position_size: pos_size,
            avg_price: avg,
            equity,
            openprofit: op,
            closedtrades: closed,
            wintrades: wins,
        });

        // 4. This bar's intents decide for the NEXT open. `strategy.exit`
        // with a stop ARMS the standing stop (Pine semantics: the position
        // closes only when price touches it) and carries for the next entry;
        // a stop-less exit and every close/close_all are market exits.
        let bar_intents: Vec<&Intent> = intents.iter().filter(|(b, _)| *b == bar).map(|(_, i)| i).collect();
        let close_wins = bar_intents.iter().any(|i| {
            matches!(i, Intent::Close { .. })
                || matches!(i, Intent::Exit { stop: None, .. })
        });
        for intent in &bar_intents {
            match intent {
                Intent::Exit { stop, .. } => {
                    if let Some(s) = stop {
                        if let Some(pos) = &mut position {
                            pos.stop = Some(*s);
                        }
                        pending_stop = Some(*s);
                    } else {
                        pending = Some(Intent::Close { id: None });
                    }
                }
                Intent::Close { .. } => pending = Some(Intent::Close { id: None }),
                Intent::Entry { long, qty, .. } if !close_wins => {
                    // Pine/TV: an entry while a position is open is refused
                    // (pyramiding 0); the replay adapter refuses the signal,
                    // the sim skips the order. Same rule, same bar.
                    if position.is_none() {
                        pending = Some(Intent::Entry { id: String::new(), long: *long, qty: *qty });
                    }
                }
                Intent::Entry { .. } => {}
            }
        }
    }

    let final_equity = sim.equity.last().copied().unwrap_or(knobs.initial_capital);
    sim.report = SimStats {
        net_profit: final_equity - knobs.initial_capital,
        total_trades: closed,
        win_rate: if closed > 0.0 { wins / closed } else { 0.0 },
        profit_factor: if gross_loss > 0.0 {
            gross_win / gross_loss
        } else if gross_win > 0.0 {
            f64::MAX
        } else {
            0.0
        },
        max_drawdown: max_dd,
    };
    sim
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::Timeframe;

    fn candle(i: usize, close: f64) -> Candle {
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
    }

    fn entry(bar: usize, long: bool) -> (usize, Intent) {
        (bar, Intent::Entry { id: "e".into(), long, qty: None })
    }

    fn close(bar: usize) -> (usize, Intent) {
        (bar, Intent::Close { id: None })
    }

    #[test]
    fn an_entry_fills_at_the_next_open_and_the_close_prices_the_trade() {
        // Closes 100..110 rising; entry decided on bar 2 fills at open[3]
        // (100-based fixture: open == close of each bar).
        let candles: Vec<Candle> = (0..10).map(|i| candle(i, 100.0 + i as f64)).collect();
        let knobs = StrategyHeader { commission_value: 0.0, slippage_pct: 0.0, ..Default::default() };
        let sim = simulate(&[entry(2, true), close(5)], &candles, &knobs);
        assert_eq!(sim.orders.len(), 2);
        assert_eq!(sim.orders[0].kind, OrderKind::Entry);
        assert_eq!(sim.orders[0].bar, 3, "fill bar is the next open");
        assert_eq!(sim.orders[0].price, 103.0);
        assert_eq!(sim.orders[1].price, 106.0, "close decided on 5 fills at open[6]");
        // Fixed sizing would need a knob; default is 10% of equity.
        let qty = sim.orders[0].qty;
        let expected = 10_000.0 * 10.0 / 100.0 / 103.0;
        assert!((qty - expected).abs() < 1e-9, "{qty} vs {expected}");
        // Gross: (106 - 103) * qty; no commission here.
        let pnl = sim.orders[1].pnl.expect("closing order carries pnl");
        assert!((pnl - 3.0 * qty).abs() < 1e-9, "{pnl}");
        assert_eq!(sim.report.total_trades, 1.0);
        assert_eq!(sim.report.win_rate, 1.0);
        assert!(sim.report.net_profit > 0.0);
    }

    #[test]
    fn a_stop_fills_intrabar_at_its_level_or_the_gap_open() {
        // Position long from open 100 with stop 95; bar 3 dips to 94.
        let mut candles: Vec<Candle> = (0..6).map(|i| candle(i, 100.0)).collect();
        candles[3] = Candle { low: 94.0, ..candle(3, 100.0) };
        let knobs = StrategyHeader { commission_value: 0.0, ..Default::default() };
        let stop_intent = (1, Intent::Exit { id: "x".into(), stop: Some(95.0), limit: None });
        let sim = simulate(&[entry(0, true), stop_intent], &candles, &knobs);
        let stop_order = sim.orders.iter().find(|o| o.kind == OrderKind::Stop).expect("stop filled");
        assert_eq!(stop_order.bar, 3, "the bar that dipped");
        assert_eq!(stop_order.price, 95.0, "fill at the stop, not the low");
        assert!(stop_order.pnl.expect("pnl") < 0.0, "stopped out at a loss");
    }

    #[test]
    fn commission_and_slippage_both_bite() {
        let candles: Vec<Candle> = (0..6).map(|i| candle(i, 100.0)).collect();
        let knobs = StrategyHeader {
            default_qty_type: QtyType::Fixed,
            default_qty_value: 10.0,
            commission_value: 1.0, // 1% notional per side
            slippage_pct: 0.5,
            ..Default::default()
        };
        let sim = simulate(&[entry(0, true), close(2)], &candles, &knobs);
        assert!((sim.orders[0].price - 100.5).abs() < 1e-9, "long pays the ask: {}", sim.orders[0].price);
        assert!((sim.orders[1].price - 99.5).abs() < 1e-9, "long sells into the bid: {}", sim.orders[1].price);
        let pnl = sim.orders[1].pnl.expect("pnl");
        // Gross (99.5 - 100.5) * 10 = -10; fees = 1% of (10*100.5) + 1% of
        // (10*99.5) = 10.05 + 9.95 = 20.0. Net -30.
        assert!((pnl + 30.0).abs() < 1e-9, "{pnl}");
        assert!(sim.report.net_profit < 0.0);
    }

    #[test]
    fn an_entry_while_in_a_position_is_skipped_and_close_wins_the_same_bar() {
        let candles: Vec<Candle> = (0..8).map(|i| candle(i, 100.0 + i as f64)).collect();
        let knobs = StrategyHeader { commission_value: 0.0, ..Default::default() };
        // Entry decided 0; second entry decided 3 (must skip); close+entry
        // decided 5 on the same bar: the close wins, the entry is dropped.
        let sim = simulate(
            &[entry(0, true), entry(3, true), close(5), entry(5, false)],
            &candles,
            &knobs,
        );
        let entries: Vec<&SimOrder> = sim.orders.iter().filter(|o| o.kind == OrderKind::Entry).collect();
        assert_eq!(entries.len(), 1, "only the first entry fills: {sim:?}");
    }

    #[test]
    fn the_equity_curve_marks_to_market_and_drawdown_tracks_the_peak() {
        // Entry decided on bar 0 fills at open[1] = 100.5 (fixture: open ==
        // close). Price runs up to 101.5, then pulls back to 101.
        let mut candles: Vec<Candle> = (0..6).map(|i| candle(i, 100.0 + i as f64 * 0.5)).collect();
        candles[4].close = 101.0;
        candles[5].close = 101.0;
        let knobs = StrategyHeader {
            default_qty_type: QtyType::Fixed,
            default_qty_value: 1.0,
            commission_value: 0.0,
            ..Default::default()
        };
        let sim = simulate(&[entry(0, true)], &candles, &knobs);
        assert_eq!(sim.orders[0].price, 100.5, "fill at the next open");
        assert_eq!(sim.equity[3], 10_000.0 + 1.0, "mark: close 101.5 vs entry 100.5");
        assert_eq!(sim.equity[5], 10_000.0 + 0.5, "mark: close 101 vs entry 100.5");
        let peak = 10_001.0;
        assert!((sim.report.max_drawdown - 0.5 / peak).abs() < 1e-9, "{}", sim.report.max_drawdown);
        // Snapshots feed the builtins.
        let s3 = &sim.snapshots[3];
        assert_eq!(s3.position_size, 1.0);
        assert_eq!(s3.avg_price, 100.5);
        assert_eq!(s3.openprofit, 1.0);
        assert_eq!(s3.closedtrades, 0.0);
    }

    #[test]
    fn an_indicator_run_simulates_nothing() {
        let candles: Vec<Candle> = (0..4).map(|i| candle(i, 100.0)).collect();
        let sim = simulate(&[], &candles, &StrategyHeader::default());
        assert!(sim.orders.is_empty());
        assert_eq!(sim.report.net_profit, 0.0);
        assert_eq!(sim.equity.len(), 4, "the curve exists even without trades");
        assert!(sim.equity.iter().all(|e| *e == 10_000.0));
    }

    #[test]
    fn the_shared_default_stop_matches_the_replay_adapters_rule() {
        let candles: Vec<Candle> = (0..30).map(|i| candle(i, 100.0 + (i % 9) as f64)).collect();
        let long = default_stop(&candles, 20, true).expect("stop");
        let short = default_stop(&candles, 20, false).expect("stop");
        assert!(long < candles[20].close && short > candles[20].close);
    }
}
