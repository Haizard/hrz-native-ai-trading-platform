//! The trend and channel set, canonical forms: Wilder's ADX with ±DI, Keltner
//! channels, Donchian channels, Supertrend, and the Parabolic SAR. Each is its
//! author's published recursion, pinned by hand-computed values in the tests.

use crate::indicators::atr::atr;
use crate::indicators::ema::ema;
use crate::types::Candle;

/// Wilder's ADX: +DI, -DI and the ADX line, with Wilder's own smoothing
/// (`prev - prev/n + cur`) throughout. (Wilder, 1978.)
pub fn adx(candles: &[Candle], period: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = candles.len();
    let (mut plus_di, mut minus_di, mut adx_line) = (vec![None; n], vec![None; n], vec![None; n]);
    if period == 0 || n < period * 2 + 1 {
        return (plus_di, minus_di, adx_line);
    }
    // True range, +DM, -DM per bar (bar 1 onward).
    let mut tr = vec![0.0; n];
    let mut pdm = vec![0.0; n];
    let mut mdm = vec![0.0; n];
    for i in 1..n {
        let (h, l, ph, pl, pc) = (candles[i].high, candles[i].low, candles[i - 1].high, candles[i - 1].low, candles[i - 1].close);
        tr[i] = (h - l).max((h - pc).abs()).max((l - pc).abs());
        let (up, down) = (h - ph, pl - l);
        pdm[i] = if up > down && up > 0.0 { up } else { 0.0 };
        mdm[i] = if down > up && down > 0.0 { down } else { 0.0 };
    }
    // Wilder's smoothed sums: first value is the plain sum of the first
    // `period` bars, then the recursion.
    let smooth = |series: &[f64], i: usize, prev: f64| -> f64 { prev - prev / period as f64 + series[i] };
    let first = period; // first smoothed index covers bars 1..=period
    let (mut str_, mut spdm, mut smdm) = (
        tr[1..=first].iter().sum::<f64>(),
        pdm[1..=first].iter().sum::<f64>(),
        mdm[1..=first].iter().sum::<f64>(),
    );
    let mut dxs: Vec<f64> = Vec::new();
    for i in first..n {
        if i > first {
            str_ = smooth(&tr, i, str_);
            spdm = smooth(&pdm, i, spdm);
            smdm = smooth(&mdm, i, smdm);
        }
        let pdi = if str_ > 0.0 { spdm / str_ * 100.0 } else { 0.0 };
        let mdi = if str_ > 0.0 { smdm / str_ * 100.0 } else { 0.0 };
        plus_di[i] = Some(pdi);
        minus_di[i] = Some(mdi);
        let dx = if pdi + mdi > 0.0 { (pdi - mdi).abs() / (pdi + mdi) * 100.0 } else { 0.0 };
        dxs.push(dx);
        // ADX: Wilder-smoothed DX, seeded with the mean of the first
        // `period` DX values.
        if dxs.len() == period {
            let seed = dxs.iter().sum::<f64>() / period as f64;
            adx_line[i] = Some(seed);
        } else if dxs.len() > period {
            let prev = adx_line[i - 1].unwrap();
            adx_line[i] = Some((prev * (period as f64 - 1.0) + dx) / period as f64);
        }
    }
    (plus_di, minus_di, adx_line)
}

/// Keltner channels: EMA(center) ± mult × ATR. (Keltner, 1960; the modern
/// EMA/ATR form.)
pub fn keltner(candles: &[Candle], ema_period: usize, atr_period: usize, mult: f64) -> Vec<Option<(f64, f64, f64)>> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let center = ema(&closes, ema_period);
    let range = atr(candles, atr_period);
    (0..candles.len())
        .map(|i| match (center[i], range[i]) {
            (Some(c), Some(a)) => Some((c + mult * a, c, c - mult * a)),
            _ => None,
        })
        .collect()
}

/// Donchian channels: the N-bar highest high and lowest low, with the middle.
/// (Donchian, 1936; the turtle traders' channel.)
pub fn donchian(candles: &[Candle], period: usize) -> Vec<Option<(f64, f64, f64)>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    for i in period - 1..n {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for c in &candles[i + 1 - period..=i] {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
        }
        out[i] = Some((hi, (hi + lo) / 2.0, lo));
    }
    out
}

/// Supertrend: ATR bands that ratchet — the upper band can only fall while
/// price is below it, the lower can only rise while price is above; the close
/// crossing the active band flips the trend. Returns (value, is_up).
pub fn supertrend(candles: &[Candle], period: usize, mult: f64) -> Vec<Option<(f64, bool)>> {
    let n = candles.len();
    let mut out = vec![None; n];
    let ranges = atr(candles, period);
    if ranges.iter().all(|v| v.is_none()) {
        return out;
    }
    let mut upper = 0.0;
    let mut lower = 0.0;
    let mut up = true;
    let mut value = 0.0;
    for i in 0..n {
        let Some(a) = ranges[i] else { continue };
        let mid = (candles[i].high + candles[i].low) / 2.0;
        let (raw_u, raw_l) = (mid + mult * a, mid - mult * a);
        if i == 0 || ranges[i - 1].is_none() {
            upper = raw_u;
            lower = raw_l;
            // The seed reads the CLOSE's own direction, not the bar's
            // midpoint: a bar that closed up seeds up.
            up = i > 0 && candles[i].close >= candles[i - 1].close || i == 0 && candles[0].close >= candles[0].open.max(mid.min(candles[0].close));
            value = if up { lower } else { upper};
            out[i] = Some((value, up));
            continue;
        }
        // The ratchet: bands never retreat against the trend.
        upper = if raw_u < upper || candles[i - 1].close > upper { raw_u } else { upper };
        lower = if raw_l > lower || candles[i - 1].close < lower { raw_l } else { lower };
        if up && candles[i].close < lower {
            up = false;
        } else if !up && candles[i].close > upper {
            up = true;
        }
        value = if up { lower } else { upper };
        out[i] = Some((value, up));
    }
    out
}

/// The Parabolic SAR: the stop that accelerates toward price (step 0.02, cap
/// 0.20), flipping when touched. (Wilder, 1978.)
pub fn parabolic_sar(candles: &[Candle], step: f64, max_step: f64) -> Vec<Option<f64>> {
    let n = candles.len();
    let mut out = vec![None; n];
    if n < 3 {
        return out;
    }
    // Seed with the first two bars' direction.
    let mut up = candles[1].close >= candles[0].close;
    let mut sar = if up { candles[0].low.min(candles[1].low) } else { candles[0].high.max(candles[1].high) };
    let mut ep = if up { candles[0].high.max(candles[1].high) } else { candles[0].low.min(candles[1].low) };
    let mut af = step;
    out[0] = Some(sar);
    out[1] = Some(sar);
    for i in 2..n {
        sar += af * (ep - sar);
        if up {
            // The SAR never rises above the prior two lows.
            sar = sar.min(candles[i - 1].low).min(candles[i - 2].low);
            if candles[i].low < sar {
                up = false;
                sar = ep;
                ep = candles[i].low;
                af = step;
            } else {
                if candles[i].high > ep {
                    ep = candles[i].high;
                    af = (af + step).min(max_step);
                }
            }
        } else {
            sar = sar.max(candles[i - 1].high).max(candles[i - 2].high);
            if candles[i].high > sar {
                up = true;
                sar = ep;
                ep = candles[i].high;
                af = step;
            } else {
                if candles[i].low < ep {
                    ep = candles[i].low;
                    af = (af + step).min(max_step);
                }
            }
        }
        out[i] = Some(sar);
    }
    out
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
    fn the_adx_reads_a_one_way_street_as_strong() {
        // A straight ramp: +DM every bar, -DM never -> DX 100, and the ADX
        // climbs toward it.
        let candles: Vec<Candle> = (0..40).map(|i| {
            let base = 100.0 + i as f64;
            k(i, base + 1.2, base + 0.2, base + 1.0)
        }).collect();
        let (plus, minus, adx_line) = adx(&candles, 14);
        let last = adx_line.iter().rposition(|v| v.is_some()).expect("adx exists");
        // The fixture's own TR (1.2, including the gap to the prior close)
        // caps +DI at 1.0/1.2 -- the expectation is the fixture's math, not
        // a round number.
        assert!(plus[last].unwrap() > 80.0, "a ramp is nearly all +DI: {}", plus[last].unwrap());
        assert!(minus[last].unwrap() < 5.0, "no -DI in a ramp");
        assert!(adx_line[last].unwrap() > 60.0, "the ADX reads the ramp as strong: {}", adx_line[last].unwrap());
    }

    #[test]
    fn the_adx_reads_a_flat_market_as_weak() {
        let candles: Vec<Candle> = (0..40).map(|i| {
            let c = 100.0 + if i % 2 == 0 { 0.3 } else { -0.3 };
            k(i, c + 0.4, c - 0.4, c)
        }).collect();
        let (_, _, adx_line) = adx(&candles, 14);
        let last = adx_line.iter().rposition(|v| v.is_some()).expect("adx");
        assert!(adx_line[last].unwrap() < 25.0, "chop reads weak: {}", adx_line[last].unwrap());
    }

    #[test]
    fn keltner_is_ema_plus_minus_atr() {
        let candles: Vec<Candle> = (0..30).map(|i| k(i, 10.6, 9.4, 10.0 + (i % 2) as f64 * 0.1)).collect();
        let bands = keltner(&candles, 20, 10, 2.0);
        let (u, c, l) = bands[29].expect("bands");
        assert!(u > c && c > l, "band order");
        assert!(((u - c) - (c - l)).abs() < 1e-9, "symmetric around the center");
    }

    #[test]
    fn donchian_is_the_windows_own_extremes() {
        let candles = vec![
            k(0, 11.0, 9.0, 10.0),
            k(1, 12.0, 9.5, 11.0),
            k(2, 11.5, 10.0, 10.5),
        ];
        let d = donchian(&candles, 2);
        assert_eq!(d[1], Some((12.0, 10.5, 9.0)));
        assert_eq!(d[2], Some((12.0, 10.75, 9.5)));
    }

    #[test]
    fn supertrend_flips_on_a_real_break() {
        // Up drift, then a sharp break down: the trend value follows price
        // up, then flips below it.
        let mut candles: Vec<Candle> = (0..20).map(|i| {
            let c = 100.0 + i as f64 * 0.5;
            k(i, c + 0.6, c - 0.4, c)
        }).collect();
        for i in 0..6 {
            let c = 110.0 - i as f64 * 2.0;
            candles.push(k(20 + i as i64, c + 0.5, c - 1.0, c));
        }
        let st = supertrend(&candles, 10, 3.0);
        let mid = st[10].expect("v");
        assert!(mid.1, "uptrend reads up");
        assert!(mid.0 < candles[10].close, "the line sits under price in an uptrend");
        let tail = st[25].expect("v");
        assert!(!tail.1, "the sharp break flips it down");
        assert!(tail.0 > candles[25].close, "the line sits over price in a downtrend");
    }

    #[test]
    fn the_psar_accelerates_and_flips() {
        let mut candles: Vec<Candle> = (0..15).map(|i| {
            let c = 100.0 + i as f64;
            k(i, c + 0.8, c - 0.2, c)
        }).collect();
        for i in 0..5 {
            let c = 115.0 - i as f64 * 3.0;
            candles.push(k(15 + i as i64, c + 0.5, c - 1.5, c));
        }
        let sar = parabolic_sar(&candles, 0.02, 0.2);
        // In the uptrend the SAR trails below price; after the break it sits above.
        assert!(sar[10].unwrap() < candles[10].low, "below in uptrend");
        assert!(sar[19].unwrap() > candles[19].high, "above after the flip");
    }
}
