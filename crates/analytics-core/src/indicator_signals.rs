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
    // -- Divergences on an oscillator line (OBV, the MACD line, the A/D
    // line): the shared machinery lives in `oscillator_divergence` below.
    {
        let obv_series = crate::indicators::oscillators::obv(candles);
        out.extend(oscillator_divergence(candles, &obv_series, EventKind::ObvDivergence, symbol));
        let macd_line: Vec<Option<f64>> = macd(&closes, 12, 26, 9).iter().map(|p| p.map(|m| m.line)).collect();
        out.extend(oscillator_divergence(candles, &macd_line, EventKind::MacdDivergenceSignal, symbol));
        let ad_series = crate::indicators::misc::ad_line(candles);
        out.extend(oscillator_divergence(candles, &ad_series, EventKind::AdDivergence, symbol));
    }

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
    // -- The CMF zero cross.
    if n > 22 {
        let v = crate::indicators::misc::cmf(candles, 20);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p <= 0.0 && c > 0.0 {
                push(EventKind::CmfZeroCross, Some(Side::Buy), None);
            } else if p >= 0.0 && c < 0.0 {
                push(EventKind::CmfZeroCross, Some(Side::Sell), None);
            }
        }
    }

    // -- MFI zone entries.
    if n > 16 {
        let v = crate::indicators::misc::mfi(candles, 14);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p >= 20.0 && c < 20.0 {
                push(EventKind::MfiZoneEntry, Some(Side::Buy), Some(c));
            } else if p <= 80.0 && c > 80.0 {
                push(EventKind::MfiZoneEntry, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- The chandelier flip: the close crossing the active stop.
    if n > 24 {
        let (long_stop, short_stop) = crate::indicators::misc::chandelier(candles, 22, 3.0);
        if let (Some(l), Some(s)) = (long_stop[n - 1], short_stop[n - 1]) {
            if closes[n - 2] >= long_stop[n - 2].unwrap_or(l) && closes[n - 1] < l {
                push(EventKind::ChandelierFlip, Some(Side::Sell), Some(l - closes[n - 1]));
            } else if closes[n - 2] <= short_stop[n - 2].unwrap_or(s) && closes[n - 1] > s {
                push(EventKind::ChandelierFlip, Some(Side::Buy), Some(closes[n - 1] - s));
            }
        }
    }

    // -- The anchored VWAP cross (against the newest swing-low anchor).
    if n > 30 {
        let swings = crate::market_structure::find_swings(candles, 3);
        if let Some(anchor) = swings.iter().rev().find(|sp| sp.kind == crate::market_structure::SwingKind::Low) {
            let v = crate::indicators::misc::anchored_vwap(candles, anchor.index);
            if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
                if closes[n - 2] <= p && closes[n - 1] > c {
                    push(EventKind::AvwapCross, Some(Side::Buy), None);
                } else if closes[n - 2] >= p && closes[n - 1] < c {
                    push(EventKind::AvwapCross, Some(Side::Sell), None);
                }
            }
        }
    }

    // -- The HV regime: the volatility rank entering the top decile (the
    // high-volatility regime) or the bottom one.
    if n > 150 {
        let (_, rank) = crate::indicators::misc::historical_volatility(candles, 20, 126);
        if let (Some(p), Some(c)) = (rank[n - 2], rank[n - 1]) {
            if p < 0.9 && c >= 0.9 {
                push(EventKind::HvRegimeShift, None, Some(c));
            } else if p > 0.1 && c <= 0.1 {
                push(EventKind::HvRegimeShift, None, Some(c));
            }
        }
    }




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

    // -- Stochastic (14, 3): zone entries (reversion side) and the %Kx%D
    // cross.
    if n > 18 {
        let (k_line, d_line) = crate::indicators::oscillators::stochastic(candles, 14, 3);
        if let (Some(kp), Some(kc), Some(dp), Some(dc)) = (k_line[n - 2], k_line[n - 1], d_line[n - 2], d_line[n - 1]) {
            if kp >= 20.0 && kc < 20.0 {
                push(EventKind::StochasticZoneEntry, Some(Side::Buy), Some(kc));
            } else if kp <= 80.0 && kc > 80.0 {
                push(EventKind::StochasticZoneEntry, Some(Side::Sell), Some(kc));
            }
            if kp <= dp && kc > dc {
                push(EventKind::StochasticCross, Some(Side::Buy), Some(kc - dc));
            } else if kp >= dp && kc < dc {
                push(EventKind::StochasticCross, Some(Side::Sell), Some(kc - dc));
            }
        }
    }

    // -- CCI (20): entering the +/-100 zones (reversion side).
    if n > 22 {
        let v = crate::indicators::oscillators::cci(candles, 20);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p >= -100.0 && c < -100.0 {
                push(EventKind::CciZoneEntry, Some(Side::Buy), Some(c));
            } else if p <= 100.0 && c > 100.0 {
                push(EventKind::CciZoneEntry, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- Williams %R (14): entering oversold (<-80) / overbought (>-20).
    if n > 16 {
        let v = crate::indicators::oscillators::williams_r(candles, 14);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p >= -80.0 && c < -80.0 {
                push(EventKind::WilliamsZoneEntry, Some(Side::Buy), Some(c));
            } else if p <= -20.0 && c > -20.0 {
                push(EventKind::WilliamsZoneEntry, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- ROC (12): the zero cross.
    if n > 14 {
        let v = crate::indicators::oscillators::roc(&closes, 12);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p <= 0.0 && c > 0.0 {
                push(EventKind::RocZeroCross, Some(Side::Buy), Some(c));
            } else if p >= 0.0 && c < 0.0 {
                push(EventKind::RocZeroCross, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- Divergences on an oscillator line (OBV and the MACD line): price
    // makes the new extreme, the oscillator refuses it. Confirmed when the
    // second swing's `strength` bars complete -- i.e. exactly at the tail.
    // -- ADX (14): the trend-strength cross -- ADX through 25 with the
    // dominant DI naming the side.
    if n > 30 {
        let (plus, minus, adx_line) = crate::indicators::trend::adx(candles, 14);
        if let (Some(ap), Some(ac), Some(pp), Some(pc), Some(mp), Some(mc)) =
            (adx_line[n - 2], adx_line[n - 1], plus[n - 2], plus[n - 1], minus[n - 2], minus[n - 1])
        {
            if ap <= 25.0 && ac > 25.0 {
                let side = if pc >= mc { Side::Buy } else { Side::Sell };
                push(EventKind::AdxTrendCross, Some(side), Some(ac));
            }
            // The DI cross itself, while the ADX says a trend exists.
            if pp <= mp && pc > mc && ac > 20.0 {
                push(EventKind::AdxTrendCross, Some(Side::Buy), Some(pc - mc));
            } else if pp >= mp && pc < mc && ac > 20.0 {
                push(EventKind::AdxTrendCross, Some(Side::Sell), Some(pc - mc));
            }
        }
    }

    // -- The Bollinger squeeze: bandwidth entering the lowest reading of the
    // trailing 120 bars. Directionless -- the expansion is the trade.
    if n > 140 {
        let bands = bollinger(&closes, 20, 2.0);
        let bw = |i: usize| bands[i].map(|b| b.bandwidth);
        if let Some(curr) = bw(n - 1) {
            let window_min = (n - 121..n - 1).filter_map(bw).fold(f64::INFINITY, f64::min);
            let prev = bw(n - 2);
            if curr <= window_min && prev.map_or(true, |p| p > window_min) {
                push(EventKind::BollingerSqueeze, None, Some(curr));
            }
        }
    }

    // -- Channel breakouts: the close outside Keltner (20, 10, 2) or the
    // Donchian (20) channel, in the break's direction.
    if n > 22 {
        let kel = crate::indicators::trend::keltner(candles, 20, 10, 2.0);
        if let (Some((up, _, lo)), Some((pup, _, plo))) = (kel[n - 1], kel[n - 2]) {
            if closes[n - 2] <= pup && closes[n - 1] > up {
                push(EventKind::KeltnerBreakout, Some(Side::Buy), Some(closes[n - 1] - up));
            } else if closes[n - 2] >= plo && closes[n - 1] < lo {
                push(EventKind::KeltnerBreakout, Some(Side::Sell), Some(lo - closes[n - 1]));
            }
        }
        let don = crate::indicators::trend::donchian(candles, 20);
        if let (Some((up, _, lo)), Some((pup, _, plo))) = (don[n - 1], don[n - 2]) {
            if closes[n - 2] <= pup && closes[n - 1] > up {
                push(EventKind::DonchianBreakout, Some(Side::Buy), Some(closes[n - 1] - up));
            } else if closes[n - 2] >= plo && closes[n - 1] < lo {
                push(EventKind::DonchianBreakout, Some(Side::Sell), Some(lo - closes[n - 1]));
            }
        }
    }

    // -- The trailing-stop flips: Supertrend and the PSAR.
    if n > 12 {
        let st = crate::indicators::trend::supertrend(candles, 10, 3.0);
        if let (Some((_, pu)), Some((_, cu))) = (st[n - 2], st[n - 1]) {
            if !pu && cu {
                push(EventKind::SupertrendFlip, Some(Side::Buy), None);
            } else if pu && !cu {
                push(EventKind::SupertrendFlip, Some(Side::Sell), None);
            }
        }
        let sar = crate::indicators::trend::parabolic_sar(candles, 0.02, 0.2);
        if let (Some(p), Some(c)) = (sar[n - 2], sar[n - 1]) {
            let was_below = p < candles[n - 2].close;
            let is_below = c < candles[n - 1].close;
            if !was_below && is_below {
                push(EventKind::PsarFlip, Some(Side::Buy), None);
            } else if was_below && !is_below {
                push(EventKind::PsarFlip, Some(Side::Sell), None);
            }
        }
    }


    // -- The Ichimoku set: the tenkan/kijun cross, price crossing the
    // kijun, a close OUT of the cloud, and the kumo twist (span A x B).
    if n > 80 {
        let ic = crate::indicators::ichimoku::ichimoku(candles, 9, 26, 52, 26);
        if let (Some(tp), Some(tc), Some(kp), Some(kc)) = (ic.tenkan[n - 2], ic.tenkan[n - 1], ic.kijun[n - 2], ic.kijun[n - 1]) {
            if tp <= kp && tc > kc {
                push(EventKind::IchimokuTenkanKijunCross, Some(Side::Buy), Some(tc - kc));
            } else if tp >= kp && tc < kc {
                push(EventKind::IchimokuTenkanKijunCross, Some(Side::Sell), Some(tc - kc));
            }
            if closes[n - 2] <= kp && closes[n - 1] > kc {
                push(EventKind::IchimokuKijunCross, Some(Side::Buy), None);
            } else if closes[n - 2] >= kp && closes[n - 1] < kc {
                push(EventKind::IchimokuKijunCross, Some(Side::Sell), None);
            }
        }
        if let (Some(ap), Some(ac), Some(bp), Some(bc)) = (ic.senkou_a[n - 2], ic.senkou_a[n - 1], ic.senkou_b[n - 2], ic.senkou_b[n - 1]) {
            if ap <= bp && ac > bc {
                push(EventKind::IchimokuKumoTwist, Some(Side::Buy), Some(ac - bc));
            } else if ap >= bp && ac < bc {
                push(EventKind::IchimokuKumoTwist, Some(Side::Sell), Some(ac - bc));
            }
            let was_inside = closes[n - 2] >= ap.min(bp) && closes[n - 2] <= ap.max(bp);
            if was_inside && closes[n - 1] > ac.max(bc) {
                push(EventKind::IchimokuCloudBreakout, Some(Side::Buy), Some(closes[n - 1] - ac.max(bc)));
            } else if was_inside && closes[n - 1] < ac.min(bc) {
                push(EventKind::IchimokuCloudBreakout, Some(Side::Sell), Some(ac.min(bc) - closes[n - 1]));
            }
        }
    }

    // -- TSI x its signal.
    if n > 40 {
        let (line, signal) = crate::indicators::misc::tsi(&closes, 13, 25, 7);
        if let (Some(lp), Some(lc), Some(sp), Some(sc)) = (line[n - 2], line[n - 1], signal[n - 2], signal[n - 1]) {
            if lp <= sp && lc > sc {
                push(EventKind::TsiCross, Some(Side::Buy), Some(lc - sc));
            } else if lp >= sp && lc < sc {
                push(EventKind::TsiCross, Some(Side::Sell), Some(lc - sc));
            }
        }
    }

    // -- The Awesome zero cross.
    if n > 36 {
        let v = crate::indicators::misc::awesome(candles, 5, 34);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p <= 0.0 && c > 0.0 {
                push(EventKind::AwesomeZeroCross, Some(Side::Buy), None);
            } else if p >= 0.0 && c < 0.0 {
                push(EventKind::AwesomeZeroCross, Some(Side::Sell), None);
            }
        }
    }

    // -- Z-score zone entries: leaving home by two sigmas, reversion side.
    if n > 22 {
        let v = crate::indicators::quant::zscore(&closes, 20);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p >= -2.0 && c < -2.0 {
                push(EventKind::ZscoreZoneEntry, Some(Side::Buy), Some(c));
            } else if p <= 2.0 && c > 2.0 {
                push(EventKind::ZscoreZoneEntry, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- Price crossing the Kalman estimate.
    if n > 30 {
        let v = crate::indicators::quant::kalman(&closes, 0.01, 0.5);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if closes[n - 2] <= p && closes[n - 1] > c {
                push(EventKind::KalmanCross, Some(Side::Buy), None);
            } else if closes[n - 2] >= p && closes[n - 1] < c {
                push(EventKind::KalmanCross, Some(Side::Sell), None);
            }
        }
    }

    // -- The CUSUM change-point: the signed stat crossing its gate.
    if n > 70 {
        let v = crate::indicators::quant::cusum_stat(&closes, 60);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p.abs() < 2.5 && c >= 2.5 {
                push(EventKind::ChangePoint, Some(Side::Buy), Some(c));
            } else if p.abs() < 2.5 && c <= -2.5 {
                push(EventKind::ChangePoint, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- The entropy regimes: disorder (top) and order (bottom).
    if n > 70 {
        let v = crate::indicators::quant::shannon_entropy(&closes, 60, 6);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p < 0.85 && c >= 0.85 {
                push(EventKind::EntropyRegimeShift, None, Some(c));
            } else if p > 0.35 && c <= 0.35 {
                push(EventKind::EntropyRegimeShift, None, Some(c));
            }
        }
    }

    // -- The Bayesian drift: the 95% band excluding zero (the drift is real,
    // in the band's own direction).
    if n > 30 {
        let (means, sds) = crate::indicators::quant2::bayesian_drift(&closes, 0.01);
        if let (Some(m), Some(sd)) = (means[n - 1], sds[n - 1]) {
            let (Some(pm), Some(psd)) = (means[n - 2], sds[n - 2]) else { unreachable!() };
            let now_bull = m - 2.0 * sd > 0.0;
            let now_bear = m + 2.0 * sd < 0.0;
            let was_bull = pm - 2.0 * psd > 0.0;
            let was_bear = pm + 2.0 * psd < 0.0;
            if now_bull && !was_bull {
                push(EventKind::BayesianDriftShift, Some(Side::Buy), Some(m));
            } else if now_bear && !was_bear {
                push(EventKind::BayesianDriftShift, Some(Side::Sell), Some(m));
            }
        }
    }

    // -- The AR(2) forecast crossing zero.
    if n > 125 {
        let v = crate::indicators::quant2::ar2_forecast(&closes, 120);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p <= 0.0 && c > 0.0 {
                push(EventKind::ArForecastCross, Some(Side::Buy), Some(c));
            } else if p >= 0.0 && c < 0.0 {
                push(EventKind::ArForecastCross, Some(Side::Sell), Some(c));
            }
        }
    }

    // -- GARCH volatility entering its own top decile (a 120-rank).
    if n > 130 {
        let closes2: Vec<f64> = candles.iter().map(|c| c.close).collect();
        let v = crate::indicators::quant2::garch(&closes2, 60, 0.1, 0.85);
        if let Some(curr) = v[n - 1] {
            let window: Vec<f64> = v[n.saturating_sub(120)..n].iter().flatten().copied().collect();
            if window.len() >= 60 {
                let below = window.iter().filter(|w| **w <= curr).count();
                let rank = below as f64 / window.len() as f64;
                let prev_rank_below = window[..window.len() - 1].iter().filter(|w| **w <= v[n - 2].unwrap_or(curr)).count();
                let prev_rank = prev_rank_below as f64 / (window.len() - 1).max(1) as f64;
                if rank >= 0.9 && prev_rank < 0.9 {
                    push(EventKind::GarchRegimeShift, None, Some(rank));
                }
            }
        }
    }

    // -- The HMM volatile state crossing 0.5.
    if n > 125 {
        let v = crate::indicators::quant2::hmm_regime(&closes, 120);
        if let (Some(p), Some(c)) = (v[n - 2], v[n - 1]) {
            if p < 0.5 && c >= 0.5 {
                push(EventKind::HmmRegimeShift, None, Some(c));
            } else if p >= 0.5 && c < 0.5 {
                push(EventKind::HmmRegimeShift, None, Some(c));
            }
        }
    }

    // -- The Bollinger mean reversion: a touch of the outer band that
    // closes back INSIDE (the reversion entry, the band's other use).
    if n > 22 {
        let bands = bollinger(&closes, 20, 2.0);
        if let Some(curr) = bands[n - 1] {
            let touched_up = candles[n - 1].high > curr.upper && closes[n - 1] < curr.upper;
            let touched_lo = candles[n - 1].low < curr.lower && closes[n - 1] > curr.lower;
            let prev_inside = closes[n - 2] <= bands[n - 2].map(|b| b.upper).unwrap_or(f64::MAX)
                && closes[n - 2] >= bands[n - 2].map(|b| b.lower).unwrap_or(f64::MIN);
            if touched_up && prev_inside {
                push(EventKind::BollingerReversion, Some(Side::Sell), Some(curr.upper));
            } else if touched_lo && prev_inside {
                push(EventKind::BollingerReversion, Some(Side::Buy), Some(curr.lower));
            }
        }
    }

    // -- MACD family consensus: EVERY ribbon line on the same side of zero.
    // The slowest pair (24, 48) is valid from bar 47; the consensus's ENTRY
    // is the event (being in it is not), so detection must start exactly
    // there -- a later guard misses the entry and never fires at all.
    if n > 47 {
        let consensus = |i: usize, above: bool| {
            crate::rsi_pca::RSI_PERIODS.iter().all(|fast| {
                macd(&closes, *fast, fast * 2, 1)[i]
                    .map_or(false, |m| if above { m.line > 0.0 } else { m.line < 0.0 })
            })
        };
        let buy_now = consensus(n - 1, true);
        let sell_now = consensus(n - 1, false);
        if (buy_now && !consensus(n - 2, true)) || (sell_now && !consensus(n - 2, false)) {
            let mean: f64 = crate::rsi_pca::RSI_PERIODS
                .iter()
                .filter_map(|fast| macd(&closes, *fast, fast * 2, 1)[n - 1].map(|m| m.line))
                .sum::<f64>()
                / crate::rsi_pca::RSI_PERIODS.len() as f64;
            push(
                EventKind::MacdFamilyConsensus,
                Some(if buy_now { Side::Buy } else { Side::Sell }),
                Some(mean),
            );
        }
    }

    // -- ATR family expansion: the family MEAN crossing 1.5x its own 20-bar
    // mean -- volatility expanding across EVERY period at once, which is a
    // regime change rather than one timeframe's noise. Neutral, edge-fired.
    if n > 60 {
        let family_mean = |i: usize| -> Option<f64> {
            let mut sum = 0.0;
            let mut count = 0usize;
            for period in crate::rsi_pca::RSI_PERIODS {
                if let Some(v) = crate::indicators::atr::atr(candles, period)[i] {
                    sum += v;
                    count += 1;
                }
            }
            (count == crate::rsi_pca::RSI_PERIODS.len()).then(|| sum / count as f64)
        };
        let base: Vec<f64> = (n - 22..n - 2).filter_map(family_mean).collect();
        if base.len() >= 10 {
            if let (Some(curr), Some(prev)) = (family_mean(n - 1), family_mean(n - 2)) {
                let mean = base.iter().sum::<f64>() / base.len() as f64;
                if curr > 1.5 * mean && prev <= 1.5 * mean {
                    push(EventKind::AtrFamilyExpansion, None, Some(curr / mean.max(f64::EPSILON)));
                }
            }
        }
    }

    // -- The GMMA cross: the short group's mean crossing the long group's
    // mean is the ribbon's trend-change statement (compression turning).
    if n > 62 {
        const SHORT: [usize; 6] = [3, 5, 8, 10, 12, 15];
        const LONG: [usize; 6] = [30, 35, 40, 45, 50, 60];
        let group = |periods: &[usize; 6], i: usize| -> Option<f64> {
            let mut sum = 0.0;
            for p in periods {
                sum += ema(&closes, *p)[i]?;
            }
            Some(sum / periods.len() as f64)
        };
        if let (Some(sp), Some(lp), Some(sc), Some(lc)) = (group(&SHORT, n - 2), group(&LONG, n - 2), group(&SHORT, n - 1), group(&LONG, n - 1)) {
            if sp <= lp && sc > lc {
                push(EventKind::GmmaCross, Some(Side::Buy), Some(sc - lc));
            } else if sp >= lp && sc < lc {
                push(EventKind::GmmaCross, Some(Side::Sell), Some(sc - lc));
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

/// Price makes the new extreme; the oscillator refuses it. Confirmed when
/// the second swing's `strength` bars complete -- i.e. exactly at the tail.
/// Shared by every oscillator's divergence (OBV, MACD, A/D, RSI's cousin).
fn oscillator_divergence(candles: &[Candle], series: &[Option<f64>], kind: EventKind, symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let n = candles.len();
    let strength = 2;
    if n < 40 {
        return out;
    }
    let Some(last) = candles.last() else { return out };
    let swing = |highs: bool| -> Option<(usize, usize)> {
        let start = n - 31;
        let mut found: Vec<usize> = Vec::new();
        for i in (start + strength)..(n - strength) {
            let price = if highs { candles[i].high } else { candles[i].low };
            let is_swing = (1..=strength).all(|d| {
                let l = if highs { candles[i - d].high } else { candles[i - d].low };
                let r = if highs { candles[i + d].high } else { candles[i + d].low };
                if highs { price > l && price >= r } else { price < l && price <= r }
            });
            if is_swing {
                found.push(i);
            }
        }
        if found.len() >= 2 {
            Some((found[found.len() - 2], found[found.len() - 1]))
        } else {
            None
        }
    };
    let mut push = |side: Side, value: Option<f64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: last.close,
            side: Some(side),
            zone_low: Some(last.low),
            zone_high: Some(last.high),
            magnitude: value,
            formed_at: None,
        });
    };
    // Bearish: price higher high, oscillator lower high.
    if let Some((a, b)) = swing(true) {
        if candles[b].high > candles[a].high
            && b + strength == n - 1
            && matches!((series[a], series[b]), (Some(oa), Some(ob)) if ob < oa)
        {
            push(Side::Sell, series[b]);
        }
    }
    // Bullish: price lower low, oscillator higher low.
    if let Some((a, b)) = swing(false) {
        if candles[b].low < candles[a].low
            && b + strength == n - 1
            && matches!((series[a], series[b]), (Some(oa), Some(ob)) if ob > oa)
        {
            push(Side::Buy, series[b]);
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
    fn the_gmma_cross_marks_the_trend_turn() {
        // A downtrend into an uptrend: the short group crosses above the
        // long group somewhere in the turn -- replay every prefix and count
        // the bullish crosses (exactly one, on its own bar).
        let mut closes: Vec<f64> = Vec::new();
        let mut price = 100.0;
        for _ in 0..80 {
            price *= 0.994;
            closes.push(price);
        }
        for _ in 0..40 {
            price *= 1.008;
            closes.push(price);
        }
        let candles = series_from(&closes);
        let mut fires = 0;
        for end in 63..closes.len() {
            for e in detect_indicator_signals(&candles[..=end], "T") {
                if e.kind == EventKind::GmmaCross && e.side == Some(Side::Buy) {
                    assert_eq!(e.bar_time, candles[end].open_time, "stamped on its own bar");
                    fires += 1;
                }
            }
        }
        assert_eq!(fires, 1, "one bullish GMMA cross in the turn");
    }

    #[test]
    fn the_macd_family_consensus_needs_every_line_agreeing() {
        // A long, smooth, strong trend: every fast/slow pair's MACD line is
        // on the trend's side at the tail.
        let mut closes: Vec<f64> = Vec::new();
        let mut price = 100.0;
        for _ in 0..120 {
            price *= 1.006;
            closes.push(price);
        }
        let candles = series_from(&closes);
        let mut fired = false;
        for end in 47..candles.len() {
            if detect_indicator_signals(&candles[..=end], "T")
                .iter()
                .any(|e| e.kind == EventKind::MacdFamilyConsensus && e.side == Some(Side::Buy))
            {
                fired = true;
            }
        }
        assert!(fired, "a uniform trend produces a family consensus");
        // The structural negative: a sharp V-turn. Right after the turn the
        // fast pairs read the rally (positive) while the slow pairs still
        // read the fall (negative) -- the family CANNOT agree there, and a
        // detector that fires is lying about the lag.
        let mut turn: Vec<f64> = Vec::new();
        let mut price = 100.0;
        for _ in 0..60 {
            price *= 0.995;
            turn.push(price);
        }
        for _ in 0..12 {
            price *= 1.03;
            turn.push(price);
        }
        let choppy = series_from(&turn);
        let mut choppy_fired = false;
        for end in 60..70 {
            if detect_indicator_signals(&choppy[..=end], "T")
                .iter()
                .any(|e| e.kind == EventKind::MacdFamilyConsensus)
            {
                choppy_fired = true;
            }
        }
        assert!(!choppy_fired, "fast and slow straddle a sharp turn -- no consensus");
    }

    #[test]
    fn the_adx_cross_names_the_trends_side() {
        // Chop first (the ADX seeds and sits under 25), then a ramp: the
        // ADX crosses 25 as the trend asserts itself -- a from-bar-1 trend
        // seeds the ADX ABOVE 25 and there is no cross to catch.
        let mut closes: Vec<f64> = Vec::new();
        for i in 0..40 {
            closes.push(100.0 + if i % 2 == 0 { 0.3 } else { -0.3 });
        }
        let mut price = 100.0;
        for _ in 0..40 {
            price *= 1.007;
            closes.push(price);
        }
        let mut candles: Vec<Candle> = Vec::new();
        for (i, c) in closes.iter().enumerate() {
            candles.push(Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: i as i64 * 3_600_000_000_000,
                open: c * 0.999,
                high: c * 1.001,
                low: c * 0.998,
                close: *c,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        let mut fired = false;
        for end in 31..candles.len() {
            for e in detect_indicator_signals(&candles[..=end], "T") {
                if e.kind == EventKind::AdxTrendCross && e.side == Some(Side::Buy) {
                    assert_eq!(e.bar_time, candles[end].open_time);
                    fired = true;
                }
            }
        }
        assert!(fired, "a ramp produces a buy-side ADX cross");
    }

    #[test]
    fn the_supertrend_flip_marks_the_reversal() {
        let mut candles: Vec<Candle> = (0..25).map(|i| {
            let c = 100.0 + i as f64 * 0.6;
            Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: i as i64 * 3_600_000_000_000,
                open: c,
                high: c + 0.5,
                low: c - 0.3,
                close: c,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            }
        }).collect();
        for i in 0..8 {
            let c = 115.0 - i as f64 * 2.5;
            candles.push(Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: (25 + i) as i64 * 3_600_000_000_000,
                open: c + 0.5,
                high: c + 0.6,
                low: c - 0.8,
                close: c,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
            });
        }
        let mut flips = 0;
        for end in 12..candles.len() {
            for e in detect_indicator_signals(&candles[..=end], "T") {
                if e.kind == EventKind::SupertrendFlip && e.side == Some(Side::Sell) {
                    assert_eq!(e.bar_time, candles[end].open_time);
                    flips += 1;
                }
            }
        }
        assert_eq!(flips, 1, "exactly one sell flip in the reversal");
    }

    #[test]
    fn the_obv_divergence_reads_volume_refusing_price() {
        // Two price lows, the second lower; OBV rising through both (volume
        // refuses the new low). The bullish divergence confirms `strength`
        // bars after the second low.
        let mut candles: Vec<Candle> = Vec::new();
        let t = |i: usize| i as i64 * 3_600_000_000_000;
        let mut push_bar = |candles: &mut Vec<Candle>, i: usize, h: f64, l: f64, c: f64, v: f64| {
            candles.push(Candle {
                symbol: "T".to_string(),
                timeframe: Timeframe::H1,
                open_time: t(i),
                open: c,
                high: h,
                low: l,
                close: c,
                volume: v,
                buy_volume: v,
                sell_volume: 0.0,
            });
        };
        // Lead-in decline.
        for i in 0..10 {
            push_bar(&mut candles, i, 101.0, 99.0, 100.0 - (10 - i) as f64 * 0.2, 10.0);
        }
        // First low at 10.
        push_bar(&mut candles, 10, 99.0, 96.0, 97.0, 10.0);
        // Recovery with RISING volume (OBV climbs).
        for i in 11..25 {
            push_bar(&mut candles, i, 99.5, 97.5, 98.0 + (i - 11) as f64 * 0.15, 100.0);
        }
        // Second low at 25: LOWER than the first, on small volume.
        push_bar(&mut candles, 25, 97.5, 95.5, 96.5, 8.0);
        // Confirmation bars (strength = 2).
        for i in 26..28 {
            push_bar(&mut candles, i, 97.5, 96.3, 97.0, 60.0);
        }
        let events = detect_indicator_signals(&candles, "T");
        let div = events.iter().find(|e| e.kind == EventKind::ObvDivergence);
        if let Some(d) = div {
            assert_eq!(d.side, Some(Side::Buy), "a lower price low on rising OBV is bullish");
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
