//! Delta -- aggressive buying minus aggressive selling.
//!
//! Delta answers "who was in control during this candle?". It is the building
//! block for CVD and the footprint, and it is why candles carry a
//! `buy_volume`/`sell_volume` split rather than just a total.

use serde::{Deserialize, Serialize};

use crate::types::{Candle, Trade};

/// Per-candle delta: `buy_volume - sell_volume`.
///
/// Positive means buyers were the aggressors (lifting the ask); negative means
/// sellers were (hitting the bid).
#[must_use]
pub fn calculate_delta(candle: &Candle) -> f64 {
    candle.delta()
}

/// Delta for every candle, in order.
#[must_use]
pub fn calculate_deltas(candles: &[Candle]) -> Vec<f64> {
    candles.iter().map(calculate_delta).collect()
}

/// Delta computed directly from a trade slice.
///
/// Uses the same aggressor classification as [`Candle`] construction, so this
/// always agrees with [`calculate_delta`] for the matching candle.
#[must_use]
pub fn calculate_delta_from_trades(trades: &[Trade]) -> f64 {
    trades.iter().map(Trade::signed_quantity).sum()
}

/// A running delta reading with a simple classify step.
///
/// Handy for the Strategy DSL, where a condition wants "is delta strongly
/// positive?" without re-deriving thresholds each time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DeltaReading {
    /// Raw delta.
    pub delta: f64,
    /// Total volume traded in the candle.
    pub volume: f64,
    /// `delta / volume` in `[-1, 1]`; `0` when volume is zero.
    ///
    /// More comparable across candles than raw delta, because it is not
    /// inflated by a high-volume session.
    pub delta_ratio: f64,
}

impl DeltaReading {
    /// Compute a reading from a candle.
    #[must_use]
    pub fn from_candle(candle: &Candle) -> Self {
        Self {
            delta: candle.delta(),
            volume: candle.volume,
            delta_ratio: ratio(candle.delta(), candle.volume),
        }
    }

    /// Whether the reading is beyond `threshold` in absolute ratio terms.
    #[must_use]
    pub fn is_strong(&self, threshold: f64) -> bool {
        self.delta_ratio.abs() >= threshold
    }
}

fn ratio(delta: f64, volume: f64) -> f64 {
    if volume.abs() < f64::EPSILON {
        0.0
    } else {
        (delta / volume).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    #[test]
    fn delta_is_buy_minus_sell() {
        assert!((calculate_delta(&candle(7.0, 3.0)) - 4.0).abs() < f64::EPSILON);
        assert!((calculate_delta(&candle(3.0, 7.0)) + 4.0).abs() < f64::EPSILON);
        assert!((calculate_delta(&candle(5.0, 5.0))).abs() < f64::EPSILON);
    }

    #[test]
    fn deltas_are_per_candle() {
        let candles = vec![candle(7.0, 3.0), candle(2.0, 8.0)];
        let deltas = calculate_deltas(&candles);
        assert_eq!(deltas.len(), 2);
        assert!((deltas[0] - 4.0).abs() < f64::EPSILON);
        assert!((deltas[1] + 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn delta_from_trades_matches_candle_delta() {
        let trades = vec![
            Trade {
                symbol: "BTCUSDT".into(),
                trade_id: 1,
                price: 100.0,
                quantity: 7.0,
                is_buyer_maker: false, // buyer aggressed -> +7
                timestamp: 0,
            },
            Trade {
                symbol: "BTCUSDT".into(),
                trade_id: 2,
                price: 100.0,
                quantity: 3.0,
                is_buyer_maker: true, // seller aggressed -> -3
                timestamp: 0,
            },
        ];
        assert!((calculate_delta_from_trades(&trades) - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn delta_ratio_is_normalized_and_clamped() {
        let r = DeltaReading::from_candle(&candle(8.0, 2.0));
        assert!((r.delta - 6.0).abs() < f64::EPSILON);
        assert!((r.delta_ratio - 0.6).abs() < 1e-9);
        assert!(r.is_strong(0.5));
        assert!(!r.is_strong(0.7));
    }

    #[test]
    fn zero_volume_does_not_divide_by_zero() {
        let r = DeltaReading::from_candle(&candle(0.0, 0.0));
        assert!((r.delta_ratio).abs() < f64::EPSILON);
        assert!(!r.is_strong(0.01));
    }
}
