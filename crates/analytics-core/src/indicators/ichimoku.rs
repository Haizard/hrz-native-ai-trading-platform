//! The Ichimoku cloud (Goichi Hosoda, 1969): the five lines and the kumo.
//! The projected lines are stored SHIFTED -- the value plotted at bar `i`
//! was computed `projection` bars back, which is exactly how the cloud
//! appears on a chart (and keeps every array candle-length).

use crate::types::Candle;

/// The five lines: (tenkan, kijun, senkou_a, senkou_b, chikou).
pub struct Ichimoku {
    /// The conversion line (9-period midpoint).
    pub tenkan: Vec<Option<f64>>,
    /// The base line (26-period midpoint).
    pub kijun: Vec<Option<f64>>,
    /// Span A: tenkan/kijun midpoint, plotted `projection` bars forward.
    pub senkou_a: Vec<Option<f64>>,
    /// Span B: the 52-period midpoint, plotted `projection` bars forward.
    pub senkou_b: Vec<Option<f64>>,
    /// The close, plotted `projection` bars BACKWARD (value at i is close[i+p]).
    pub chikou: Vec<Option<f64>>,
}

/// The period midpoint: (highest high + lowest low) / 2.
fn midpoint(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
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
        out[i] = Some((hi + lo) / 2.0);
    }
    out
}

/// The classic (9, 26, 52) with the 26-bar projection.
pub fn ichimoku(candles: &[Candle], tenkan_p: usize, kijun_p: usize, senkou_b_p: usize, projection: usize) -> Ichimoku {
    let n = candles.len();
    let tenkan = midpoint(candles, tenkan_p);
    let kijun = midpoint(candles, kijun_p);
    let span_b_raw = midpoint(candles, senkou_b_p);
    // The spans are plotted forward: value at i = computed at i - projection.
    let shift = |series: &Vec<Option<f64>>| -> Vec<Option<f64>> {
        let mut out = vec![None; n];
        for i in projection..n {
            out[i] = series[i - projection];
        }
        out
    };
    let senkou_a_raw: Vec<Option<f64>> = (0..n)
        .map(|i| match (tenkan[i], kijun[i]) {
            (Some(t), Some(k)) => Some((t + k) / 2.0),
            _ => None,
        })
        .collect();
    let senkou_a = shift(&senkou_a_raw);
    let senkou_b = shift(&span_b_raw);
    // The chikou plotted back: value at i = close at i + projection.
    let mut chikou = vec![None; n];
    for i in 0..n.saturating_sub(projection) {
        chikou[i] = Some(candles[i + projection].close);
    }
    Ichimoku {
        tenkan,
        kijun,
        senkou_a,
        senkou_b,
        chikou,
    }
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
    fn the_lines_are_the_periods_own_midpoints_shifted() {
        // 60 bars of a steady ramp.
        let candles: Vec<Candle> = (0..60).map(|i| k(i, 100.0 + i as f64 + 1.0, 100.0 + i as f64 - 1.0, 100.0 + i as f64)).collect();
        let ic = ichimoku(&candles, 9, 26, 52, 26);
        // Tenkan(9) at bar 30: mid of bars 22..30 highs/lows: hi = 131, lo = 121 -> 126.
        let t = ic.tenkan[30].expect("tenkan");
        assert!((t - 126.0).abs() < 1e-9, "the 9-bar midpoint: {t}");
        // Senkou A at bar 30 = (tenkan[4] + kijun[4]) / 2 -- the shift IS
        // the projection (bar 4 has no kijun yet -> None).
        assert!(ic.senkou_a[30].is_none(), "span A needs a kijun 26 bars back");
        // Chikou at 30 = close at 56.
        assert_eq!(ic.chikou[30], Some(candles[56].close));
        // Senkou B's warmup is 52 + 26 = 78 bars: at 59 it is honestly
        // absent (the shift of a value that does not exist yet is None).
        assert!(ic.senkou_b[59].is_none(), "span B warms at 78 bars, not before");
    }
}
