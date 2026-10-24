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

use crate::interp::{Intent, RiskLimitKind};
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
    /// Extra same-direction entries allowed on one position (Pine's
    /// pyramiding): 0 = a single leg, 2 = up to three legs.
    pub pyramiding: usize,
    /// Monetary value of one price point per unit (syminfo.pointvalue):
    /// 1.0 for spot, the contract multiplier for futures. Instrument
    /// metadata the host stamps on the knobs before the run, not a
    /// script-declared knob.
    pub pointvalue: f64,
    /// Contract-flavored execution (Deriv multipliers): entry qty is read
    /// as STAKE in account currency, PnL scales by `multiplier`, and the
    /// loss caps at the stake via the venue's stop-out. Host-stamped from
    /// the instrument's product, not script-declared.
    pub contracts: bool,
    /// The multiplier's leverage for contract mode (`strategy(multiplier=50)`).
    pub multiplier: f64,
    /// The account guards: halt NEW entries when the day/week/drawdown/
    /// streak/position limits trip. Managing an open position never halts.
    pub guards: Option<analytics_core::risk::RiskGuards>,
    /// The position-management rules: break-even and the ATR trail, applied
    /// per bar on the open trade with its own entry context.
    pub management: Option<analytics_core::risk::ManagementRules>,
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
            pyramiding: 0,
            pointvalue: 1.0,
            contracts: false,
            multiplier: 50.0,
            guards: None,
            management: None,
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
    Limit,
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
    /// Cumulative closed PnL (Pine's strategy.netprofit: realized only,
    /// the open leg does not count until it closes).
    pub netprofit: f64,
    /// Cumulative winning-leg PnL (strategy.grossprofit).
    pub grossprofit: f64,
    /// Cumulative losing-leg PnL, NEGATIVE (strategy.grossloss), so the
    /// profit-factor idiom reads `grossprofit / math.abs(grossloss)`.
    pub grossloss: f64,
    /// Running peak-to-trough equity drawdown in currency
    /// (strategy.max_drawdown), not a fraction.
    pub max_drawdown: f64,
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
    /// Same-direction legs filled so far (1 = the initial entry).
    legs: usize,
    entry_price: f64,
    entry_bar: usize,
    stop: Option<f64>,
    limit: Option<f64>,
    /// Fraction the armed exit closes when it triggers (Pine's
    /// qty_percent); None = the whole position.
    exit_pct: Option<f64>,
    /// Stacked partial exits: pending (limit, percent-of-original) pairs.
    /// A ladder the document armed at entry -- half at +1R, the rest at
    /// +2R -- filled independently of the bracket, which stays the
    /// protection underneath.
    partials: Vec<(f64, f64)>,
    /// The entry's own quantity, for partial-exit percentages (Pine's
    /// qty_percent reads percent of the ENTRY, not of the remainder).
    original_qty: f64,
    /// Trailing-stop activation distance from the entry (price units).
    trail_points: Option<f64>,
    /// Trailing-stop activation as an absolute price.
    trail_price: Option<f64>,
    /// Distance the ratcheting stop trails the peak (price units).
    trail_offset: Option<f64>,
    /// The best price since the trail armed (the peak for longs, the
    /// trough for shorts).
    trail_peak: f64,
}

/// Book one exit leg -- possibly a fraction of the position -- and shrink
/// the remainder. Returns (closed_qty, pnl). Fees are pro-rata per leg, so
/// a three-leg scale-out pays the same total commission as one full exit.
fn close_leg(
    pos: &mut Position,
    frac: f64,
    price: f64,
    knobs: &StrategyHeader,
) -> (f64, f64) {
    let closed_qty = if frac >= 1.0 { pos.qty } else { pos.qty * frac.max(0.0) };
    if knobs.contracts {
        // Contract mode: qty IS the stake; the move scales by the
        // multiplier, the loss caps at the stake. The round-trip commission
        // lands at close like the spot path, but on the STAKE -- Deriv's
        // notional is the stake, not price x quantity.
        let gross = analytics_core::multiplier::multiplier_pnl(
            pos.entry_price,
            price,
            closed_qty,
            knobs.multiplier,
            pos.long,
        );
        let fees = commission(knobs, closed_qty);
        pos.qty -= closed_qty;
        return (closed_qty, gross - fees);
    }
    let gross = if pos.long {
        (price - pos.entry_price) * closed_qty * knobs.pointvalue
    } else {
        (pos.entry_price - price) * closed_qty * knobs.pointvalue
    };
    let fees = commission(knobs, closed_qty * pos.entry_price) + commission(knobs, closed_qty * price);
    pos.qty -= closed_qty;
    (closed_qty, gross - fees)
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
/// The mirror of `stop_fill_price` for targets: a long's limit fills at the
/// limit, or at the open when the bar GAPPED past it (the better price).
#[must_use]
pub fn limit_fill_price(long: bool, open: f64, limit: f64) -> f64 {
    if long { open.max(limit) } else { open.min(limit) }
}

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

fn open_pnl(pos: &Position, price: f64, knobs: &StrategyHeader) -> f64 {
    if knobs.contracts {
        return analytics_core::multiplier::multiplier_pnl(
            pos.entry_price,
            price,
            pos.qty,
            knobs.multiplier,
            pos.long,
        );
    }
    let dir = if pos.long { 1.0 } else { -1.0 };
    (price - pos.entry_price) * pos.qty * dir * knobs.pointvalue
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
    let mut max_dd_abs = 0.0f64;
    sim.equity = Vec::with_capacity(candles.len());
    sim.snapshots = Vec::with_capacity(candles.len());
    // The fill an intent decided on bar b queues for bar b+1: one pending
    // market order at a time (the one-decision-per-bar rule).
    let mut pending: Option<Intent> = None;
    let mut pending_stop: Option<f64> = None;
    let mut pending_limit: Option<f64> = None;
    let mut pending_pct: Option<f64> = None;
    // Stacked partial rungs queued before the entry fills: they attach to
    // the position the moment it opens (the partial ladder arms at entry).
    let mut pending_partials: Vec<(f64, f64)> = Vec::new();
    let mut pending_trail: (Option<f64>, Option<f64>, Option<f64>) = (None, None, None);
    // strategy.risk.* limits, settable per intent from the next bar on.
    let mut risk_max_qty: Option<f64> = None;
    let mut risk_max_day_loss: Option<f64> = None;
    let mut risk_max_dd: Option<f64> = None;
    let mut risk_allow: (bool, bool) = (true, true);
    let mut risk_dd_halted = false;
    let mut risk_day_floor = 0.0f64;
    let mut risk_day_id: i64 = 0;
    let mut risk_day_halted = false;
    // The risk layer's own state: the guards read it, the management
    // spends it. Day/week starts roll on UTC boundaries.
    let mut risk_state = analytics_core::risk::RiskState {
        equity: knobs.initial_capital,
        peak: knobs.initial_capital,
        day_start: knobs.initial_capital,
        week_start: knobs.initial_capital,
        consecutive_losses: 0,
        open_positions: 0,
    };
    const DAY_NS: i64 = 3_600_000_000_000 * 24;
    const WEEK_NS: i64 = DAY_NS * 7;
    let mut last_day_id: i64 = -1;
    let mut last_week_id: i64 = -1;

    for bar in 0..candles.len() {
        let c = &candles[bar];
        // The guard state rolls with the calendar before anything fills.
        if knobs.guards.is_some() {
            let day_id = c.open_time.div_euclid(DAY_NS);
            let week_id = c.open_time.div_euclid(WEEK_NS);
            if last_day_id < 0 {
                last_day_id = day_id;
                last_week_id = week_id;
            }
            if day_id != last_day_id {
                risk_state.day_start = risk_state.equity;
                last_day_id = day_id;
            }
            if week_id != last_week_id {
                risk_state.week_start = risk_state.equity;
                last_week_id = week_id;
            }
        }
        // 1. Fills queued by the previous bar's intents, at THIS open.
        if let Some(intent) = pending.take() {
            match intent {
                Intent::Entry { long, qty, .. }
                    if position.is_none()
                        || position.as_ref().is_some_and(|p| {
                            p.long == long && p.legs < 1 + knobs.pyramiding
                        }) =>
                {
                    // The risk layer's guards: NEW risk halts here; an open
                    // position is managed to its end regardless.
                    risk_state.open_positions = usize::from(position.is_some());
                    if let Some(guards) = &knobs.guards {
                        if !guards.allows(&risk_state) {
                            continue;
                        }
                    }
                    let price = fill_price(c.open, long, knobs.slippage_pct);
                    let equity_mark = knobs.initial_capital + realized;
                    let qty = qty.unwrap_or_else(|| match knobs.default_qty_type {
                        QtyType::Fixed => knobs.default_qty_value,
                        QtyType::PercentOfEquity => equity_mark * knobs.default_qty_value / 100.0 / price,
                    });
                    // strategy.risk.max_position_size caps the order.
                    let qty = risk_max_qty.map_or(qty, |cap| qty.min(cap));
                    if qty > 0.0 && price > 0.0 {
                        if let Some(mut pos) = position.take() {
                            // A pyramid leg stacks: the cost basis becomes
                            // the qty-weighted average of the legs, and a
                            // bracket armed with THIS leg replaces the old
                            // one (the fresh leg's exit plan wins).
                            let new_qty = pos.qty + qty;
                            pos.entry_price =
                                (pos.qty * pos.entry_price + qty * price) / new_qty;
                            pos.qty = new_qty;
                            pos.legs += 1;
                            if pending_stop.is_some() {
                                pos.stop = pending_stop.take();
                            }
                            if pending_limit.is_some() {
                                pos.limit = pending_limit.take();
                            }
                            if pending_pct.is_some() {
                                pos.exit_pct = pending_pct.take();
                            }
                            if pending_trail.0.is_some() || pending_trail.1.is_some() || pending_trail.2.is_some() {
                                pos.trail_points = pending_trail.0.take();
                                pos.trail_price = pending_trail.1.take();
                                pos.trail_offset = pending_trail.2.take();
                                pos.trail_peak = price;
                            }
                            position = Some(pos);
                        } else {
                            position = Some(Position {
                                long,
                                qty,
                                legs: 1,
                                entry_price: price,
                                entry_bar: bar,
                                // A stop declared BEFORE the entry (bracket style:
                                // `strategy.exit(stop=...)` the same bar) arms the
                                // position the moment it fills.
                                stop: pending_stop.take(),
                                limit: pending_limit.take(),
                                exit_pct: pending_pct.take(),
                                partials: std::mem::take(&mut pending_partials),
                                original_qty: qty,
                                trail_points: pending_trail.0.take(),
                                trail_price: pending_trail.1.take(),
                                trail_offset: pending_trail.2.take(),
                                trail_peak: price,
                            });
                        }
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
                Intent::Exit { qty_percent, .. } | Intent::Close { qty_percent, .. } => {
                    if let Some(mut pos) = position.take() {
                        let exit_ref = pending_stop.take().unwrap_or(c.open);
                        // The CLOSING trade's side: exiting a long is a sell
                        // (fill slips down), exiting a short is a buy.
                        let price = fill_price(exit_ref, !pos.long, knobs.slippage_pct);
                        let frac = qty_percent.map(|p| (p / 100.0).clamp(0.0, 1.0)).unwrap_or(1.0);
                        let (closed_qty, pnl) = close_leg(&mut pos, frac, price, knobs);
                        realized += pnl;
                        closed += 1.0;
                        if pnl > 0.0 { risk_state.consecutive_losses = 0; } else { risk_state.consecutive_losses += 1; }
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
                            qty: closed_qty,
                            price,
                            stop: pos.stop,
                            pnl: Some(pnl),
                        });
                        if pos.qty > 1e-12 {
                            position = Some(pos);
                        }
                    }
                }
                _ => {}
            }
        }

        // 1b. The trailing stop ratchets: once the peak moves the
        //    activation distance in favor, the stop follows the peak by
        //    the offset and never gives ground back.
        if let Some(pos) = &mut position {
            if pos.trail_offset.is_some() {
                let offset = pos.trail_offset.unwrap_or(0.0);
                let activate_at = pos.trail_price.unwrap_or_else(|| {
                    let d = pos.trail_points.unwrap_or(0.0);
                    if pos.long { pos.entry_price + d } else { pos.entry_price - d }
                });
                if pos.long {
                    pos.trail_peak = pos.trail_peak.max(c.high);
                    if pos.trail_peak >= activate_at {
                        let t = pos.trail_peak - offset;
                        pos.stop = Some(pos.stop.map_or(t, |cur| cur.max(t)));
                    }
                } else {
                    pos.trail_peak = pos.trail_peak.min(c.low);
                    if pos.trail_peak <= activate_at {
                        let t = pos.trail_peak + offset;
                        pos.stop = Some(pos.stop.map_or(t, |cur| cur.min(t)));
                    }
                }
            }
        }

        // 1c. The contract stop-out: in contract mode an adverse move of
        //    1/multiplier of the entry consumes the whole stake, at the
        //    venue's own level (it fills there, before any user stop).
        if knobs.contracts {
            if let Some(mut pos) = position.take() {
                let stop_out = analytics_core::multiplier::stop_out_level(
                    pos.entry_price,
                    knobs.multiplier,
                    pos.long,
                );
                let hit = if pos.long { c.low <= stop_out } else { c.high >= stop_out };
                if hit {
                    let (closed_qty, pnl) = close_leg(&mut pos, 1.0, stop_out, knobs);
                    realized += pnl;
                    closed += 1.0;
                    if pnl > 0.0 { risk_state.consecutive_losses = 0; } else { risk_state.consecutive_losses += 1; }
                    gross_loss -= pnl;
                    sim.orders.push(SimOrder {
                        bar,
                        kind: OrderKind::Stop,
                        long: pos.long,
                        qty: closed_qty,
                        price: stop_out,
                        stop: Some(stop_out),
                        pnl: Some(pnl),
                    });
                    if pos.qty > 1e-12 {
                        position = Some(pos);
                    }
                } else {
                    position = Some(pos);
                }
            }
        }

        // 2. Intrabar stop on the active position (a stop updated by this
        //    bar's queued intent activates NEXT bar, like any order).
        if let Some(pos) = &position {
            if let Some(stop) = pos.stop {
                let hit = if pos.long { c.low <= stop } else { c.high >= stop };
                if hit {
                    let mut pos = position.take().expect("checked above");
                    let price = stop_fill_price(pos.long, c.open, stop);
                    let frac = pos.exit_pct.map(|p| (p / 100.0).clamp(0.0, 1.0)).unwrap_or(1.0);
                    let (closed_qty, pnl) = close_leg(&mut pos, frac, price, knobs);
                    realized += pnl;
                    closed += 1.0;
                    if pnl > 0.0 { risk_state.consecutive_losses = 0; } else { risk_state.consecutive_losses += 1; }
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
                        qty: closed_qty,
                        price,
                        stop: pos.stop,
                        pnl: Some(pnl),
                    });
                    // Pine consumes a filled exit order: a partial fill
                    // leaves the remainder UNCOVERED -- the script arms the
                    // next leg with a fresh strategy.exit call.
                    if pos.qty > 1e-12 {
                        pos.stop = None;
                        pos.limit = None;
                        pos.exit_pct = None;
                        position = Some(pos);
                    }
                }
            }
        }

        // 2b. Intrabar limit on the active position. The stop check runs
        // first, so a bar that would touch both resolves to the stop -- the
        // conservative read, since nothing about the bar's path is known.
        if let Some(pos) = &position {
            if let Some(limit) = pos.limit {
                let hit = if pos.long { c.high >= limit } else { c.low <= limit };
                if hit {
                    let mut pos = position.take().expect("checked above");
                    let price = limit_fill_price(pos.long, c.open, limit);
                    let frac = pos.exit_pct.map(|p| (p / 100.0).clamp(0.0, 1.0)).unwrap_or(1.0);
                    let (closed_qty, pnl) = close_leg(&mut pos, frac, price, knobs);
                    realized += pnl;
                    closed += 1.0;
                    if pnl > 0.0 { risk_state.consecutive_losses = 0; } else { risk_state.consecutive_losses += 1; }
                    if pnl > 0.0 {
                        wins += 1.0;
                        gross_win += pnl;
                    } else {
                        gross_loss -= pnl;
                    }
                    sim.orders.push(SimOrder {
                        bar,
                        kind: OrderKind::Limit,
                        long: pos.long,
                        qty: closed_qty,
                        price,
                        stop: pos.stop,
                        pnl: Some(pnl),
                    });
                    if pos.qty > 1e-12 {
                        // The filled limit order is consumed (Pine
                        // semantics); the remainder needs a fresh arm.
                        pos.stop = None;
                        pos.limit = None;
                        pos.exit_pct = None;
                        pos.trail_points = None;
                        pos.trail_price = None;
                        pos.trail_offset = None;
                        position = Some(pos);
                    }
                }
            }
        }

        // 2c. The partial ladder: each armed rung fills at its own level,
        // closing its percent of the ORIGINAL quantity (Pine's qty_percent
        // reads percent of the entry). The bracket stays armed underneath.
        if let Some(pos) = &mut position {
            let mut hit_idx: Option<usize> = None;
            for (i, (limit, _)) in pos.partials.iter().enumerate() {
                let hit = if pos.long { c.high >= *limit } else { c.low <= *limit };
                if hit {
                    hit_idx = Some(i);
                    break;
                }
            }
            if let Some(i) = hit_idx {
                let (limit, pct) = pos.partials[i];
                let mut pos = position.take().expect("checked above");
                let price = limit_fill_price(pos.long, c.open, limit);
                let closed_qty = (pos.original_qty * (pct / 100.0)).min(pos.qty);
                let frac = (closed_qty / pos.qty).clamp(0.0, 1.0);
                let (closed_qty, pnl) = close_leg(&mut pos, frac, price, knobs);
                realized += pnl;
                closed += 1.0;
                if pnl > 0.0 { risk_state.consecutive_losses = 0; } else { risk_state.consecutive_losses += 1; }
                if pnl > 0.0 {
                    wins += 1.0;
                    gross_win += pnl;
                } else {
                    gross_loss -= pnl;
                }
                sim.orders.push(SimOrder {
                    bar,
                    kind: OrderKind::Limit,
                    long: pos.long,
                    qty: closed_qty,
                    price,
                    stop: pos.stop,
                    pnl: Some(pnl),
                });
                pos.partials.remove(i);
                if pos.qty > 1e-12 {
                    position = Some(pos);
                }
            }
        }

        // 3. Mark the account at this close.
        let open_pnl_mark = position.as_ref().map_or(0.0, |p| open_pnl(p, c.close, knobs));
        let equity = knobs.initial_capital + realized + open_pnl_mark;
        peak = peak.max(equity);
        if peak > 0.0 {
            max_dd = max_dd.max((peak - equity) / peak);
        }
        let (pos_size, avg, op) = position.as_ref().map_or((0.0, 0.0, open_pnl_mark), |p| {
            (if p.long { p.qty } else { -p.qty }, p.entry_price, open_pnl_mark)
        });
        // strategy.risk halts: max_intraday_loss resets each calendar day,
        // max_drawdown is permanent once breached.
        let day = analytics_core::types::Timeframe::D1.bucket_of(c.open_time);
        if day != risk_day_id {
            risk_day_id = day;
            risk_day_floor = realized;
            risk_day_halted = false;
        }
        if let Some(cap) = risk_max_day_loss {
            let day_loss = (risk_day_floor - (realized + open_pnl_mark)).max(0.0);
            if day_loss > cap {
                risk_day_halted = true;
            }
        }
        if let Some(cap) = risk_max_dd {
            if peak - equity > cap {
                risk_dd_halted = true;
            }
        }
        // The risk layer's state refresh: equity and peak ride the close.
        risk_state.equity = equity;
        risk_state.peak = risk_state.peak.max(equity);
        // The position-management rules, applied on the open trade with its
        // own entry context: break-even, then the ATR trail -- a stop that
        // only ever tightens.
        if let (Some(rules), Some(pos)) = (&knobs.management, position.as_mut()) {
            let atr_value = analytics_core::indicators::atr::atr(&candles[..=bar], 14)[bar].unwrap_or(0.0);
            let favorable = if pos.long {
                candles[..=bar].iter().map(|k| k.high).fold(f64::MIN, f64::max).max(c.close)
            } else {
                candles[..=bar].iter().map(|k| k.low).fold(f64::MAX, f64::min).min(c.close)
            };
            // The trade's own R: entry to its CURRENT stop (the sim has no
            // initial-stop field; the current stop before management IS the
            // widest it has been).
            let current = pos.stop.unwrap_or(pos.entry_price);
            let new_stop = analytics_core::risk::managed_stop(
                pos.long,
                pos.entry_price,
                current,
                current,
                favorable,
                atr_value,
                rules,
            );
            if let Some(stop) = pos.stop.as_mut() {
                *stop = new_stop;
            }
        }

        sim.equity.push(equity);
        max_dd_abs = max_dd_abs.max(peak - equity);
        sim.snapshots.push(SimSnapshot {
            position_size: pos_size,
            avg_price: avg,
            equity,
            openprofit: op,
            closedtrades: closed,
            wintrades: wins,
            netprofit: realized,
            grossprofit: gross_win,
            grossloss: gross_loss,
            max_drawdown: max_dd_abs,
        });

        // 4. This bar's intents decide for the NEXT open. `strategy.exit`
        // with a stop ARMS the standing stop (Pine semantics: the position
        // closes only when price touches it) and carries for the next entry;
        // a stop-less exit and every close/close_all are market exits.
        let bar_intents: Vec<&Intent> = intents.iter().filter(|(b, _)| *b == bar).map(|(_, i)| i).collect();
        // A market close beats an entry the same bar -- but an exit that
        // only ARMS (a stop, a limit, or a trailing stop) is no close at
        // all, so the trail knobs count as arming here.
        let close_wins = bar_intents.iter().any(|i| {
            matches!(i, Intent::Close { .. })
                || matches!(
                    i,
                    Intent::Exit {
                        stop: None,
                        limit: None,
                        trail_points: None,
                        trail_price: None,
                        trail_offset: None,
                        ..
                    }
                )
        });
        for intent in &bar_intents {
            match intent {
                Intent::Exit { stop, limit, qty_percent, trail_points, trail_price, trail_offset, .. } => {
                    let mut armed = false;
                    // The stacked partial: a limit with a fraction and no
                    // stop is one rung of the partial ladder, not the
                    // bracket -- append it, and the per-bar fill below
                    // closes each rung at its own level.
                    if stop.is_none() && limit.is_some() && qty_percent.is_some() {
                        let rung = (limit.expect("checked"), qty_percent.expect("checked"));
                        match &mut position {
                            Some(pos) => pos.partials.push(rung),
                            None => pending_partials.push(rung),
                        }
                        armed = true;
                    }
                    if !armed {
                    if let Some(s) = stop {
                        if let Some(pos) = &mut position {
                            pos.stop = Some(*s);
                        }
                        pending_stop = Some(*s);
                        armed = true;
                    }
                    if let Some(l) = limit {
                        if let Some(pos) = &mut position {
                            pos.limit = Some(*l);
                        }
                        pending_limit = Some(*l);
                        armed = true;
                    }
                    if trail_points.is_some() || trail_price.is_some() || trail_offset.is_some() {
                        if let Some(pos) = &mut position {
                            pos.trail_points = *trail_points;
                            pos.trail_price = *trail_price;
                            pos.trail_offset = *trail_offset;
                        }
                        pending_trail = (*trail_points, *trail_price, *trail_offset);
                        armed = true;
                    }
                    if armed {
                        // The fraction rides the bracket it armed.
                        if let Some(pos) = &mut position {
                            pos.exit_pct = *qty_percent;
                        }
                        pending_pct = *qty_percent;
                    }
                    }
                    if !armed {
                        pending = Some(Intent::Close { id: None, qty_percent: *qty_percent });
                    }
                }
                Intent::Close { qty_percent, .. } => {
                    pending = Some(Intent::Close { id: None, qty_percent: *qty_percent });
                }
                Intent::RiskLimit { kind, value } => {
                    match kind {
                        RiskLimitKind::MaxPositionSize => risk_max_qty = Some(*value),
                        RiskLimitKind::MaxIntradayLoss => risk_max_day_loss = Some(*value),
                        RiskLimitKind::MaxDrawdown => risk_max_dd = Some(*value),
                    }
                }
                Intent::RiskDirection { allow_long, allow_short } => {
                    risk_allow = (*allow_long, *allow_short);
                }
                Intent::Entry { long, qty, .. } if !close_wins => {
                    // Pine/TV: an entry while a position is open is refused
                    // past the pyramiding budget (0 = a single leg); a
                    // same-direction entry within the budget stacks.
                    let stacking = position.as_ref().is_some_and(|p| {
                        p.long == *long && p.legs < 1 + knobs.pyramiding
                    });
                    // strategy.risk gates: the direction must be allowed and
                    // no halt may be active.
                    let direction_ok = if *long { risk_allow.0 } else { risk_allow.1 };
                    if (position.is_none() || stacking) && direction_ok && !risk_dd_halted && !risk_day_halted {
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
mod partial_tests {
    use super::*;
    use analytics_core::types::{Candle, Timeframe};

    fn k(bar: usize, o: f64, h: f64, l: f64, c: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: bar as i64 * 3_600_000_000_000,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    #[test]
    fn stacked_partials_ladder_out_of_the_position() {
        // Entry at bar 0; the ladder: half at 105 (+1R from a 5-wide stop),
        // the rest at 110 (+2R). Two rungs, two bars, the position halves
        // then closes.
        let candles = vec![
            k(0, 100.0, 100.5, 99.5, 100.0),
            k(1, 100.0, 106.0, 99.8, 105.5), // the first rung touches
            k(2, 105.5, 111.0, 105.0, 110.5), // the second rung touches
        ];
        let intents = vec![
            (0, Intent::Entry { id: "C".to_string(), long: true, qty: None }),
            (0, Intent::Exit {
                id: "C".to_string(),
                stop: Some(95.0),
                limit: None,
                trail_points: None,
                trail_offset: None,
                trail_price: None,
                qty_percent: None,
            }),
            (0, Intent::Exit {
                id: "C".to_string(),
                stop: None,
                limit: Some(105.0),
                trail_points: None,
                trail_offset: None,
                trail_price: None,
                qty_percent: Some(50.0),
            }),
            (0, Intent::Exit {
                id: "C".to_string(),
                stop: None,
                limit: Some(110.0),
                trail_points: None,
                trail_offset: None,
                trail_price: None,
                qty_percent: Some(50.0),
            }),
        ];
        let knobs = StrategyHeader::default();
        let sim = simulate(&intents, &candles, &knobs);
        let limits: Vec<_> = sim.orders.iter().filter(|o| o.kind == OrderKind::Limit).collect();
        assert_eq!(limits.len(), 2, "both rungs filled: {:?}", sim.orders.iter().map(|o| (o.kind, o.qty)).collect::<Vec<_>>());
        assert!(limits[0].qty > 0.0 && limits[1].qty > 0.0);
        // Each rung closed half of the ORIGINAL quantity.
        let entry = sim.orders.iter().find(|o| o.kind == OrderKind::Entry).expect("entry");
        let half = entry.qty / 2.0;
        assert!((limits[0].qty - half).abs() < 1e-9, "the first rung closed half the entry: {} vs {half}", limits[0].qty);
        assert!((limits[1].qty - half).abs() < 1e-9, "the second rung closed the other half: {} vs {half}", limits[1].qty);
        assert_eq!(sim.report.total_trades, 2.0, "two partial closes");
    }
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
        (bar, Intent::Close { id: None, qty_percent: None })
    }

    #[test]
    fn max_drawdown_halts_new_entries_once_breached() {
        let candles = |closes: &[(f64, f64, f64)]| -> Vec<analytics_core::types::Candle> {
            closes
                .iter()
                .enumerate()
                .map(|(i, (h, l, c))| analytics_core::types::Candle {
                    symbol: "T".to_string(),
                    timeframe: analytics_core::types::Timeframe::H1,
                    open_time: i as i64 * analytics_core::types::Timeframe::H1.nanos(),
                    open: *c,
                    high: *h,
                    low: *l,
                    close: *c,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                })
                .collect()
        };
        // A losing long (100 -> 92 breaches the 6.0 dd cap), then a signal
        // that should be refused, then a recovery.
        let cs = candles(&[
            (101.0, 99.0, 100.0), // 0: entry signal
            (100.5, 95.0, 96.0),  // 1: entry at open 100.5, slides
            (96.0, 91.0, 92.0),   // 2: dd = 10000->9991.5+(-8.5) ... breaches 6.0
            (93.0, 91.5, 92.5),   // 3: second signal -- must be refused
            (110.0, 92.0, 108.0), // 4: recovery never trades
        ]);
        let intents = vec![
            (0, Intent::Entry { id: "L".into(), long: true, qty: Some(1.0) }),
            (0, Intent::RiskLimit { kind: RiskLimitKind::MaxDrawdown, value: 6.0 }),
            (2, Intent::Entry { id: "L2".into(), long: true, qty: Some(1.0) }),
        ];
        let knobs = StrategyHeader {
            default_qty_type: QtyType::Fixed,
            default_qty_value: 1.0,
            commission_value: 0.0,
            slippage_pct: 0.0,
            ..StrategyHeader::default()
        };
        let sim = simulate(&intents, &cs, &knobs);
        let entries: Vec<&SimOrder> = sim.orders.iter().filter(|o| o.kind == OrderKind::Entry).collect();
        assert_eq!(entries.len(), 1, "the breach bars the second entry: {:?}", sim.orders);
    }

    #[test]
    fn a_deriv_contract_backtest_uses_multiplier_pnl_and_the_stop_out() {
        let candles = |bars: &[(f64, f64, f64)]| -> Vec<analytics_core::types::Candle> {
            bars.iter()
                .enumerate()
                .map(|(i, (h, l, c))| analytics_core::types::Candle {
                    symbol: "DERIV:VOLATILITY 75".to_string(),
                    timeframe: analytics_core::types::Timeframe::H1,
                    open_time: i as i64 * analytics_core::types::Timeframe::H1.nanos(),
                    open: *c,
                    high: *h,
                    low: *l,
                    close: *c,
                    volume: 0.0,
                    buy_volume: 0.0,
                    sell_volume: 0.0,
                })
                .collect()
        };
        let knobs = StrategyHeader {
            initial_capital: 1_000.0,
            default_qty_type: QtyType::Fixed,
            default_qty_value: 10.0,
            commission_value: 0.0,
            slippage_pct: 0.0,
            contracts: true,
            multiplier: 100.0,
            ..StrategyHeader::default()
        };

        // Ride to a good exit: 101 -> 102 is +0.99% at 100x = +99% of the
        // 10 stake = +9.90 (the multiplier mark, not the spot mark of +0.99).
        let cs = candles(&[
            (101.0, 99.0, 100.0), // 0: entry signal
            (102.0, 100.0, 101.0), // 1: fills at open 100-ish (slippage 0 -> open)
            (103.0, 101.5, 102.0), // 2: exit signal
            (103.0, 101.0, 102.0), // 3: fills the close at open 102
        ]);
        let intents = vec![
            (0, Intent::Entry { id: "L".into(), long: true, qty: Some(10.0) }),
            (2, Intent::Close { id: None, qty_percent: None }),
        ];
        let sim = simulate(&intents, &cs, &knobs);
        let exit = sim.orders.iter().find(|o| o.pnl.is_some()).expect("a closing leg");
        assert!(
            (exit.pnl.unwrap() - 9.900990099009901).abs() < 1e-9,
            "the multiplier mark at the close: {}",
            exit.pnl.unwrap()
        );

        // The stop-out: -1% at 100x consumes the whole 10 stake, at the
        // venue's own level (99% of entry for the long).
        let cs = candles(&[
            (101.0, 99.0, 100.0), // 0: entry
            (100.5, 98.0, 99.0),  // 1: fills at open 100, low 98 breaches the 99.0 stop-out
            (102.0, 97.0, 98.0),  // 2: the dead position must NOT sell again
        ]);
        let intents = vec![(0, Intent::Entry { id: "L".into(), long: true, qty: Some(10.0) })];
        let sim = simulate(&intents, &cs, &knobs);
        let stop = sim
            .orders
            .iter()
            .find(|o| o.kind == OrderKind::Stop)
            .expect("the stop-out fires");
        assert!((stop.pnl.unwrap() + 10.0).abs() < 1e-9, "the stake is gone: {}", stop.pnl.unwrap());
        assert_eq!(sim.orders.iter().filter(|o| o.pnl.is_some()).count(), 1, "no double close");
    }

    #[test]
    fn a_trailing_stop_activates_then_ratchets_to_the_peak() {
        let candles = |closes: &[(f64, f64, f64)]| -> Vec<analytics_core::types::Candle> {
            closes
                .iter()
                .enumerate()
                .map(|(i, (h, l, c))| analytics_core::types::Candle {
                    symbol: "T".to_string(),
                    timeframe: analytics_core::types::Timeframe::H1,
                    open_time: i as i64 * analytics_core::types::Timeframe::H1.nanos(),
                    open: *c,
                    high: *h,
                    low: *l,
                    close: *c,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                })
                .collect()
        };
        // Enter at 101 (bar 1's open after the bar-0 signal). trail_points
        // 3.0 activates at 104: bar 1's high (102) is too small, bar 2's
        // 105 activates (stop 104), bar 3's 107 ratchets (stop 106), and
        // bar 4's 108 ratchets again (stop 107) -- whose own low of 105
        // then triggers it, banking +6 at the stop.
        let cs = candles(&[
            (101.0, 99.0, 100.0),   // 0: signal bar
            (102.0, 100.0, 101.0),  // 1: entry fills at open 101; high 102 < 104
            (105.0, 104.5, 104.8),  // 2: activates; stop 104, low holds
            (107.0, 106.5, 106.8),  // 3: ratchets; stop 106, low holds
            (108.0, 105.0, 106.0),  // 4: stop 107; the bar's own low breaks it
            (110.0, 105.0, 108.0),  // 5: (post-exit rally the trade misses)
        ]);
        let intents = vec![
            (0, Intent::Entry { id: "L".into(), long: true, qty: Some(1.0) }),
            (0, Intent::Exit {
                id: "x".into(),
                stop: None,
                limit: None,
                qty_percent: None,
                trail_points: Some(3.0),
                trail_price: None,
                trail_offset: Some(1.0),
            }),
        ];
        let knobs = StrategyHeader {
            default_qty_type: QtyType::Fixed,
            default_qty_value: 1.0,
            commission_value: 0.0,
            slippage_pct: 0.0,
            ..StrategyHeader::default()
        };
        let sim = simulate(&intents, &cs, &knobs);
        let stops: Vec<&SimOrder> = sim.orders.iter().filter(|o| o.kind == OrderKind::Stop).collect();
        assert_eq!(stops.len(), 1, "exactly one trailing-stop fill: {:?}", sim.orders);
        assert_eq!(stops[0].bar, 4, "the stop fills on the bar that breaks 107");
        // The bar opens at 106 -- through the 107 stop -- so the gap rule
        // fills at the open, the worse (honest) price.
        assert!((stops[0].price - 106.0).abs() < 1e-9, "the gap rule fills at the open: {}", stops[0].price);
        // PnL: entry 101, exit 106 -> +5.
        assert!((stops[0].pnl.unwrap() - 5.0).abs() < 1e-9, "the run banks the trend: {:?}", stops[0].pnl);
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
        let stop_intent = (1, Intent::Exit { id: "x".into(), stop: Some(95.0), limit: None, qty_percent: None, trail_points: None, trail_price: None, trail_offset: None });
        let sim = simulate(&[entry(0, true), stop_intent], &candles, &knobs);
        let stop_order = sim.orders.iter().find(|o| o.kind == OrderKind::Stop).expect("stop filled");
        assert_eq!(stop_order.bar, 3, "the bar that dipped");
        assert_eq!(stop_order.price, 95.0, "fill at the stop, not the low");
        assert!(stop_order.pnl.expect("pnl") < 0.0, "stopped out at a loss");
    }

    #[test]
    fn exit_limit_fills_at_target_and_only_when_touched() {
        // From bar 5 the close is 102 (high 103) -- the 103 limit touches
        // intrabar without the open gapping past it.
        let candles: Vec<Candle> = (0..10)
            .map(|i| candle(i, if i >= 5 { 102.0 } else { 100.0 }))
            .collect();
        let knobs = StrategyHeader::default();
        let intents = vec![
            (0, Intent::Entry { id: "L".into(), long: true, qty: Some(1.0) }),
            (1, Intent::Exit { id: "x".into(), stop: Some(95.0), limit: Some(103.0), qty_percent: None, trail_points: None, trail_price: None, trail_offset: None }),
        ];
        let sim = simulate(&intents, &candles, &knobs);
        // The long fills at bar 1's open (100); the 103 limit touches when
        // bars jump to 104 at index 5 (high 105 >= 103) -- one Limit order.
        let limit_orders: Vec<_> = sim
            .orders
            .iter()
            .filter(|o| o.kind == OrderKind::Limit)
            .collect();
        assert_eq!(limit_orders.len(), 1, "orders: {:?}", sim.orders);
        assert!((limit_orders[0].price - 103.0).abs() < 1e-9);
        assert!(limit_orders[0].pnl.unwrap() > 0.0);
        assert!(sim.orders.iter().all(|o| o.kind != OrderKind::Stop));
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
