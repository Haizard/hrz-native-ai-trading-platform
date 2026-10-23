//! The risk layer: sizing, stops, guards, and the ruin math. Pure functions
//! and one state machine -- the engines that sit between a strategy's signal
//! and its order. Every formula is canonical, pinned by hand-computed values.

use crate::indicators::atr::atr;
use crate::types::Candle;

/// An ATR stop: `mult` ATR(period) beyond the entry, on the trade's side.
pub fn atr_stop(candles: &[Candle], entry_idx: usize, long: bool, period: usize, mult: f64) -> Option<f64> {
    let a = atr(candles, period)[entry_idx]?;
    let entry = candles[entry_idx].close;
    Some(if long { entry - mult * a } else { entry + mult * a })
}

/// A swing stop: beyond the newest `lookback`-swing on the trade's side
/// (below the newest swing low for a long, above the newest high for a
/// short) -- the structure that made the entry is the thing that kills it.
pub fn swing_stop(candles: &[Candle], long: bool, lookback: usize) -> Option<f64> {
    let n = candles.len();
    if n < lookback * 2 + 2 {
        return None;
    }
    let last = n - 1;
    for i in (lookback..last.saturating_sub(lookback)).rev() {
        let k = &candles[i];
        if long {
            if (i - lookback..=i + lookback).all(|j| j == i || candles[j].low > k.low) {
                return Some(k.low);
            }
        } else if (i - lookback..=i + lookback).all(|j| j == i || candles[j].high < k.high) {
            return Some(k.high);
        }
    }
    None
}

/// The volatility-adjusted quantity: risk exactly `risk_fraction` of equity,
/// so the size is the risk divided by the stop distance. The stop carries
/// the volatility; the size carries the risk.
pub fn vol_adjusted_qty(equity: f64, risk_fraction: f64, stop_distance: f64) -> f64 {
    if stop_distance <= 0.0 || risk_fraction <= 0.0 || equity <= 0.0 {
        return 0.0;
    }
    equity * risk_fraction / stop_distance
}

/// The Kelly fraction: `p - (1-p)/R`, where p is the win rate and R the
/// payoff (average win over average loss). The full Kelly is the
/// growth-optimal bet -- and twice what most desks survive; the cap is the
/// desk's answer.
pub fn kelly_fraction(win_rate: f64, payoff: f64) -> f64 {
    if payoff <= 0.0 || !(0.0..=1.0).contains(&win_rate) {
        return 0.0;
    }
    (win_rate - (1.0 - win_rate) / payoff).max(0.0)
}

/// Kelly with the desk's cap (half Kelly is the convention).
pub fn capped_kelly(win_rate: f64, payoff: f64, cap: f64) -> f64 {
    kelly_fraction(win_rate, payoff).min(cap).max(0.0)
}

/// Risk of ruin, the gambler's-ruin closed form for unit steps: the
/// probability of losing `units` consecutive risk units before winning
/// enough to never look back, with edge `e = p*R - (1-p)` in R units.
/// `((1 - e) / (1 + e))^units`; no edge, certain ruin.
pub fn risk_of_ruin(win_rate: f64, payoff: f64, risk_fraction: f64, ruin_fraction: f64) -> f64 {
    if risk_fraction <= 0.0 || ruin_fraction <= 0.0 || payoff <= 0.0 {
        return 1.0;
    }
    let edge = win_rate * payoff - (1.0 - win_rate);
    if edge <= 0.0 {
        return 1.0;
    }
    // Approximate units: the ruin level in risk units, the edge per unit.
    let units = (ruin_fraction / risk_fraction).max(1.0);
    let per_unit_edge = edge / (win_rate * payoff + (1.0 - win_rate)).max(1e-12);
    let ratio = ((1.0 - per_unit_edge) / (1.0 + per_unit_edge)).clamp(0.0, 1.0);
    ratio.powf(units)
}

/// The running account state the guards read, updated per closed trade and
/// per bar.
#[derive(Debug, Clone, Copy, Default)]
pub struct RiskState {
    /// Current equity.
    pub equity: f64,
    /// The peak equity so far.
    pub peak: f64,
    /// The day-start equity (UTC).
    pub day_start: f64,
    /// The week-start equity (UTC week).
    pub week_start: f64,
    /// Closed trades in a row that lost.
    pub consecutive_losses: usize,
    /// Currently open positions.
    pub open_positions: usize,
}

/// The guards: the limits that halt new entries when the account trips
/// them. Any guard set to 0 or None is off.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskGuards {
    /// Halt when the day is down this fraction from its start.
    pub max_daily_loss_pct: f64,
    /// Halt when the week is down this fraction.
    pub max_weekly_loss_pct: f64,
    /// Halt when equity is down this fraction from its peak.
    pub max_drawdown_pct: f64,
    /// Halt after this many consecutive losing trades.
    pub consecutive_loss_limit: usize,
    /// Halt new entries at this many open positions.
    pub max_open_positions: usize,
}

impl RiskGuards {
    /// Whether a new entry is allowed in this state. The answer is about
    /// NEW risk; managing an open position is never halted.
    pub fn allows(&self, state: &RiskState) -> bool {
        if state.equity <= 0.0 {
            return false;
        }
        if self.max_daily_loss_pct > 0.0
            && state.day_start > 0.0
            && state.equity < state.day_start * (1.0 - self.max_daily_loss_pct / 100.0)
        {
            return false;
        }
        if self.max_weekly_loss_pct > 0.0
            && state.week_start > 0.0
            && state.equity < state.week_start * (1.0 - self.max_weekly_loss_pct / 100.0)
        {
            return false;
        }
        if self.max_drawdown_pct > 0.0
            && state.peak > 0.0
            && state.equity < state.peak * (1.0 - self.max_drawdown_pct / 100.0)
        {
            return false;
        }
        if self.consecutive_loss_limit > 0 && state.consecutive_losses >= self.consecutive_loss_limit {
            return false;
        }
        if self.max_open_positions > 0 && state.open_positions >= self.max_open_positions {
            return false;
        }
        true
    }
}

/// The position-management rules, evaluated per bar on an open trade: when
/// to move the stop to break-even and when the ATR trail arms.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManagementRules {
    /// Move the stop to the entry once the trade is this many R in profit
    /// (R = the entry's own stop distance). 0 = off.
    pub break_even_r: f64,
    /// Arm the ATR trail once the trade is this many R in profit. 0 = off.
    pub trail_arm_r: f64,
    /// The trail's ATR multiple once armed. 0 = off.
    pub trail_atr_mult: f64,
}

/// The managed stop for this bar: the HIGHEST of the three candidates for a
/// long (the fixed stop, break-even, the trail), the lowest for a short. A
/// stop only tightens; it never retreats.
pub fn managed_stop(
    long: bool,
    entry: f64,
    entry_stop: f64,
    current_stop: f64,
    favorable: f64,
    atr_value: f64,
    rules: &ManagementRules,
) -> f64 {
    let r = (entry - entry_stop).abs();
    if r <= 0.0 {
        return current_stop;
    }
    let r_multiple = if long { (favorable - entry) / r } else { (entry - favorable) / r };
    let mut stop = current_stop;
    if rules.break_even_r > 0.0 && r_multiple >= rules.break_even_r {
        stop = if long { stop.max(entry) } else { stop.min(entry) };
    }
    if rules.trail_arm_r > 0.0 && rules.trail_atr_mult > 0.0 && r_multiple >= rules.trail_arm_r && atr_value > 0.0 {
        let trail = if long { favorable - rules.trail_atr_mult * atr_value } else { favorable + rules.trail_atr_mult * atr_value };
        stop = if long { stop.max(trail) } else { stop.min(trail) };
    }
    stop
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, h: f64, l: f64, c: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: c,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    #[test]
    fn the_vol_adjusted_size_risks_exactly_the_fraction() {
        // 10k equity, 1% risk, a 2.0 stop distance: 50 units.
        let qty = vol_adjusted_qty(10_000.0, 0.01, 2.0);
        assert!((qty - 50.0).abs() < 1e-9);
        // The loss at the stop IS the risk: 50 * 2.0 = 100 = 1% of 10k.
        assert!((qty * 2.0 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_kelly_fraction_is_the_canonical_form() {
        // 60% winners, payoff 2: 0.6 - 0.4/2 = 0.4.
        assert!((kelly_fraction(0.6, 2.0) - 0.4).abs() < 1e-9);
        // No edge: zero, never negative.
        assert_eq!(kelly_fraction(0.3, 1.0), 0.0);
        // The cap binds: half Kelly for the same stats is 0.2.
        assert!((capped_kelly(0.6, 2.0, 0.2) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn the_ruin_math_reads_edge_and_units() {
        // No edge: certain ruin.
        assert_eq!(risk_of_ruin(0.4, 1.0, 0.01, 0.5), 1.0);
        // A real edge, a deep ruin level, small risk: low ruin probability.
        let r = risk_of_ruin(0.55, 1.5, 0.01, 0.5);
        assert!(r < 0.5, "a positive edge at small risk is survivable: {r}");
        // Risking more per trade raises the ruin probability.
        let r2 = risk_of_ruin(0.55, 1.5, 0.05, 0.5);
        assert!(r2 > r, "bigger bets, more ruin: {r2} vs {r}");
    }

    #[test]
    fn the_guards_halt_on_their_own_tripwires() {
        let guards = RiskGuards {
            max_daily_loss_pct: 2.0,
            max_weekly_loss_pct: 0.0,
            max_drawdown_pct: 10.0,
            consecutive_loss_limit: 3,
            max_open_positions: 2,
        };
        let fine = RiskState {
            equity: 10_000.0,
            peak: 10_200.0,
            day_start: 10_100.0,
            week_start: 10_000.0,
            consecutive_losses: 1,
            open_positions: 1,
        };
        assert!(guards.allows(&fine));
        let daily_tripped = RiskState { equity: 9_800.0, ..fine }; // -3% on the day
        assert!(!guards.allows(&daily_tripped));
        let dd_tripped = RiskState { equity: 9_000.0, day_start: 9_000.0, ..fine }; // -12% from peak
        assert!(!guards.allows(&dd_tripped));
        let streak_tripped = RiskState { consecutive_losses: 3, ..fine };
        assert!(!guards.allows(&streak_tripped));
        let full = RiskState { open_positions: 2, ..fine };
        assert!(!guards.allows(&full));
        // An empty guard set always allows.
        assert!(RiskGuards::default().allows(&daily_tripped));
    }

    #[test]
    fn the_managed_stop_only_tightens() {
        let rules = ManagementRules {
            break_even_r: 1.0,
            trail_arm_r: 2.0,
            trail_atr_mult: 1.5,
        };
        // A long at 100 stopped at 98 (R=2). At +0.5R nothing moves.
        let s = managed_stop(true, 100.0, 98.0, 98.0, 101.0, 0.5, &rules);
        assert_eq!(s, 98.0);
        // At +1R (102): break-even arms.
        let s = managed_stop(true, 100.0, 98.0, 98.0, 102.0, 0.5, &rules);
        assert_eq!(s, 100.0);
        // At +2R (104): the trail arms at 104 - 0.75 = 103.25, and the stop
        // is the HIGHEST of the three.
        let s = managed_stop(true, 100.0, 98.0, 100.0, 104.0, 0.5, &rules);
        assert!((s - 103.25).abs() < 1e-9);
        // A trail never retreats: at 103 the trail reads 102.25 but the
        // existing stop holds.
        let s = managed_stop(true, 100.0, 98.0, 103.25, 103.0, 0.5, &rules);
        assert_eq!(s, 103.25);
    }

    #[test]
    fn the_atr_and_swing_stops_sit_where_they_should() {
        let candles: Vec<Candle> = (0..30)
            .map(|i| {
                let c = 100.0 + (i % 5) as f64 * 0.3;
                k(i as i64 * 3_600_000_000_000, c + 0.6, c - 0.6, c)
            })
            .collect();
        let stop = atr_stop(&candles, 29, true, 14, 2.0).expect("atr stop");
        assert!(stop < candles[29].close, "a long's stop is below the entry");
        let swing = swing_stop(&candles, true, 2).expect("swing stop");
        assert!(swing < candles[29].close, "the newest swing low is below");
    }
}
