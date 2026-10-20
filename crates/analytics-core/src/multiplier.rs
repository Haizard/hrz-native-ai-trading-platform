//! Multiplier-contract math, Deriv's own semantics.
//!
//! The one implementation every consumer shares -- the trading adapter
//! (`market_data::deriv_trade`), the strategy simulator (`pine_lite::sim`),
//! and anything that prices a multiplier later. A multiplier is not a spot
//! position: the move scales by the leverage, the loss caps at the stake via
//! the venue's stop-out, and take-profit / stop-loss ride as absolute limit
//! orders.

use crate::types::Candle;

/// Multiplier PnL at an exit price: the move scales by the multiplier, and
/// the loss is capped at the stake (a stop-out at `1 / multiplier` of the
/// entry). `long` is MULTUP; `!long` is MULTDOWN.
#[must_use]
pub fn multiplier_pnl(entry: f64, exit: f64, stake: f64, multiplier: f64, long: bool) -> f64 {
    if entry <= 0.0 || stake <= 0.0 || multiplier <= 0.0 {
        return 0.0;
    }
    let dir = if long { 1.0 } else { -1.0 };
    let raw = stake * multiplier * (exit - entry) / entry * dir;
    raw.max(-stake)
}

/// The stop-out level: the adverse move that costs the whole stake.
#[must_use]
pub fn stop_out_level(entry: f64, multiplier: f64, long: bool) -> f64 {
    if long {
        entry * (1.0 - 1.0 / multiplier)
    } else {
        entry * (1.0 + 1.0 / multiplier)
    }
}

/// One candle's path against an open multiplier position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOutcome {
    /// Nothing triggered; the position rides.
    Open,
    /// The stop-out level was touched; the stake is lost.
    StoppedOut,
    /// `take_profit` was touched.
    TakeProfit,
    /// `stop_loss` was touched.
    StopLoss,
}

/// Evaluate one candle's range against an open multiplier position. The
/// stop-out is always the first check (the venue closes there before any
/// user limit), then the user stop-loss, then take-profit -- the same
/// conservative order the spot sim uses.
#[must_use]
pub fn path_outcome(
    candle: &Candle,
    entry: f64,
    multiplier: f64,
    long: bool,
    take_profit: Option<f64>,
    stop_loss: Option<f64>,
) -> PathOutcome {
    let stop_out = stop_out_level(entry, multiplier, long);
    if long {
        if candle.low <= stop_out {
            return PathOutcome::StoppedOut;
        }
        if let Some(sl) = stop_loss {
            if candle.low <= sl {
                return PathOutcome::StopLoss;
            }
        }
        if let Some(tp) = take_profit {
            if candle.high >= tp {
                return PathOutcome::TakeProfit;
            }
        }
    } else {
        if candle.high >= stop_out {
            return PathOutcome::StoppedOut;
        }
        if let Some(sl) = stop_loss {
            if candle.high >= sl {
                return PathOutcome::StopLoss;
            }
        }
        if let Some(tp) = take_profit {
            if candle.low <= tp {
                return PathOutcome::TakeProfit;
            }
        }
    }
    PathOutcome::Open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Candle, Timeframe};

    #[test]
    fn pnl_scales_and_caps() {
        assert!((multiplier_pnl(100.0, 101.0, 10.0, 100.0, true) - 10.0).abs() < 1e-9);
        assert_eq!(multiplier_pnl(100.0, 101.0, 10.0, 100.0, false), -10.0);
        assert_eq!(multiplier_pnl(100.0, 98.0, 10.0, 100.0, true), -10.0);
        assert!((stop_out_level(100.0, 100.0, true) - 99.0).abs() < 1e-9);
        assert!((stop_out_level(100.0, 100.0, false) - 101.0).abs() < 1e-9);
        assert!((stop_out_level(100.0, 50.0, true) - 98.0).abs() < 1e-9);
    }

    #[test]
    fn paths_precedence() {
        let c = |h: f64, l: f64| Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: 100.0,
            high: h,
            low: l,
            close: 100.0,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        };
        // Riding: the low (99.5) touches neither the stop-out (99.0) nor the
        // user stop (99.2), and the high is short of the take-profit.
        assert_eq!(
            path_outcome(&c(101.0, 99.5), 100.0, 100.0, true, Some(105.0), Some(99.2)),
            PathOutcome::Open
        );
        assert_eq!(
            path_outcome(&c(101.0, 98.9), 100.0, 100.0, true, Some(105.0), Some(99.5)),
            PathOutcome::StoppedOut
        );
        assert_eq!(
            path_outcome(&c(101.0, 99.4), 100.0, 100.0, true, Some(105.0), Some(99.5)),
            PathOutcome::StopLoss
        );
        assert_eq!(
            path_outcome(&c(105.5, 99.5), 100.0, 100.0, true, Some(105.0), Some(99.2)),
            PathOutcome::TakeProfit
        );
        assert_eq!(
            path_outcome(&c(101.1, 99.0), 100.0, 100.0, false, None, None),
            PathOutcome::StoppedOut
        );
        assert_eq!(
            path_outcome(&c(100.5, 94.0), 100.0, 100.0, false, Some(95.0), None),
            PathOutcome::TakeProfit
        );
    }
}
