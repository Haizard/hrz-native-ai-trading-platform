//! The wickra adapter: the external engine's harmonic detections, wrapped
//! onto the platform's event stream.
//!
//! The integration rule, same as every adapter here: the external engine
//! COMPUTES, the event contract WRAPS. Wickra's detectors are streaming
//! state machines (one `update` per candle, `+1.0` on a bullish completion,
//! `-1.0` on a bearish one), so the adapter replays the window through each
//! of them and emits a [`MarketEvent`] exactly on completion bars.
//!
//! Their math is proven by OUR tests before any of it is trusted (the
//! platform rule for external logic): a textbook XABCD shape fires at its
//! D point, and noise fires nothing.
//!
//! ## The honest strictness note
//!
//! Wickra's harmonic detectors bake in a 5% swing-reversal threshold and the
//! tight textbook Fibonacci windows. Verified on this platform (October
//! 2026): that combination is geometrically strict enough that completions
//! effectively do not occur on real noisy data -- zero across 3 years of
//! BTCUSDT daily bars and 800-bar 15m windows on BTC and Deriv synthetics.
//! The detectors are CORRECT (the textbook shape fires exactly at D), but a
//! trader expecting frequent harmonic signals should know the threshold
//! reality: these fire on exceptionally clean shapes only. A calibrated
//! in-house detector with market-tuned thresholds is the documented
//! follow-up if the family needs to be practical rather than pure.

use crate::events::{EventKind, MarketEvent};
use crate::types::{Candle, Side};
use wickra::Indicator;

/// One detector's identity: the event kind it maps to and its constructor.
struct Detector {
    kind: EventKind,
    make: fn() -> Box<dyn Indicator<Input = wickra::Candle, Output = f64>>,
}

/// The harmonic family, all eight, over one set of constructors.
fn detectors() -> Vec<Detector> {
    vec![
        Detector { kind: EventKind::HarmonicGartley, make: || Box::new(wickra::Gartley::new()) },
        Detector { kind: EventKind::HarmonicBat, make: || Box::new(wickra::Bat::new()) },
        Detector { kind: EventKind::HarmonicButterfly, make: || Box::new(wickra::Butterfly::new()) },
        Detector { kind: EventKind::HarmonicCrab, make: || Box::new(wickra::Crab::new()) },
        Detector { kind: EventKind::HarmonicShark, make: || Box::new(wickra::Shark::new()) },
        Detector { kind: EventKind::HarmonicCypher, make: || Box::new(wickra::Cypher::new()) },
        Detector { kind: EventKind::HarmonicAbcd, make: || Box::new(wickra::Abcd::new()) },
        Detector { kind: EventKind::HarmonicThreeDrives, make: || Box::new(wickra::ThreeDrives::new()) },
    ]
}

/// Our candle into theirs, through their validating constructor. An
/// upstream candle that breaks OHLC (sloppy data happens) is skipped rather
/// than fed to their validated engine.
fn as_wickra(candle: &Candle) -> Option<wickra::Candle> {
    wickra::Candle::new(
        candle.open,
        candle.high,
        candle.low,
        candle.close,
        candle.volume,
        candle.open_time,
    )
    .ok()
}

/// The candlestick family from the master list, wickra-driven: per-bar
/// signed outputs (their own +1.0/-1.0) wrapped as events on the bar they
/// land on. Our homegrown fourteen stay the authority where they overlap;
/// these are the patterns the list has and we did not.
fn candlestick_detectors() -> Vec<Detector> {
    vec![
        Detector { kind: EventKind::DragonflyDoji, make: || Box::new(wickra::DragonflyDoji::new()) },
        Detector { kind: EventKind::GravestoneDoji, make: || Box::new(wickra::GravestoneDoji::new()) },
        Detector { kind: EventKind::LongLeggedDoji, make: || Box::new(wickra::LongLeggedDoji::new()) },
        Detector { kind: EventKind::SpinningTop, make: || Box::new(wickra::SpinningTop::new()) },
        Detector { kind: EventKind::Tweezer, make: || Box::new(wickra::Tweezer::new()) },
        Detector { kind: EventKind::Kicker, make: || Box::new(wickra::Kicking::new()) },
        Detector { kind: EventKind::MatchingLow, make: || Box::new(wickra::MatchingLow::new()) },
        Detector { kind: EventKind::OnNeck, make: || Box::new(wickra::OnNeck::new()) },
        Detector { kind: EventKind::ThreeInside, make: || Box::new(wickra::ThreeInside::new()) },
        Detector { kind: EventKind::ThreeOutside, make: || Box::new(wickra::ThreeOutside::new()) },
        Detector { kind: EventKind::ThreeMethods, make: || Box::new(wickra::RisingThreeMethods::new()) },
        Detector { kind: EventKind::ThreeMethods, make: || Box::new(wickra::FallingThreeMethods::new()) },
        Detector { kind: EventKind::AbandonedBaby, make: || Box::new(wickra::AbandonedBaby::new()) },
        Detector { kind: EventKind::TriStar, make: || Box::new(wickra::Tristar::new()) },
        Detector { kind: EventKind::StickSandwich, make: || Box::new(wickra::StickSandwich::new()) },
        Detector { kind: EventKind::MatHold, make: || Box::new(wickra::MatHold::new()) },
        Detector { kind: EventKind::TasukiGap, make: || Box::new(wickra::TasukiGap::new()) },
        Detector { kind: EventKind::LadderBottom, make: || Box::new(wickra::LadderBottom::new()) },
        Detector { kind: EventKind::BeltHold, make: || Box::new(wickra::BeltHold::new()) },
        Detector { kind: EventKind::Hikkake, make: || Box::new(wickra::Hikkake::new()) },
    ]
}

/// Replay the window through the candlestick detectors, one event per
/// pattern bar (their signed values carry the direction).
pub fn detect_candlesticks(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 2 {
        return out;
    }
    for detector in candlestick_detectors() {
        let mut engine = (detector.make)();
        for candle in candles {
            let Some(wc) = as_wickra(candle) else { continue };
            let Some(value) = engine.update(wc) else { continue };
            if value == 0.0 {
                continue;
            }
            out.push(MarketEvent {
                kind: detector.kind,
                symbol: symbol.to_string(),
                timeframe: candle.timeframe,
                bar_time: candle.open_time,
                close: candle.close,
                price: candle.close,
                side: Some(if value > 0.0 { Side::Buy } else { Side::Sell }),
                zone_low: Some(candle.low),
                zone_high: Some(candle.high),
                magnitude: Some(value.abs()),
                formed_at: None,
            });
        }
    }
    out
}

/// Replay the window through the harmonic detectors, emitting one event per
/// completion. The completions are the detector's own ±1.0 outputs on the
/// bar they land on; the level is that bar's close (the completion point).
pub fn detect_harmonics(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 12 {
        return out;
    }
    for detector in detectors() {
        let mut engine = (detector.make)();
        for (i, candle) in candles.iter().enumerate() {
            let Some(wc) = as_wickra(candle) else { continue };
            let signal = engine.update(wc);
            let Some(value) = signal else { continue };
            if value == 0.0 {
                continue;
            }
            out.push(MarketEvent {
                kind: detector.kind,
                symbol: symbol.to_string(),
                timeframe: candle.timeframe,
                bar_time: candle.open_time,
                close: candle.close,
                price: candle.close,
                side: Some(if value > 0.0 { Side::Buy } else { Side::Sell }),
                zone_low: None,
                zone_high: None,
                magnitude: Some(value.abs()),
                formed_at: None,
            });
            let _ = i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn k(t: i64, price: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: price,
            high: price + 0.4,
            low: price - 0.4,
            close: price,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    /// A textbook bullish Gartley, with every leg clearing the engine's 5%
    /// pivot-confirmation threshold: X=100, A=112, B=105 (0.583 of XA), C=110.5
    /// (0.786 of AB), D=103 (0.75 of XA, 1.364 of BC) -- and a 5%+ bounce
    /// after D, because the completion fires when D CONFIRMS, not when it prints.
    fn textbook_gartley() -> Vec<Candle> {
        let h = 3_600_000_000_000_i64;
        let prices = [
            // A leading swing HIGH first: the tracker bootstraps tracking an
            // uptrend, so without a prior high the X low never becomes a pivot.
            105.0, 102.0, 100.0,               // prior high 105 -> X=100
            104.0, 108.0, 112.0,               // X to A (A=112)
            110.0, 107.0, 105.0,               // A to B (6.25% reversal)
            107.0, 109.0, 110.5,               // B to C (5.2%)
            108.0, 105.0, 103.0,               // C to D (6.8%)
            104.5, 106.0, 108.0, 109.0,        // the bounce (5.8%: D confirms)
            106.0, 103.5, 106.5, 108.5,        // another swing cycle, so seven
                                               // pivots accumulate for the
                                               // Three Drives detector (cap 7)
        ];
        prices
            .iter()
            .enumerate()
            .map(|(i, p)| k(i as i64 * h, *p))
            .collect()
    }

    #[test]
    fn the_gartley_fires_at_its_completion_point() {
        let events = detect_harmonics(&textbook_gartley(), "T");
        let gartley = events.iter().find(|e| e.kind == EventKind::HarmonicGartley);
        assert!(gartley.is_some(), "the textbook shape completes: {:?}", events.iter().map(|e| e.kind.name()).collect::<Vec<_>>());
        let gartley = gartley.expect("checked above");
        assert_eq!(gartley.side, Some(Side::Buy), "D is a swing low: bullish");
        assert!(
            gartley.price > 102.0 && gartley.price < 110.0,
            "the level lands in the completion region: {}",
            gartley.price
        );
    }

    fn textbook_gartley_no_tail() -> Vec<Candle> {
        let h = 3_600_000_000_000_i64;
        let prices = [
            106.0, 105.0, 102.0, 100.0,
            104.0, 108.0, 112.0,
            110.0, 107.0, 105.0,
            107.0, 109.0, 110.5,
            108.0, 105.0, 103.0,
            104.5, 106.0, 108.0, 109.0,
        ];
        prices.iter().enumerate().map(|(i, p)| k(i as i64 * h, *p)).collect()
    }

    #[test]
    fn the_completion_reaches_the_engine_stream() {
        let events = crate::events::detect_events(
            &textbook_gartley_no_tail(),
            "T",
            &crate::events::EventConfig::default(),
        );
        let gartley = events.iter().find(|e| e.kind == EventKind::HarmonicGartley);
        assert!(gartley.is_some(), "the merge delivers it: {:?}", events.iter().map(|e| e.kind.name()).collect::<Vec<_>>());
    }

    #[test]
    fn the_wickra_candle_family_fires_on_textbook_shapes() {
        // The value-pinning rule for external logic: their patterns prove
        // themselves on shapes any textbook would accept.
        let h = 3_600_000_000_000_i64;
        let bar = |t: i64, o: f64, hi: f64, lo: f64, c: f64| Candle {
            symbol: "T".to_string(),
            timeframe: Timeframe::H1,
            open_time: t,
            open: o,
            high: hi,
            low: lo,
            close: c,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        };
        // A dragonfly doji: open/close at the top, a long lower shadow.
        let candles = vec![
            bar(0, 100.0, 100.6, 99.5, 100.3),
            bar(h, 100.2, 100.3, 96.0, 100.1),
        ];
        let events = detect_candlesticks(&candles, "T");
        assert!(
            events.iter().any(|e| e.kind == EventKind::DragonflyDoji && e.side == Some(Side::Buy)),
            "the dragonfly fires bullish: {:?}",
            events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
        );

        // A kicking gap: a black marubozu (body >= 95% of range), then a
        // white marubozu gapping ABOVE the prior bar's high (their rule).
        let candles = vec![
            bar(0, 101.0, 101.05, 99.0, 99.05),
            bar(h, 101.5, 102.95, 101.5, 102.9),
        ];
        let events = detect_candlesticks(&candles, "T");
        assert!(
            events.iter().any(|e| e.kind == EventKind::Kicker && e.side == Some(Side::Buy)),
            "the bullish kicker fires: {:?}",
            events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
        );

        // Every candlestick detector constructs and answers on a real series
        // (long enough for the multi-bar families to evaluate at all).
        let candles = vec![
            bar(0, 101.0, 101.2, 99.0, 99.3),
            bar(h, 100.0, 100.4, 99.5, 100.1),
            bar(2 * h, 100.1, 100.5, 99.8, 100.3),
            bar(3 * h, 100.3, 100.7, 100.0, 100.5),
            bar(4 * h, 100.5, 100.9, 100.2, 100.7),
            bar(5 * h, 100.7, 101.1, 100.4, 100.9),
            bar(6 * h, 100.9, 101.3, 100.6, 101.1),
            bar(7 * h, 101.1, 101.5, 100.8, 101.3),
        ];
        for detector in candlestick_detectors() {
            let mut engine = (detector.make)();
            let mut answered = false;
            for candle in &candles {
                if let Some(wc) = as_wickra(candle) {
                    if engine.update(wc).is_some() {
                        answered = true;
                    }
                }
            }
            assert!(answered, "{:?} never answered", detector.kind);
        }
    }

    #[test]
    fn flat_noise_completes_nothing() {
        let h = 3_600_000_000_000_i64;
        let candles: Vec<Candle> = (0..40)
            .map(|i| k(i as i64 * h, 100.0 + (i % 3) as f64 * 0.1))
            .collect();
        let events = detect_harmonics(&candles, "T");
        assert!(events.is_empty(), "flat series, no harmonics: {:?}", events.len());
    }

    #[test]
    fn every_detector_constructs_and_answers() {
        // The 8 detectors must all exist and produce a value on a real
        // series (even a 0.0): a constructor that silently fails is a
        // registry entry the catalog cannot back.
        let candles = textbook_gartley();
        for detector in detectors() {
            let mut engine = (detector.make)();
            let mut answered = false;
            for candle in &candles {
                if let Some(wc) = as_wickra(candle) {
                    if engine.update(wc).is_some() {
                        answered = true;
                    }
                }
            }
            assert!(answered, "{:?} never answered on a real series", detector.kind);
        }
    }
}
