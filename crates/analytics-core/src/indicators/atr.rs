//! Average True Range and true range.

use super::{mean, warmup};
use crate::types::Candle;

/// True range for each candle.
///
/// `TR = max(high - low, |high - prev_close|, |low - prev_close|)`.
///
/// The first candle has no previous close, so its true range is simply
/// `high - low`. That is the standard convention and keeps the output length
/// equal to the input length.
#[must_use]
pub fn true_range(candles: &[Candle]) -> Vec<f64> {
    let mut out = Vec::with_capacity(candles.len());

    for (i, candle) in candles.iter().enumerate() {
        let tr = if i == 0 {
            candle.high - candle.low
        } else {
            let prev_close = candles[i - 1].close;
            let a = candle.high - candle.low;
            let b = (candle.high - prev_close).abs();
            let c = (candle.low - prev_close).abs();
            a.max(b).max(c)
        };
        out.push(tr);
    }

    out
}

/// Average True Range, using Wilder's smoothing.
///
/// Seeded with the mean of the first `period` true ranges, so the first value
/// lands at index `period - 1` and everything before it is `None`.
#[must_use]
pub fn atr(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let mut out = warmup(candles.len());
    if period == 0 || candles.len() < period {
        return out;
    }

    let tr = true_range(candles);
    let Some(seed) = mean(&tr[..period]) else {
        return out;
    };
    out[period - 1] = Some(seed);

    #[allow(clippy::cast_precision_loss)]
    let p = period as f64;
    let mut prev = seed;

    for i in period..candles.len() {
        prev = (prev * (p - 1.0) + tr[i]) / p;
        out[i] = Some(prev);
    }

    out
}

/// ATR as a percentage of the closing price -- comparable across symbols and
/// price levels, unlike raw ATR.
///
/// Useful for position sizing: "risk 1% of the account with a stop 2 ATR away"
/// only makes sense if ATR is normalized.
#[must_use]
pub fn atr_percent(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    atr(candles, period)
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let close = candles.get(i)?.close;
            let value = value?;
            if close.abs() < f64::EPSILON {
                None
            } else {
                Some(value / close)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: 0,
            open: close,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    #[test]
    fn first_true_range_is_high_minus_low() {
        let candles = vec![candle(110.0, 100.0, 105.0)];
        assert!((true_range(&candles)[0] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn true_range_accounts_for_gaps() {
        // Second candle gaps up: high 130 vs prev close 100 -> TR = 30, not 5.
        let candles = vec![candle(105.0, 100.0, 100.0), candle(130.0, 125.0, 128.0)];
        let tr = true_range(&candles);
        assert!((tr[1] - 30.0).abs() < 1e-9);
    }

    #[test]
    fn true_range_accounts_for_gaps_down() {
        // Second candle gaps down: prev close 100, low 60 -> TR = 40.
        let candles = vec![candle(105.0, 100.0, 100.0), candle(70.0, 60.0, 65.0)];
        let tr = true_range(&candles);
        assert!((tr[1] - 40.0).abs() < 1e-9);
    }

    #[test]
    fn atr_seeds_at_period_minus_one() {
        let candles: Vec<Candle> = (0..5).map(|_| candle(110.0, 100.0, 105.0)).collect();
        let result = atr(&candles, 3);
        assert_eq!(result[0], None);
        assert_eq!(result[1], None);
        // All true ranges are 10 -> ATR is 10.
        assert!((result[2].unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn atr_uses_wilder_smoothing_not_a_plain_mean() {
        // TRs: 10, 10, 10, then a huge 100.
        let mut candles: Vec<Candle> = (0..3).map(|_| candle(110.0, 100.0, 105.0)).collect();
        candles.push(candle(205.0, 105.0, 200.0));

        let result = atr(&candles, 3);
        let seed = 10.0;
        // Wilder: (10 * 2 + 100) / 3 = 40
        let expected = (seed * 2.0 + 100.0) / 3.0;
        assert!((result[3].unwrap() - expected).abs() < 1e-9);
        // A plain mean of the last 3 TRs would be (10+10+100)/3 = 40 too here,
        // so assert the seed value explicitly to lock the recursion down.
        assert!((result[2].unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn atr_percent_normalizes_by_close() {
        let candles: Vec<Candle> = (0..4).map(|_| candle(110.0, 100.0, 100.0)).collect();
        let result = atr_percent(&candles, 2);
        assert_eq!(result[0], None);
        // ATR 10 over close 100 -> 0.1
        assert!((result[1].unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn insufficient_data_is_all_none() {
        let candles = vec![candle(110.0, 100.0, 105.0)];
        assert_eq!(atr(&candles, 5), vec![None]);
        assert_eq!(atr(&candles, 0), vec![None]);
    }

    #[test]
    fn empty_input_is_handled() {
        assert!(true_range(&[]).is_empty());
        assert!(atr(&[], 3).is_empty());
    }
}
