//! CVD -- Cumulative Volume Delta.
//!
//! The running sum of [`delta`](crate::delta) across candles. Divergence
//! between CVD and price is one of the core order-flow signals: price making a
//! new low while CVD does not means sellers are running out of aggression.
//!
//! ## Sessions
//!
//! Crypto trades 24/7, so a "session" here is a **UTC calendar day**. When
//! `reset_at_session` is on, the accumulator returns to zero at each day
//! boundary and the first candle of the new session reads its own delta. That
//! keeps the line comparable across days instead of drifting forever.

use serde::{Deserialize, Serialize};

use crate::types::{Candle, NS_PER_SEC};

/// Nanoseconds in one UTC day.
pub const NS_PER_DAY: i64 = 86_400 * NS_PER_SEC;

/// The UTC-day index a timestamp falls in.
///
/// Uses `div_euclid` so pre-1970 timestamps floor correctly instead of
/// truncating toward zero.
#[must_use]
pub const fn session_of(ts: i64) -> i64 {
    ts.div_euclid(NS_PER_DAY)
}

/// Cumulative delta across `candles`.
///
/// With `reset_at_session` the series restarts at each UTC day boundary;
/// otherwise it accumulates from the first candle.
#[must_use]
pub fn calculate_cvd(candles: &[Candle], reset_at_session: bool) -> Vec<f64> {
    let mut engine = Cvd::new(reset_at_session);
    candles.iter().map(|c| engine.update(c)).collect()
}

/// Stateful CVD accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cvd {
    running: f64,
    reset_at_session: bool,
    current_session: Option<i64>,
}

impl Cvd {
    /// A fresh accumulator.
    #[must_use]
    pub const fn new(reset_at_session: bool) -> Self {
        Self {
            running: 0.0,
            reset_at_session,
            current_session: None,
        }
    }

    /// Feed a candle and return the updated cumulative value.
    pub fn update(&mut self, candle: &Candle) -> f64 {
        if self.reset_at_session {
            let session = session_of(candle.open_time);
            if self.current_session != Some(session) {
                self.running = 0.0;
                self.current_session = Some(session);
            }
        }

        self.running += candle.delta();
        self.running
    }

    /// The current cumulative value.
    #[must_use]
    pub const fn value(&self) -> f64 {
        self.running
    }

    /// Force the accumulator back to zero (e.g. on a manual session break).
    pub fn reset(&mut self) {
        self.running = 0.0;
        self.current_session = None;
    }
}

/// CVD divergence between price and cumulative delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CvdDivergence {
    /// Price made a new low, CVD did not -- sellers losing conviction.
    Bullish,
    /// Price made a new high, CVD did not -- buyers losing conviction.
    Bearish,
    /// No divergence in the window.
    None,
}

/// Compare the extremes of price and CVD over a window.
///
/// Returns [`CvdDivergence::Bullish`] when the lowest low of the window occurs
/// in the most recent portion while CVD's minimum happened earlier, and the
/// mirror image for bearish. `lookback` is the number of trailing candles to
/// consider.
#[must_use]
pub fn detect_cvd_divergence(candles: &[Candle], cvd: &[f64], lookback: usize) -> CvdDivergence {
    if candles.len() < 2 || cvd.len() != candles.len() {
        return CvdDivergence::None;
    }

    let start = candles.len().saturating_sub(lookback.max(2));
    let window = &candles[start..];
    let window_cvd = &cvd[start..];

    let (price_low_idx, price_low) = argmin_by(window, |c| c.low);
    let (price_high_idx, price_high) = argmax_by(window, |c| c.high);
    let (cvd_low_idx, cvd_low) = argmin_by_f64(window_cvd);
    let (cvd_high_idx, cvd_high) = argmax_by_f64(window_cvd);

    // Guard against a perfectly flat series, which would be noise, not a signal.
    let price_range = price_high - price_low;
    let cvd_range = cvd_high - cvd_low;
    if price_range <= f64::EPSILON || cvd_range <= f64::EPSILON {
        return CvdDivergence::None;
    }

    // Price printed its low late in the window, CVD's low was earlier.
    if price_low_idx > cvd_low_idx {
        return CvdDivergence::Bullish;
    }
    if price_high_idx > cvd_high_idx {
        return CvdDivergence::Bearish;
    }

    CvdDivergence::None
}

fn argmin_by<T, F: Fn(&T) -> f64>(items: &[T], key: F) -> (usize, f64) {
    let mut best = (0usize, f64::INFINITY);
    for (i, item) in items.iter().enumerate() {
        let v = key(item);
        if v < best.1 {
            best = (i, v);
        }
    }
    best
}

fn argmax_by<T, F: Fn(&T) -> f64>(items: &[T], key: F) -> (usize, f64) {
    let mut best = (0usize, f64::NEG_INFINITY);
    for (i, item) in items.iter().enumerate() {
        let v = key(item);
        if v > best.1 {
            best = (i, v);
        }
    }
    best
}

fn argmin_by_f64(items: &[f64]) -> (usize, f64) {
    argmin_by(items, |v| *v)
}

fn argmax_by_f64(items: &[f64]) -> (usize, f64) {
    argmax_by(items, |v| *v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(open_time: i64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    #[test]
    fn cvd_is_a_running_sum() {
        let candles = vec![
            candle(0, 7.0, 3.0), // +4  -> 4
            candle(0, 2.0, 8.0), // -6  -> -2
            candle(0, 5.0, 5.0), //  0  -> -2
        ];
        assert_eq!(calculate_cvd(&candles, false), vec![4.0, -2.0, -2.0]);
    }

    #[test]
    fn cvd_accumulates_across_sessions_when_not_resetting() {
        let candles = vec![candle(0, 10.0, 0.0), candle(2 * NS_PER_DAY, 10.0, 0.0)];
        assert_eq!(calculate_cvd(&candles, false), vec![10.0, 20.0]);
    }

    #[test]
    fn cvd_resets_at_the_utc_day_boundary() {
        let last_minute_of_day = NS_PER_DAY - 60 * NS_PER_SEC;
        let candles = vec![
            candle(0, 10.0, 0.0),                           // day 0 -> 10
            candle(last_minute_of_day, 5.0, 0.0),           // day 0 -> 15
            candle(NS_PER_DAY, 3.0, 0.0),                   // day 1 -> 3 (reset)
            candle(NS_PER_DAY + 60 * NS_PER_SEC, 1.0, 0.0), // day 1 -> 4
        ];
        assert_eq!(calculate_cvd(&candles, true), vec![10.0, 15.0, 3.0, 4.0]);
    }

    #[test]
    fn session_index_is_stable_and_floors_for_negative_times() {
        assert_eq!(session_of(0), 0);
        assert_eq!(session_of(NS_PER_DAY - 1), 0);
        assert_eq!(session_of(NS_PER_DAY), 1);
        // -1ns is still inside day -1, not day 0.
        assert_eq!(session_of(-1), -1);
    }

    #[test]
    fn engine_exposes_running_value() {
        let mut cvd = Cvd::new(false);
        cvd.update(&candle(0, 4.0, 1.0));
        cvd.update(&candle(0, 1.0, 4.0));
        assert!((cvd.value()).abs() < f64::EPSILON);

        cvd.reset();
        assert!((cvd.value()).abs() < f64::EPSILON);
    }

    #[test]
    fn bullish_divergence_when_price_lows_late_but_cvd_low_was_early() {
        // CVD bottoms at index 0; price bottoms at index 2.
        let candles = vec![
            candle(0, 0.0, 10.0), // delta -10
            candle(0, 10.0, 5.0), // delta  +5
            candle(0, 10.0, 0.0), // delta +10
        ];
        let mut c = candles.clone();
        c[0].low = 95.0;
        c[1].low = 97.0;
        c[2].low = 90.0; // price low is last
        let cvd = calculate_cvd(&c, false);

        assert_eq!(detect_cvd_divergence(&c, &cvd, 3), CvdDivergence::Bullish);
    }

    #[test]
    fn no_divergence_on_flat_input() {
        let candles = vec![candle(0, 5.0, 5.0), candle(0, 5.0, 5.0)];
        let cvd = calculate_cvd(&candles, false);
        assert_eq!(
            detect_cvd_divergence(&candles, &cvd, 2),
            CvdDivergence::None
        );
    }
}
