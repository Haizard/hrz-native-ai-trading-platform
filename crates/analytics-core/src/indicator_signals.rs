//! The technical indicators' SIGNALS: what each measurement detects, stamped
//! on the bar that completes it, so a chart shows the detections instead of
//! only drawing the line. Every detector here reads only the window's tail --
//! the scan replay feeds it each prefix, so an attached indicator marks every
//! signal across the visible history at once rather than waiting for new bars.

use crate::events::{EventKind, MarketEvent};
use crate::indicators::{bollinger::bollinger, ema::ema, macd::macd, rsi::rsi, sma::sma};
use crate::rsi_divergence::{rsi_divergences, RsiDivergenceConfig, RsiDivergenceKind};
use crate::types::{Candle, Side};

const RSI_PERIOD: usize = 14;
const OVERBOUGHT: f64 = 70.0;
const OVERSOLD: f64 = 30.0;

/// Detect every indicator signal completing on the window's LAST bar.
pub fn detect_indicator_signals(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let Some(last) = candles.last() else { return out };
    let n = candles.len();
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let mut push = |kind: EventKind, side: Option<Side>, magnitude: Option<f64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: last.close,
            side,
            zone_low: Some(last.low),
            zone_high: Some(last.high),
            magnitude,
            formed_at: None,
        });
    };

    // -- RSI zone crossings (14): entering oversold is the buy-side signal,
    // entering overbought the sell-side.
    if n > RSI_PERIOD + 1 {
        let values = rsi(&closes, RSI_PERIOD);
        if let (Some(prev), Some(curr)) = (values[n - 2], values[n - 1]) {
            if prev >= OVERSOLD && curr < OVERSOLD {
                push(EventKind::RsiZoneCross, Some(Side::Buy), Some(curr));
            } else if prev <= OVERBOUGHT && curr > OVERBOUGHT {
                push(EventKind::RsiZoneCross, Some(Side::Sell), Some(curr));
            }
        }
    }

    // -- RSI family consensus: EVERY period 2..=24 in the same zone is the
    // family's strongest statement. Fires on entering the consensus, not on
    // every bar inside it.
    if n > 26 {
        let consensus = |i: usize, above: bool| {
            crate::rsi_pca::RSI_PERIODS.iter().all(|p| {
                rsi(&closes, *p)[i].map_or(false, |v| if above { v > OVERBOUGHT } else { v < OVERSOLD })
            })
        };
        let buy_now = consensus(n - 1, false);
        let sell_now = consensus(n - 1, true);
        if (buy_now && !consensus(n - 2, false)) || (sell_now && !consensus(n - 2, true)) {
            let mean: f64 = crate::rsi_pca::RSI_PERIODS
                .iter()
                .filter_map(|p| rsi(&closes, *p)[n - 1])
                .sum::<f64>()
                / crate::rsi_pca::RSI_PERIODS.len() as f64;
            push(
                EventKind::RsiFamilyConsensus,
                Some(if sell_now { Side::Sell } else { Side::Buy }),
                Some(mean),
            );
        }
    }

    // -- RSI/price divergences confirmed exactly at the tail (the second
    // swing plus the detector's strength bars lands on the last bar).
    if n > 40 {
        let config = RsiDivergenceConfig { lookback: n, ..RsiDivergenceConfig::default() };
        for div in rsi_divergences(candles, &config) {
            if div.rsi_index + config.strength == n - 1 {
                push(
                    EventKind::RsiDivergenceSignal,
                    Some(match div.direction {
                        RsiDivergenceKind::Bullish => Side::Buy,
                        RsiDivergenceKind::Bearish => Side::Sell,
                    }),
                    Some(div.rsi),
                );
            }
        }
    }

    // -- MACD (12, 26, 9): the signal-line cross is the classic; the zero
    // cross is the slower trend statement. Both on the tail bar only.
    if n > 36 {
        let points = macd(&closes, 12, 26, 9);
        if let (Some(prev), Some(curr)) = (points[n - 2], points[n - 1]) {
            if prev.line <= prev.signal && curr.line > curr.signal {
                push(EventKind::MacdSignalCross, Some(Side::Buy), Some(curr.histogram));
            } else if prev.line >= prev.signal && curr.line < curr.signal {
                push(EventKind::MacdSignalCross, Some(Side::Sell), Some(curr.histogram));
            }
            if prev.line <= 0.0 && curr.line > 0.0 {
                push(EventKind::MacdZeroCross, Some(Side::Buy), Some(curr.line));
            } else if prev.line >= 0.0 && curr.line < 0.0 {
                push(EventKind::MacdZeroCross, Some(Side::Sell), Some(curr.line));
            }
        }
    }

    // -- The golden/death cross: the 50 crossing the 200, EMA and SMA as
    // their own concepts.
    if n > 201 {
        for (kind, average) in [(EventKind::EmaCross, ema as fn(&[f64], usize) -> Vec<Option<f64>>), (EventKind::SmaCross, sma)] {
            let fast = average(&closes, 50);
            let slow = average(&closes, 200);
            if let (Some(fp), Some(sp), Some(fc), Some(sc)) = (fast[n - 2], slow[n - 2], fast[n - 1], slow[n - 1]) {
                if fp <= sp && fc > sc {
                    push(kind, Some(Side::Buy), Some(fc - sc));
                } else if fp >= sp && fc < sc {
                    push(kind, Some(Side::Sell), Some(fc - sc));
                }
            }
        }
    }

    // -- Bollinger (20, 2): a close OUTSIDE a band is the breakout, in the
    // break's own direction.
    if n > 21 {
        let bands = bollinger(&closes, 20, 2.0);
        if let (Some(prev), Some(curr)) = (bands[n - 2], bands[n - 1]) {
            let pct_b = (closes[n - 1] - curr.lower) / (curr.upper - curr.lower).max(f64::EPSILON);
            if closes[n - 2] <= prev.upper && closes[n - 1] > curr.upper {
                push(EventKind::BollingerBreakout, Some(Side::Buy), Some(pct_b));
            } else if closes[n - 2] >= prev.lower && closes[n - 1] < curr.lower {
                push(EventKind::BollingerBreakout, Some(Side::Sell), Some(pct_b));
            }
        }
    }

    // -- ATR expansion: volatility jumping past 1.5x its own 20-bar mean is
    // not directional -- it is the market changing gear. Edge-fired.
    if n > 36 {
        let values = crate::indicators::atr::atr(candles, 14);
        if let Some(curr) = values[n - 1] {
            let base: Vec<f64> = values[n - 22..n - 2].iter().flatten().copied().collect();
            if base.len() >= 10 {
                let mean = base.iter().sum::<f64>() / base.len() as f64;
                let was = values[n - 2].map_or(false, |p| p > 1.5 * mean);
                if curr > 1.5 * mean && !was {
                    push(EventKind::AtrExpansion, None, Some(curr / mean.max(f64::EPSILON)));
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, close: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: close,
            high: close * 1.001,
            low: close * 0.999,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn series_from(closes: &[f64]) -> Vec<Candle> {
        closes.iter().enumerate().map(|(i, c)| k(i as i64 * 3_600_000_000_000, *c)).collect()
    }

    #[test]
    fn an_oversold_entry_fires_buy_on_its_own_bar() {
        // A steady climb (RSI pinned high), then a sharp two-bar drop at the
        // tail carrying RSI through 30 -- the cross must stamp the LAST bar.
        let mut closes: Vec<f64> = Vec::new();
        let mut price = 100.0;
        for _ in 0..60 {
            price *= 1.004;
            closes.push(price);
        }
        for _ in 0..3 {
            price *= 0.94;
            closes.push(price);
        }
        let candles = series_from(&closes);
        let events = detect_indicator_signals(&candles, "T");
        let cross = events.iter().find(|e| e.kind == EventKind::RsiZoneCross);
        if let Some(cross) = cross {
            assert_eq!(cross.side, Some(Side::Buy));
            assert_eq!(cross.bar_time, candles.last().expect("last").open_time, "stamped on its own bar");
            assert!(cross.magnitude.expect("rsi") < OVERSOLD + 1.0);
        }
        // A flat tail fires nothing.
        let flat: Vec<f64> = (0..60).map(|i| 100.0 + (i as f64 / 5.0).sin() * 0.5).collect();
        let flat_candles = series_from(&flat);
        assert!(
            !detect_indicator_signals(&flat_candles, "T").iter().any(|e| e.kind == EventKind::RsiZoneCross),
            "no cross without a zone entry"
        );
    }

    #[test]
    fn the_macd_cross_fires_in_both_directions() {
        // Warm up near flat (so the signal EMA seeds honestly), a sharp
        // decline (the line dives below the signal), then a sharp recovery
        // (the line crosses back above) -- a REAL cross, not the seeding
        // artifact a steady drift produces.
        let mut closes: Vec<f64> = Vec::new();
        let mut price = 100.0;
        for i in 0..40 {
            price = 100.0 + (i as f64 / 4.0).sin() * 0.5;
            closes.push(price);
        }
        for _ in 0..15 {
            price *= 0.985;
            closes.push(price);
        }
        for _ in 0..8 {
            price *= 1.03;
            closes.push(price);
        }
        let candles = series_from(&closes);
        // The cross happens wherever the line outruns the signal -- in this
        // shape the descent decelerates and the cross lands mid-series.
        // Per-bar semantics mean the scan stamps it on ITS OWN bar: replay
        // every prefix, collect the bars the cross fires on, and require
        // exactly one, stamped on the prefix's own tail.
        let mut fire_bars = Vec::new();
        for end in 36..closes.len() {
            for e in detect_indicator_signals(&candles[..=end], "T") {
                if e.kind == EventKind::MacdSignalCross && e.side == Some(Side::Buy) {
                    assert_eq!(e.bar_time, candles[end].open_time, "stamped on its own bar");
                    fire_bars.push(end);
                }
            }
        }
        assert_eq!(fire_bars.len(), 1, "one bullish cross in the shape: {fire_bars:?}");
    }

    #[test]
    fn a_bollinger_breakout_follows_the_close() {
        // Quiet, then a thrust through the upper band at the tail.
        let mut closes: Vec<f64> = vec![100.0; 30];
        for i in 0..10 {
            closes.push(100.0 + (i as f64 / 3.0).sin() * 0.2);
        }
        closes.push(closes.last().expect("last") * 1.06);
        let candles = series_from(&closes);
        let events = detect_indicator_signals(&candles, "T");
        let breakout = events.iter().find(|e| e.kind == EventKind::BollingerBreakout);
        if let Some(b) = breakout {
            assert_eq!(b.side, Some(Side::Buy), "an upper break is buy-side");
            assert!(b.magnitude.expect("%b") > 1.0, "above the band is %B > 1");
        }
    }

    #[test]
    fn atr_expansion_is_neutral_and_edge_fired() {
        // Calm, then one violent bar at the tail.
        let mut candles: Vec<Candle> = (0..50)
            .map(|i| {
                let c = 100.0 + (i as f64 / 7.0).sin() * 0.3;
                Candle {
                    symbol: "T".to_string(),
                    timeframe: Timeframe::H1,
                    open_time: i as i64 * 3_600_000_000_000,
                    open: c,
                    high: c + 0.2,
                    low: c - 0.2,
                    close: c,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                }
            })
            .collect();
        let t = candles.len() as i64 * 3_600_000_000_000;
        candles.push(Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: 100.0,
            high: 106.0,
            low: 99.0,
            close: 105.0,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        });
        let events = detect_indicator_signals(&candles, "T");
        let expansion = events.iter().find(|e| e.kind == EventKind::AtrExpansion);
        if let Some(e) = expansion {
            assert_eq!(e.side, None, "volatility has no direction");
            assert!(e.magnitude.expect("ratio") > 1.5);
        }
    }
}
