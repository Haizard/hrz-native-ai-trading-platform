//! The indicator tail: TSI, Awesome Oscillator, A/D, CMF, MFI, anchored VWAP,
//! the chandelier exit, and historical volatility. Canonical forms, pinned by
//! hand-computed values in the tests.

use crate::indicators::atr::atr;
use crate::indicators::ema::ema;
use crate::indicators::sma::sma;
use crate::types::Candle;

/// True Strength Index: 100 × EMA_slow(EMA_fast(m)) / EMA_slow(EMA_fast(|m|)),
/// m the 1-bar momentum, fast 13, slow 25; the signal is the 7-EMA of it.
/// (Blau, 1991.)
pub fn tsi(closes: &[f64], fast: usize, slow: usize, signal: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = closes.len();
    let mut momentum = vec![0.0; n];
    for i in 1..n {
        momentum[i] = closes[i] - closes[i - 1];
    }
    let abs_momentum: Vec<f64> = momentum.iter().map(|m| m.abs()).collect();
    let smooth = |series: &[f64]| -> Vec<f64> {
        let first = ema(series, fast);
        let first_filled: Vec<f64> = first.iter().enumerate().map(|(i, v)| v.unwrap_or(series[i])).collect();
        let second = ema(&first_filled, slow);
        let second_filled: Vec<f64> = second.iter().enumerate().map(|(i, v)| v.unwrap_or(first_filled[i])).collect();
        second_filled
    };
    let num = smooth(&momentum);
    let den = smooth(&abs_momentum);
    let mut line = vec![None; n];
    for i in 0..n {
        if i >= fast + slow - 1 && den[i] != 0.0 {
            line[i] = Some(100.0 * num[i] / den[i]);
        }
    }
    let line_filled: Vec<f64> = line.iter().map(|v| v.unwrap_or(0.0)).collect();
    let sig = ema(&line_filled, signal);
    let sig_out: Vec<Option<f64>> = sig
        .iter()
        .enumerate()
        .map(|(i, v)| if line[i].is_some() { *v } else { None })
        .collect();
    (line, sig_out)
}

/// Awesome Oscillator: SMA5 of bar midpoints minus SMA34. (Williams, 1990s.)
pub fn awesome(candles: &[Candle], fast: usize, slow: usize) -> Vec<Option<f64>> {
    let mids: Vec<f64> = candles.iter().map(|c| (c.high + c.low) / 2.0).collect();
    let f = sma(&mids, fast);
    let s = sma(&mids, slow);
    (0..candles.len())
        .map(|i| match (f[i], s[i]) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        })
        .collect()
}

/// Money-flow multiplier and volume for one bar (the A/D family's atom).
fn mfv(c: &Candle) -> f64 {
    let range = c.high - c.low;
    if range <= 0.0 {
        return 0.0;
    }
    ((c.close - c.low) - (c.high - c.close)) / range * c.volume
}

/// Accumulation/Distribution: the running sum of money-flow volume.
/// (Williams, 1966, popularized by Chaikin.)
pub fn ad_line(candles: &[Candle]) -> Vec<Option<f64>> {
    let mut out = vec![None; candles.len()];
    let mut total = 0.0;
    for (i, c) in candles.iter().enumerate() {
        total += mfv(c);
        out[i] = Some(total);
    }
    out
}

/// Chaikin Money Flow: the `period`-sum of MFV over the `period`-sum of
/// volume. (Chaikin, 1980s.)
pub fn cmf(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    for i in period - 1..n {
        let window = &candles[i + 1 - period..=i];
        let num: f64 = window.iter().map(mfv).sum();
        let den: f64 = window.iter().map(|c| c.volume).sum();
        out[i] = Some(if den > 0.0 { num / den } else { 0.0 });
    }
    out
}

/// Money Flow Index: the RSI's own ratio, computed on money flow (typical
/// price × volume) instead of price moves. 14 is the convention.
pub fn mfi(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if period == 0 || n < period + 1 {
        return out;
    }
    let tp: Vec<f64> = candles.iter().map(|c| (c.high + c.low + c.close) / 3.0).collect();
    for i in period..n {
        let mut pos = 0.0;
        let mut neg = 0.0;
        for j in i + 1 - period..=i {
            let flow = tp[j] * candles[j].volume;
            if tp[j] > tp[j - 1] {
                pos += flow;
            } else if tp[j] < tp[j - 1] {
                neg += flow;
            }
        }
        out[i] = Some(if neg > 0.0 {
            100.0 - 100.0 / (1.0 + pos / neg)
        } else {
            100.0
        });
    }
    out
}

/// Volume-weighted average price from an anchor bar index forward.
pub fn anchored_vwap(candles: &[Candle], anchor: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; candles.len()];
    let mut pv = 0.0;
    let mut v = 0.0;
    for (i, c) in candles.iter().enumerate().skip(anchor) {
        let typical = (c.high + c.low + c.close) / 3.0;
        pv += typical * c.volume;
        v += c.volume;
        out[i] = if v > 0.0 { Some(pv / v) } else { None };
    }
    out
}

/// The chandelier exit: the long stop hangs `mult` ATR under the `period`
/// highest high; the short stop hangs it over the lowest low. (Le Beau,
/// 1990s.) Returns (long_stop, short_stop).
pub fn chandelier(candles: &[Candle], period: usize, mult: f64) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = candles.len();
    let (mut long_stop, mut short_stop) = (vec![None; n], vec![None; n]);
    let ranges = atr(candles, period);
    if period == 0 || n < period {
        return (long_stop, short_stop);
    }
    for i in period - 1..n {
        let Some(a) = ranges[i] else { continue };
        let hi = candles[i + 1 - period..=i].iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let lo = candles[i + 1 - period..=i].iter().map(|c| c.low).fold(f64::MAX, f64::min);
        long_stop[i] = Some(hi - mult * a);
        short_stop[i] = Some(lo + mult * a);
    }
    (long_stop, short_stop)
}

/// Historical volatility: the annualization-free standard deviation of log
/// returns over `period`, and its percentile rank over its own trailing
/// `rank_window` readings (the regime number traders actually quote).
pub fn historical_volatility(candles: &[Candle], period: usize, rank_window: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = candles.len();
    let (mut hv, mut rank) = (vec![None; n], vec![None; n]);
    if period == 0 || n < period + 1 {
        return (hv, rank);
    }
    let mut rets = vec![0.0; n];
    for i in 1..n {
        if candles[i - 1].close > 0.0 {
            rets[i] = (candles[i].close / candles[i - 1].close).ln();
        }
    }
    for i in period..n {
        let window = &rets[i + 1 - period..=i];
        let mean = window.iter().sum::<f64>() / period as f64;
        let var = window.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / period as f64;
        hv[i] = Some(var.sqrt());
    }
    for i in 0..n {
        let Some(v) = hv[i] else { continue };
        let start = i.saturating_sub(rank_window.saturating_sub(1));
        let window: Vec<f64> = hv[start..=i].iter().flatten().copied().collect();
        if window.len() >= rank_window / 2 {
            let below = window.iter().filter(|w| **w <= v).count();
            rank[i] = Some(below as f64 / window.len() as f64);
        }
    }
    (hv, rank)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, h: f64, l: f64, c: f64, v: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: c,
            high: h,
            low: l,
            close: c,
            volume: v,
            buy_volume: v / 2.0,
            sell_volume: v / 2.0,
        }
    }

    #[test]
    fn the_tsi_is_bounded_momentum() {
        let closes: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 / 6.0).sin() * 4.0).collect();
        let (line, signal) = tsi(&closes, 13, 25, 7);
        let last = line.iter().rposition(|v| v.is_some()).expect("tsi");
        assert!((-100.0..=100.0).contains(&line[last].unwrap()), "a ratio, bounded");
        assert!(signal[last].is_some(), "the signal line exists");
    }

    #[test]
    fn the_awesome_is_two_smas_apart() {
        // Flat mids at 10: fast SMA 10, slow 10 -> zero.
        let candles: Vec<Candle> = (0..40).map(|i| k(i, 10.5, 9.5, 10.0, 1.0)).collect();
        let v = awesome(&candles, 5, 34);
        assert!(v[39].unwrap().abs() < 1e-9, "flat is zero");
        // Rising mids: fast over slow.
        let up: Vec<Candle> = (0..40).map(|i| k(i, 10.0 + i as f64 + 0.5, 10.0 + i as f64 - 0.5, 10.0 + i as f64, 1.0)).collect();
        assert!(awesome(&up, 5, 34)[39].unwrap() > 0.0);
    }

    #[test]
    fn the_ad_line_accumulates_signed_money_flow() {
        // Close at the high: multiplier +1, full volume added.
        let candles = vec![
            k(0, 10.5, 9.5, 10.5, 100.0),
            k(1, 10.5, 9.5, 9.5, 50.0), // close at the low: -1
        ];
        let v = ad_line(&candles);
        assert_eq!(v[0], Some(100.0));
        assert_eq!(v[1], Some(50.0));
    }

    #[test]
    fn the_cmf_is_the_periods_own_ratio() {
        // Every close at its high on constant volume: CMF +1.
        let candles: Vec<Candle> = (0..25).map(|i| k(i, 10.5, 9.5, 10.5, 100.0)).collect();
        let v = cmf(&candles, 20);
        assert!((v[24].unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_mfi_reads_all_up_flow_as_100() {
        let candles: Vec<Candle> = (0..20).map(|i| k(i, 10.0 + i as f64 + 0.5, 10.0 + i as f64 - 0.5, 10.0 + i as f64, 100.0)).collect();
        let v = mfi(&candles, 14);
        assert_eq!(v[19], Some(100.0), "no negative flow is 100");
    }

    #[test]
    fn the_anchored_vwap_starts_at_its_anchor() {
        let candles: Vec<Candle> = (0..10).map(|i| k(i, 11.0, 9.0, 10.0, 100.0)).collect();
        let v = anchored_vwap(&candles, 5);
        assert!(v[4].is_none(), "nothing before the anchor");
        assert_eq!(v[5], Some(10.0), "the anchor bar's own typical price");
        assert_eq!(v[9], Some(10.0), "constant prices hold the average");
    }

    #[test]
    fn the_chandelier_hangs_off_the_extremes() {
        let candles: Vec<Candle> = (0..25).map(|i| k(i, 11.0, 9.0, 10.0, 1.0)).collect();
        let (long_stop, short_stop) = chandelier(&candles, 22, 3.0);
        let (l, s) = (long_stop[24].unwrap(), short_stop[24].unwrap());
        assert!(l < 11.0 && s > 9.0, "the stops hang off the window's extremes");
        assert!((s - 9.0) > 0.0 && (11.0 - l) > 0.0);
    }

    #[test]
    fn the_hv_rank_orders_itself() {
        // Calm then violent: the rank of the violent tail sits high, the
        // calm early readings low.
        let mut candles: Vec<Candle> = (0..60).map(|i| k(i, 100.2, 99.8, 100.0, 1.0)).collect();
        for i in 0..20 {
            let c = 100.0 + if i % 2 == 0 { 3.0 } else { -3.0 };
            candles.push(k(60 + i as i64, c + 1.0, c - 1.0, c, 1.0));
        }
        let (hv, rank) = historical_volatility(&candles, 20, 30);
        let calm = hv[30].expect("hv");
        let wild = hv[79].expect("hv");
        assert!(wild > calm * 5.0, "the violent stretch reads volatile: {calm} vs {wild}");
        assert!(rank[79].unwrap() > 0.9, "and its rank sits at the top: {}", rank[79].unwrap());
    }
}
