//! VWAP -- Volume Weighted Average Price.
//!
//! VWAP is the volume-weighted fair price over a window. Institutional flow
//! tends to reference it, so "price above VWAP" is a widely-watched bias
//! signal, and an *anchored* VWAP (from a specific event, e.g. a session open
//! or a liquidity sweep) is a common order-flow anchor.
//!
//! Typical price is `(high + low + close) / 3`, the standard approximation when
//! only OHLCV is available. When trades are available prefer
//! [`calculate_vwap_from_trades`], which uses actual execution prices.

use serde::{Deserialize, Serialize};

use crate::cvd::{session_of, NS_PER_DAY};
use crate::types::{Candle, Trade};

/// Typical price of a candle.
#[must_use]
pub fn typical_price(candle: &Candle) -> f64 {
    (candle.high + candle.low + candle.close) / 3.0
}

/// VWAP over the whole slice, or `None` if no volume traded.
#[must_use]
pub fn calculate_vwap(candles: &[Candle]) -> Option<f64> {
    let mut pv = 0.0;
    let mut vol = 0.0;
    for candle in candles {
        pv += typical_price(candle) * candle.volume;
        vol += candle.volume;
    }
    divide(pv, vol)
}

/// VWAP anchored at `anchor_ns`: only candles at or after the anchor count.
///
/// Returns `None` when the anchor is past every candle or no volume traded
/// after it.
#[must_use]
pub fn calculate_anchored_vwap(candles: &[Candle], anchor_ns: i64) -> Option<f64> {
    let from = candles.partition_point(|c| c.open_time < anchor_ns);
    calculate_vwap(&candles[from..])
}

/// VWAP computed from raw trades, weighted by executed size at actual prices.
#[must_use]
pub fn calculate_vwap_from_trades(trades: &[Trade]) -> Option<f64> {
    let mut pv = 0.0;
    let mut vol = 0.0;
    for trade in trades {
        pv += trade.price * trade.quantity;
        vol += trade.quantity;
    }
    divide(pv, vol)
}

/// Rolling VWAP, optionally restarting at each UTC day boundary.
///
/// `None` entries mean "no volume yet in this window" rather than zero, which
/// matters: a zero VWAP would look like a real price level on a chart.
#[must_use]
pub fn calculate_vwap_series(candles: &[Candle], reset_at_session: bool) -> Vec<Option<f64>> {
    let mut engine = Vwap::new(reset_at_session);
    candles.iter().map(|c| engine.update(c)).collect()
}

/// Incremental VWAP accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vwap {
    price_volume: f64,
    volume: f64,
    reset_at_session: bool,
    current_session: Option<i64>,
}

impl Vwap {
    /// A fresh accumulator.
    #[must_use]
    pub const fn new(reset_at_session: bool) -> Self {
        Self {
            price_volume: 0.0,
            volume: 0.0,
            reset_at_session,
            current_session: None,
        }
    }

    /// Feed a candle; returns the VWAP so far, or `None` if no volume yet.
    pub fn update(&mut self, candle: &Candle) -> Option<f64> {
        if self.reset_at_session {
            let session = session_of(candle.open_time);
            if self.current_session != Some(session) {
                self.price_volume = 0.0;
                self.volume = 0.0;
                self.current_session = Some(session);
            }
        }

        self.price_volume += typical_price(candle) * candle.volume;
        self.volume += candle.volume;
        self.value()
    }

    /// The current VWAP, or `None` if nothing has been fed in.
    #[must_use]
    pub fn value(&self) -> Option<f64> {
        divide(self.price_volume, self.volume)
    }

    /// Anchor the accumulator at `anchor_ns`, discarding earlier volume.
    ///
    /// Call this when replaying history to simulate an anchored VWAP without
    /// re-filtering the slice.
    pub fn anchor_at(&mut self, candles: &[Candle], anchor_ns: i64) {
        self.reset();
        for candle in candles.iter().filter(|c| c.open_time >= anchor_ns) {
            self.update(candle);
        }
    }

    /// Reset to zero volume.
    pub fn reset(&mut self) {
        self.price_volume = 0.0;
        self.volume = 0.0;
        self.current_session = None;
    }
}

/// Distance of `price` from VWAP as a fraction of VWAP.
///
/// Positive means price is above VWAP. `None` when VWAP is unavailable or
/// zero.
#[must_use]
pub fn vwap_deviation(price: f64, vwap: Option<f64>) -> Option<f64> {
    let vwap = vwap?;
    if vwap.abs() < f64::EPSILON {
        return None;
    }
    Some((price - vwap) / vwap)
}

/// The UTC-day VWAP for each day present in the slice, keyed by session index.
#[must_use]
pub fn session_vwaps(candles: &[Candle]) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = Vec::new();
    let mut pv = 0.0;
    let mut vol = 0.0;
    let mut current: Option<i64> = None;

    for candle in candles {
        let session = session_of(candle.open_time);
        if current != Some(session) {
            if let (Some(prev), Some(value)) = (current, divide(pv, vol)) {
                out.push((prev, value));
            }
            pv = 0.0;
            vol = 0.0;
            current = Some(session);
        }
        pv += typical_price(candle) * candle.volume;
        vol += candle.volume;
    }

    if let (Some(prev), Some(value)) = (current, divide(pv, vol)) {
        out.push((prev, value));
    }

    out
}

/// Nanoseconds in one UTC day, re-exported for callers anchoring by day.
pub const DAY_NS: i64 = NS_PER_DAY;

fn divide(numerator: f64, denominator: f64) -> Option<f64> {
    if denominator.abs() < f64::EPSILON {
        None
    } else {
        Some(numerator / denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(open_time: i64, high: f64, low: f64, close: f64, volume: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high,
            low,
            close,
            volume,
            buy_volume: volume / 2.0,
            sell_volume: volume / 2.0,
        }
    }

    #[test]
    fn typical_price_is_the_mean_of_hlc() {
        let c = candle(0, 102.0, 96.0, 99.0, 1.0);
        assert!((typical_price(&c) - 99.0).abs() < 1e-9);
    }

    #[test]
    fn vwap_is_volume_weighted_not_a_plain_mean() {
        // tp1 = 100, tp2 = 110. Equal volumes -> 105.
        // Weighted 3:1 towards the first -> 102.5
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 3.0),
            candle(60, 110.0, 110.0, 110.0, 1.0),
        ];
        assert!((calculate_vwap(&candles).unwrap() - 102.5).abs() < 1e-9);
    }

    #[test]
    fn vwap_of_empty_or_volumeless_input_is_none() {
        assert!(calculate_vwap(&[]).is_none());
        assert!(calculate_vwap(&[candle(0, 100.0, 100.0, 100.0, 0.0)]).is_none());
    }

    #[test]
    fn anchored_vwap_ignores_candles_before_the_anchor() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 1.0),
            candle(60, 200.0, 200.0, 200.0, 1.0),
            candle(120, 200.0, 200.0, 200.0, 1.0),
        ];
        // Anchored at the second candle -> only 200s count.
        assert!((calculate_anchored_vwap(&candles, 60).unwrap() - 200.0).abs() < 1e-9);
        // Anchor past everything -> None.
        assert!(calculate_anchored_vwap(&candles, 999).is_none());
    }

    #[test]
    fn anchored_vwap_includes_the_anchor_candle_itself() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 1.0),
            candle(60, 200.0, 200.0, 200.0, 1.0),
        ];
        assert!((calculate_anchored_vwap(&candles, 0).unwrap() - 150.0).abs() < 1e-9);
    }

    #[test]
    fn trade_vwap_uses_execution_prices() {
        let trades = vec![
            Trade {
                symbol: "BTCUSDT".into(),
                trade_id: 1,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
                timestamp: 0,
            },
            Trade {
                symbol: "BTCUSDT".into(),
                trade_id: 2,
                price: 200.0,
                quantity: 3.0,
                is_buyer_maker: true,
                timestamp: 0,
            },
        ];
        // (100*1 + 200*3) / 4 = 175
        assert!((calculate_vwap_from_trades(&trades).unwrap() - 175.0).abs() < 1e-9);
    }

    #[test]
    fn vwap_series_is_none_before_any_volume() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 0.0),
            candle(60, 110.0, 110.0, 110.0, 2.0),
        ];
        let series = calculate_vwap_series(&candles, false);
        assert_eq!(series[0], None);
        assert!((series[1].unwrap() - 110.0).abs() < 1e-9);
    }

    #[test]
    fn session_reset_vwap_restarts_each_day() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 1.0),
            candle(DAY_NS, 200.0, 200.0, 200.0, 1.0),
        ];
        let series = calculate_vwap_series(&candles, true);
        assert!((series[0].unwrap() - 100.0).abs() < 1e-9);
        assert!((series[1].unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn incremental_engine_matches_batch() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 3.0),
            candle(60, 110.0, 110.0, 110.0, 1.0),
        ];
        let batch = calculate_vwap(&candles).unwrap();

        let mut engine = Vwap::new(false);
        for c in &candles {
            engine.update(c);
        }
        assert!((engine.value().unwrap() - batch).abs() < 1e-9);
    }

    #[test]
    fn deviation_is_signed_and_guards_against_zero_vwap() {
        assert!((vwap_deviation(110.0, Some(100.0)).unwrap() - 0.1).abs() < 1e-9);
        assert!((vwap_deviation(90.0, Some(100.0)).unwrap() + 0.1).abs() < 1e-9);
        assert!(vwap_deviation(100.0, None).is_none());
        assert!(vwap_deviation(100.0, Some(0.0)).is_none());
    }

    #[test]
    fn session_vwaps_are_grouped_by_utc_day() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 1.0),
            candle(60, 200.0, 200.0, 200.0, 1.0),
            candle(DAY_NS, 300.0, 300.0, 300.0, 1.0),
        ];
        let sessions = session_vwaps(&candles);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0, 0);
        assert!((sessions[0].1 - 150.0).abs() < 1e-9);
        assert_eq!(sessions[1].0, 1);
        assert!((sessions[1].1 - 300.0).abs() < 1e-9);
    }
}
