//! Derived market events -- the layer between raw candles and an agent that
//! reacts (`docs/09`, the event-driven posture).
//!
//! ## Why this exists
//!
//! The platform's event bus (`market_data::MarketEventBus`) carries **raw**
//! lanes: trades, closed candles, order-book snapshots. Nothing on it says
//! "liquidity at 104100 was just swept" or "a bullish fair value gap just
//! opened", because those are *derived* facts -- they exist only after
//! detectors have compared the new bar against what came before it. This
//! module is that comparison, expressed once, deterministically, in the same
//! crate as every other trading calculation.
//!
//! ## The contract: diff a window, emit what the window contains
//!
//! [`detect_events`] takes a candle series and reports every event the
//! **newest** candle produced. It is deliberately a *diff*, not a history:
//! callers hand it the trailing window each time a candle closes, and the
//! events that come back are exactly the ones that bar is responsible for.
//! That is what makes it embeddable in a live loop without a dedup ledger --
//! the caller's own candle stream is the dedup.
//!
//! Everything here is pure: same candles in, same events out. The lane's
//! consumers (the agent socket, the AI's own tools, a future watcher) can
//! replay a window and get the identical answer the live loop reported.
//!
//! ## What is deliberately *not* here
//!
//! No ordering or priority between events, no thresholds beyond the explicit
//! ones in [`EventConfig`], and no state. A sweep that happened three bars ago
//! is not this module's news; the bar that swept it already reported it.

use serde::{Deserialize, Serialize};

use crate::liquidity::{detect_liquidity_levels_with, LiquidityConfig};
use crate::market_structure::{detect_market_structure, StructureConfig};
use crate::types::{Candle, Side, Timeframe};

/// How far back the detectors look, in bars, when the caller does not say.
///
/// Large enough that a swing detector with the default three-bar lookback has
/// several confirmed swings to work with; small enough that a live loop can
/// run it per closed bar on a 300-bar buffer without noticing.
pub const DEFAULT_WINDOW: usize = 150;

/// A kind of derived event the engine can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Price ran beyond a liquidity level and closed back inside it.
    LiquiditySweep,
    /// A close through the last swing in the trend's direction.
    Bos,
    /// A close through the last swing against the trend's direction.
    Choch,
    /// A fair value gap opened on this bar.
    FvgCreated,
    /// Price traded fully back through an open fair value gap.
    FvgFilled,
    /// Price traded into a fresh supply/demand zone.
    ZoneTested,
    /// Bar volume exceeded a multiple of the recent average.
    VolumeSpike,
    /// Bar delta exceeded a multiple of the recent average absolute delta.
    DeltaShift,
}

impl EventKind {
    /// The wire name, which is also the shell's label key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LiquiditySweep => "liquidity_sweep",
            Self::Bos => "bos",
            Self::Choch => "choch",
            Self::FvgCreated => "fvg_created",
            Self::FvgFilled => "fvg_filled",
            Self::ZoneTested => "zone_tested",
            Self::VolumeSpike => "volume_spike",
            Self::DeltaShift => "delta_shift",
        }
    }

    /// Every kind, for a client that wants to build a filter menu.
    pub const ALL: [Self; 8] = [
        Self::LiquiditySweep,
        Self::Bos,
        Self::Choch,
        Self::FvgCreated,
        Self::FvgFilled,
        Self::ZoneTested,
        Self::VolumeSpike,
        Self::DeltaShift,
    ];
}

/// One derived market fact, as the engine emits it.
///
/// Carries **levels, not prose**: `price` and `zone_low`/`zone_high` are
/// numbers a chart can draw a line at and an agent can cite verbatim, which is
/// the whole point of the grounding rule -- an event that arrived as a sentence
/// would have to be parsed back into numbers before anyone could act on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketEvent {
    /// What happened.
    pub kind: EventKind,
    /// Which market the bar closed on.
    pub symbol: String,
    /// Which resolution produced the bar.
    pub timeframe: Timeframe,
    /// Open time of the bar that caused the event, unix nanoseconds.
    pub bar_time: i64,
    /// Close of the bar that caused the event.
    pub close: f64,
    /// The level the event is about: the swept price, the broken swing, the
    /// spike bar's close. Kind-specific; documented per variant in prose.
    pub price: f64,
    /// Which side acted, when the event has one. A sweep carries the side of
    /// the *liquidity* (a sell-side sweep releases buying), a break the
    /// direction of the close; spike kinds leave it `None`.
    pub side: Option<Side>,
    /// The bottom of a band, for the band-shaped kinds (`fvg_*`, `zone_tested`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_low: Option<f64>,
    /// The top of a band, for the band-shaped kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_high: Option<f64>,
    /// What the event measured, normalised per kind: the multiple of average
    /// volume a spike reached, the touches on a swept level. `None` where the
    /// kind has no natural magnitude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnitude: Option<f64>,
    /// Open time of the band's origin bar, when the event names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formed_at: Option<i64>,
}

/// Tuning for [`detect_events`].
///
/// The spike thresholds are **multiples of the recent average**, not absolute
/// sizes, so the same config means the same thing on BTCUSDT and on a coin
/// trading at 0.02 -- the same reason the scanner ranks by ATR *percent*.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EventConfig {
    /// Swing lookback for the structure and liquidity detectors.
    pub structure_lookback: usize,
    /// Bars of history the "recent average" for spike kinds spans.
    pub spike_average_bars: usize,
    /// Volume above `multiplier *` the average is a spike.
    pub volume_multiplier: f64,
    /// `|delta|` above `multiplier *` the average absolute delta is a shift.
    pub delta_multiplier: f64,
    /// Price gap, as a fraction of the bar's own range, that must separate the
    /// three-touch rule for a fair value gap.
    ///
    /// A zero fraction accepts any gap at all, which on liquid markets floods
    /// the lane with noise; the default of one eighth keeps the *obvious* gaps.
    pub fvg_min_fraction: f64,
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            structure_lookback: 3,
            spike_average_bars: 20,
            volume_multiplier: 2.5,
            delta_multiplier: 2.5,
            fvg_min_fraction: 0.125,
        }
    }
}

impl EventConfig {
    /// Accept a config that may carry non-finite or degenerate values, by
    /// falling back per field to the default.
    ///
    /// Total rather than an error, because a live loop calls this per closed
    /// bar for every symbol it watches: one malformed field must degrade to
    /// the default, not take the lane down.
    #[must_use]
    pub fn sanitised(self) -> Self {
        let d = Self::default();
        let positive = |v: f64, fallback: f64| {
            if v.is_finite() && v > 0.0 {
                v
            } else {
                fallback
            }
        };
        Self {
            structure_lookback: if self.structure_lookback == 0 {
                d.structure_lookback
            } else {
                self.structure_lookback
            },
            spike_average_bars: if self.spike_average_bars == 0 {
                d.spike_average_bars
            } else {
                self.spike_average_bars
            },
            volume_multiplier: positive(self.volume_multiplier, d.volume_multiplier),
            delta_multiplier: positive(self.delta_multiplier, d.delta_multiplier),
            fvg_min_fraction: positive(self.fvg_min_fraction, d.fvg_min_fraction),
        }
    }
}

/// Every event the **newest** candle in `candles` produced.
///
/// The caller owns the window: pass the trailing [`DEFAULT_WINDOW`] bars (or
/// the whole buffer) each time a candle closes, and what comes back is that
/// bar's news and nothing else. An empty series, or one shorter than the
/// structure lookback needs, yields no events -- that is a fact about the
/// data, not an error.
///
/// Event families and their sources:
///
/// * **Sweep** -- the liquidity detector over the window *excluding* the last
///   bar marks where stops rested; the last bar wicking through and closing
///   back inside fires it. Excluding the swept bar from the level set is what
///   keeps a level from sweeping itself.
/// * **BOS / CHoCH** -- the structure detector over the full window, filtered
///   to the last bar's index.
/// * **FVG created** -- the classic three-candle rule over the last three
///   bars: bar 1's extreme must not be revisited by bar 3, and the gap must
///   clear [`EventConfig::fvg_min_fraction`] of the middle bar's range.
/// * **FVG filled** -- gaps opened earlier in the window that the last bar
///   fully traded back through.
/// * **Zone tested** -- the supply/demand detector over the window, filtered
///   to fresh zones that the last bar's range reaches into.
/// * **Volume spike / delta shift** -- the last bar against the average of
///   [`EventConfig::spike_average_bars`] bars before it.
#[must_use]
pub fn detect_events(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    let config = config.sanitised();
    let Some(last) = candles.last() else {
        return Vec::new();
    };
    if candles.len() <= config.structure_lookback * 2 {
        // Not enough history for a single confirmed swing; the detectors would
        // all return empty anyway, so say it once here.
        return Vec::new();
    }

    let mut events = Vec::new();
    let bar_time = last.open_time;

    // -- Liquidity sweeps ----------------------------------------------------
    // Levels are detected over everything *before* the bar, so a level can
    // never be built from the bar that swept it (the same guard
    // `liquidity.rs` applies internally, one layer down).
    let history = &candles[..candles.len() - 1];
    let levels = detect_liquidity_levels_with(
        history,
        LiquidityConfig {
            lookback: config.structure_lookback,
            ..LiquidityConfig::default()
        },
    );
    for level in levels {
        // A sweep is a *wick* through and a *close* back: the stops were taken,
        // the move was rejected. A close beyond the level is a breakout, which
        // is structure's news (a BOS), not liquidity's.
        let wicked_through = match level.kind {
            k if k.is_above() => last.high > level.price && last.close < level.price,
            k if k.is_below() => last.low < level.price && last.close > level.price,
            _ => continue,
        };
        if !wicked_through {
            continue;
        }
        events.push(MarketEvent {
            kind: EventKind::LiquiditySweep,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time,
            close: last.close,
            price: level.price,
            // The side names the *liquidity*, matching `LiquidityKind`: a
            // sell-side pool releasing is what a long setup wants swept.
            side: Some(if level.kind.is_above() {
                Side::Sell
            } else {
                Side::Buy
            }),
            zone_low: None,
            zone_high: None,
            magnitude: Some(level.touches as f64),
            formed_at: Some(level.formed_at),
        });
    }

    // -- BOS / CHoCH ----------------------------------------------------------
    let structure = detect_market_structure(
        candles,
        StructureConfig {
            lookback: config.structure_lookback,
        },
    );
    for brk in &structure.breaks {
        if brk.timestamp != bar_time {
            continue;
        }
        events.push(MarketEvent {
            kind: match brk.kind {
                crate::market_structure::BreakKind::Bos => EventKind::Bos,
                crate::market_structure::BreakKind::Choch => EventKind::Choch,
            },
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time,
            close: last.close,
            price: brk.level,
            side: Some(brk.direction),
            zone_low: None,
            zone_high: None,
            magnitude: None,
            formed_at: None,
        });
    }

    // -- Fair value gaps: created --------------------------------------------
    if candles.len() >= 3 {
        let (one, two, three) = (
            &candles[candles.len() - 3],
            &candles[candles.len() - 2],
            last,
        );
        let two_range = (two.high - two.low).abs();
        // Bullish: bar 3's low never comes back into bar 1's high. Bearish: bar
        // 3's high never comes down into bar 1's low. The fraction guard keeps
        // a hair-thin gap on a tiny range from being news.
        let bullish = three.low > one.high
            && two_range > 0.0
            && (three.low - one.high) / two_range >= config.fvg_min_fraction;
        let bearish = three.high < one.low
            && two_range > 0.0
            && (one.low - three.high) / two_range >= config.fvg_min_fraction;
        if bullish {
            events.push(MarketEvent {
                kind: EventKind::FvgCreated,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: (three.low + one.high) / 2.0,
                side: Some(Side::Buy),
                zone_low: Some(one.high),
                zone_high: Some(three.low),
                magnitude: None,
                formed_at: Some(two.open_time),
            });
        }
        if bearish {
            events.push(MarketEvent {
                kind: EventKind::FvgCreated,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: (one.low + three.high) / 2.0,
                side: Some(Side::Sell),
                zone_low: Some(three.high),
                zone_high: Some(one.low),
                magnitude: None,
                formed_at: Some(two.open_time),
            });
        }
    }

    // -- Fair value gaps: filled ---------------------------------------------
    // Scan the window's gaps (oldest first) for one the last bar fully traded
    // through. Cap the scan so a long window cannot make the per-bar cost grow
    // without bound: 50 gaps is far more than a 150-bar window produces.
    let mut open_gaps: Vec<(i64, Side, f64, f64)> = Vec::new();
    for i in 2..candles.len().saturating_sub(1) {
        let (one, three) = (&candles[i - 2], &candles[i]);
        if three.low > one.high {
            open_gaps.push((candles[i - 1].open_time, Side::Buy, one.high, three.low));
        }
        if three.high < one.low {
            open_gaps.push((candles[i - 1].open_time, Side::Sell, three.high, one.low));
        }
    }
    open_gaps.sort_by_key(|(t, _, _, _)| *t);
    for (origin, side, low, high) in open_gaps.into_iter().rev().take(50) {
        if origin == bar_time {
            continue;
        }
        let filled = last.low <= low && last.high >= high;
        if !filled {
            continue;
        }
        events.push(MarketEvent {
            kind: EventKind::FvgFilled,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time,
            close: last.close,
            price: (low + high) / 2.0,
            side: Some(side),
            zone_low: Some(low),
            zone_high: Some(high),
            magnitude: None,
            formed_at: Some(origin),
        });
    }

    // -- Zone tests -----------------------------------------------------------
    // The supply/demand detector over the window; a fresh zone the last bar
    // reaches into is being tested right now. `detect_zones` is not free (it
    // re-runs structure detection), so it runs only when the caller's window
    // is not absurd -- and 500 bars is the same bound the tools use.
    if candles.len() <= 500 {
        for zone in crate::regions::detect_zones(
            candles,
            crate::regions::ZoneConfig {
                structure_lookback: config.structure_lookback,
                ..crate::regions::ZoneConfig::default()
            },
        ) {
            if !zone.is_fresh() {
                continue;
            }
            // Reached into, not merely touched: the bar's range must overlap
            // the band's, which is what "price came back to the zone" means.
            let overlaps = last.low <= zone.price_high && last.high >= zone.price_low;
            if !overlaps {
                continue;
            }
            events.push(MarketEvent {
                kind: EventKind::ZoneTested,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: (zone.price_low + zone.price_high) / 2.0,
                side: Some(zone.side),
                zone_low: Some(zone.price_low),
                zone_high: Some(zone.price_high),
                magnitude: None,
                formed_at: Some(zone.formed_at),
            });
        }
    }

    // -- Volume spike ---------------------------------------------------------
    let spike_window = config
        .spike_average_bars
        .min(candles.len().saturating_sub(1));
    if spike_window > 0 {
        let window = &candles[candles.len() - 1 - spike_window..candles.len() - 1];
        let n = window.len() as f64;
        let avg = window.iter().map(|c| c.volume).sum::<f64>() / n;
        if avg > 0.0 && last.volume / avg >= config.volume_multiplier {
            events.push(MarketEvent {
                kind: EventKind::VolumeSpike,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: last.close,
                side: None,
                zone_low: None,
                zone_high: None,
                magnitude: Some(last.volume / avg),
                formed_at: None,
            });
        }
    }

    // -- Delta shift ----------------------------------------------------------
    if spike_window > 0 {
        let window = &candles[candles.len() - 1 - spike_window..candles.len() - 1];
        let n = window.len() as f64;
        let avg_abs = window.iter().map(|c| c.delta().abs()).sum::<f64>() / n;
        if avg_abs > 0.0 && last.delta().abs() / avg_abs >= config.delta_multiplier {
            events.push(MarketEvent {
                kind: EventKind::DeltaShift,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: last.close,
                side: Some(if last.delta() > 0.0 {
                    Side::Buy
                } else {
                    Side::Sell
                }),
                zone_low: None,
                zone_high: None,
                magnitude: Some(last.delta().abs() / avg_abs),
                formed_at: None,
            });
        }
    }

    // Deterministic order: by kind, then by price. A stable order is part of
    // the contract -- a replayed window must name its events in the same order
    // the live loop did, or consumers cannot diff the two.
    events.sort_by(|a, b| {
        (a.kind as usize)
            .cmp(&(b.kind as usize))
            .then_with(|| a.price.total_cmp(&b.price))
    });
    events
}

/// Rebuild the events a window produced, per closed bar.
///
/// The historical twin of [`detect_events`]: walk the series bar by bar and
/// emit each bar's news against the history before it. This is what a backtest
/// of an event-driven rule -- and the platform's own replay tests -- run over.
/// The **last** bar's slice is exactly what [`detect_events`] returns for the
/// same config, so a live lane and a replay cannot disagree.
#[must_use]
pub fn replay_events(
    candles: &[Candle],
    symbol: &str,
    config: &EventConfig,
) -> Vec<(usize, Vec<MarketEvent>)> {
    let mut out = Vec::new();
    for end in 1..=candles.len() {
        let window = &candles[..end];
        let events = detect_events(window, symbol, config);
        if !events.is_empty() {
            out.push((end - 1, events));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(open_time: i64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn volume(c: Candle, volume: f64, buy: f64) -> Candle {
        Candle {
            volume,
            buy_volume: buy,
            sell_volume: volume - buy,
            ..c
        }
    }

    const BASE: i64 = 1_760_000_000_000_000_000; // a plausible bar open
    const MIN: i64 = 60 * 1_000_000_000;

    /// A triangular wave: peaks of 116.25 at every index where `i % 8 == 4`,
    /// troughs of 100 at every `i % 8 == 0`. Unlike a monotonic series -- which
    /// confirms no swing ever, because every later bar is higher -- this one
    /// confirms a swing high three bars after each peak, which is what the
    /// structure and liquidity detectors need to have anything to say.
    fn zigzag(n: usize) -> Vec<Candle> {
        (0..n)
            .map(|i| {
                let phase = i % 8;
                let w = if phase <= 4 { phase } else { 8 - phase };
                let price = 100.0 + 4.0 * w as f64;
                c(BASE + i as i64 * MIN, price + 0.25, price - 0.25, price)
            })
            .collect()
    }

    #[test]
    fn an_empty_or_short_window_yields_no_events() {
        assert!(detect_events(&[], "BTCUSDT", &EventConfig::default()).is_empty());
        let short: Vec<Candle> = (0..6)
            .map(|i| c(BASE + i as i64 * MIN, 10.0, 1.0, 5.0))
            .collect();
        assert!(detect_events(&short, "BTCUSDT", &EventConfig::default()).is_empty());
    }

    #[test]
    fn a_wick_through_equal_highs_that_closes_back_is_a_sweep() {
        // Equal highs at 12 (two touches), each confirmed by three quiet bars,
        // then a bar that runs to 14 and closes at 11: stops taken, move
        // rejected. The touches sit at indices 3 and 7 because a swing is only
        // confirmed once three bars exist on each side -- a touch at index 1
        // would never be one at all.
        let candles = vec![
            c(BASE, 10.0, 8.0, 9.0),
            c(BASE + MIN, 10.5, 9.0, 9.5),
            c(BASE + 2 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 3 * MIN, 12.0, 9.0, 11.0), // touch 1
            c(BASE + 4 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 5 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 6 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 7 * MIN, 12.0, 9.0, 11.5), // touch 2
            c(BASE + 8 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 9 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 10 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 11 * MIN, 14.0, 10.8, 11.0), // the sweep
        ];
        let events = detect_events(&candles, "BTCUSDT", &EventConfig::default());
        let sweep = events
            .iter()
            .find(|e| e.kind == EventKind::LiquiditySweep)
            .expect("the last bar swept the equal highs");
        assert_eq!(sweep.price, 12.0);
        assert_eq!(sweep.side, Some(Side::Sell));
        assert_eq!(sweep.bar_time, BASE + 11 * MIN);
        assert_eq!(sweep.magnitude, Some(2.0), "two touches built the level");
    }

    #[test]
    fn a_close_beyond_the_level_is_not_a_sweep() {
        // Same shape, but the last bar *closes* above: a breakout, which is
        // structure's news. Only the sweep kind must be absent.
        let candles = vec![
            c(BASE, 10.0, 8.0, 9.0),
            c(BASE + MIN, 10.5, 9.0, 9.5),
            c(BASE + 2 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 3 * MIN, 12.0, 9.0, 11.0), // touch 1
            c(BASE + 4 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 5 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 6 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 7 * MIN, 12.0, 9.0, 11.5), // touch 2
            c(BASE + 8 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 9 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 10 * MIN, 10.5, 9.0, 9.5),
            c(BASE + 11 * MIN, 14.0, 11.2, 13.5), // the breakout
        ];
        let events = detect_events(&candles, "BTCUSDT", &EventConfig::default());
        assert!(
            !events.iter().any(|e| e.kind == EventKind::LiquiditySweep),
            "a close through the level is a breakout, not a sweep: {events:?}"
        );
    }

    #[test]
    fn every_swept_level_predates_the_bar_that_swept_it() {
        // The guard the sweep family depends on: levels are detected over the
        // history *excluding* the last bar, so no level can be built from the
        // bar that swept it. A confirmed swing high at 10.4 followed by a
        // wick through it exercises the edge -- and every sweep's level must
        // have formed strictly before the sweep bar.
        let candles = vec![
            c(BASE, 10.0, 8.0, 9.0),
            c(BASE + MIN, 10.2, 9.0, 10.0),
            c(BASE + 2 * MIN, 10.0, 9.2, 9.6),
            c(BASE + 3 * MIN, 10.4, 9.4, 10.2), // the swing high
            c(BASE + 4 * MIN, 10.0, 9.0, 9.4),
            c(BASE + 5 * MIN, 10.2, 9.2, 9.8),
            c(BASE + 6 * MIN, 10.0, 9.0, 9.5),
            c(BASE + 7 * MIN, 10.0, 9.6, 9.8),
            c(BASE + 8 * MIN, 10.0, 9.6, 9.8),
            c(BASE + 9 * MIN, 10.0, 9.6, 9.8),
            c(BASE + 10 * MIN, 12.0, 9.8, 10.1), // the sweep -- and the last bar
        ];
        let events = detect_events(&candles, "BTCUSDT", &EventConfig::default());
        let sweeps: Vec<_> = events
            .iter()
            .filter(|e| e.kind == EventKind::LiquiditySweep)
            .collect();
        assert!(
            !sweeps.is_empty(),
            "the sweep bar wicked through the 10.4 swing"
        );
        for e in sweeps {
            assert!(
                e.formed_at.unwrap() < e.bar_time,
                "a level formed on the sweep bar itself means the exclusion guard is gone"
            );
        }
    }

    #[test]
    fn a_bullish_fvg_opens_and_later_fills() {
        // Bar 3 gaps away from bar 1's high and never returns: a bullish FVG
        // of 2.0 over a middle-bar range of 11, comfortably over the fraction
        // guard. Its edges become the event's band.
        let mut candles: Vec<Candle> = (1..10)
            .map(|i| c(BASE - i as i64 * MIN, 97.5, 96.5, 97.0))
            .collect();
        candles.push(c(BASE, 100.0, 98.0, 99.0));
        candles.push(c(BASE + MIN, 110.0, 99.0, 105.0));
        candles.push(c(BASE + 2 * MIN, 109.0, 102.0, 106.0));
        let config = EventConfig::default();
        let events = detect_events(&candles, "BTCUSDT", &config);
        let created = events
            .iter()
            .find(|e| e.kind == EventKind::FvgCreated)
            .expect("the third bar opened a bullish gap");
        assert_eq!(created.side, Some(Side::Buy));
        assert_eq!(created.zone_low, Some(100.0));
        assert_eq!(created.zone_high, Some(102.0));

        // Now a later bar trades fully back through the gap: filled.
        let mut later = candles.clone();
        later.push(c(BASE + 10 * MIN, 103.0, 99.0, 99.5));
        let events = detect_events(&later, "BTCUSDT", &config);
        let filled = events
            .iter()
            .find(|e| e.kind == EventKind::FvgFilled)
            .expect("the last bar traded through the whole gap");
        assert_eq!(filled.zone_low, Some(100.0));
        assert_eq!(filled.zone_high, Some(102.0));
    }

    #[test]
    fn a_hairline_gap_is_not_created() {
        // The gap is real but a rounding error against the middle bar's range:
        // under the fraction guard, so no event.
        let mut pre: Vec<Candle> = (1..10)
            .map(|i| c(BASE - i as i64 * MIN, 97.5, 96.5, 97.0))
            .collect();
        pre.extend(vec![
            c(BASE, 100.0, 99.0, 99.5),
            c(BASE + MIN, 110.0, 98.5, 105.0),
            c(BASE + 2 * MIN, 106.0, 100.05, 105.5),
        ]);
        let events = detect_events(&pre, "BTCUSDT", &EventConfig::default());
        assert!(
            !events.iter().any(|e| e.kind == EventKind::FvgCreated),
            "a 0.05 gap over an 11.5 range is noise: {events:?}"
        );
    }

    #[test]
    fn a_volume_spike_fires_at_the_multiplier() {
        // Nineteen quiet bars, then one at five times the average.
        let mut candles: Vec<Candle> = (0..19)
            .map(|i| volume(c(BASE + i as i64 * MIN, 101.0, 99.0, 100.0), 1.0, 0.5))
            .collect();
        candles.push(volume(c(BASE + 19 * MIN, 102.0, 100.0, 101.5), 5.0, 4.5));
        let events = detect_events(&candles, "BTCUSDT", &EventConfig::default());
        let spike = events
            .iter()
            .find(|e| e.kind == EventKind::VolumeSpike)
            .expect("5x average is a spike");
        assert!(spike.magnitude.unwrap() >= 2.5);
        assert_eq!(spike.side, None, "a spike has no natural side");
    }

    #[test]
    fn a_delta_shift_names_its_side() {
        // Nineteen bars with a small consistent buy tilt, then one
        // overwhelmingly bought. The tilt must be nonzero: a zero average
        // |delta| has no ratio, and "no ratio" is not news.
        let mut candles: Vec<Candle> = (0..19)
            .map(|i| volume(c(BASE + i as i64 * MIN, 101.0, 99.0, 100.0), 1.0, 0.55))
            .collect();
        candles.push(volume(c(BASE + 19 * MIN, 102.0, 100.0, 101.5), 1.0, 0.95));
        let events = detect_events(&candles, "BTCUSDT", &EventConfig::default());
        let shift = events
            .iter()
            .find(|e| e.kind == EventKind::DeltaShift)
            .expect("9x the average |delta| is a shift");
        assert_eq!(shift.side, Some(Side::Buy));
    }

    #[test]
    fn a_bos_on_the_last_bar_is_reported_once() {
        // A zigzag whose every peak is 116.25, then a bar closing at 121: the
        // first close above a confirmed swing high, so exactly one break -- and
        // only the last bar's, because that is all a diff reports.
        let mut candles = zigzag(40);
        candles.push(c(BASE + 40 * MIN, 122.0, 115.0, 121.0));
        let config = EventConfig::default();
        let events = detect_events(&candles, "BTCUSDT", &config);
        let breaks: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Bos | EventKind::Choch))
            .collect();
        assert_eq!(
            breaks.len(),
            1,
            "only the last bar's break is news: {breaks:?}"
        );
        assert_eq!(breaks[0].bar_time, BASE + 40 * MIN);
        assert_eq!(
            breaks[0].price, 116.25,
            "the broken level is the swing high"
        );
    }

    #[test]
    fn a_replayed_window_ends_where_detection_begins() {
        // The contract the live loop depends on: the last slice of a replay is
        // exactly what the per-bar detector reports for that bar.
        let mut candles = zigzag(30);
        candles.push(c(BASE + 30 * MIN, 122.0, 115.0, 121.0));
        let config = EventConfig::default();
        let replayed = replay_events(&candles, "BTCUSDT", &config);
        let (_, last_slice) = replayed.last().expect("the final bar produced events");
        assert_eq!(
            last_slice,
            &detect_events(&candles, "BTCUSDT", &config),
            "the replay's last bar and the live detector must agree"
        );
    }

    #[test]
    fn a_degenerate_config_degrades_to_the_default() {
        // A config from the wire can carry anything; the detector must still
        // run, not divide by zero or loop forever.
        let hostile = EventConfig {
            structure_lookback: 0,
            spike_average_bars: 0,
            volume_multiplier: 0.0,
            delta_multiplier: f64::NAN,
            fvg_min_fraction: f64::INFINITY,
        }
        .sanitised();
        assert_eq!(hostile, EventConfig::default());
        // And with the hostile values in place the detector still walks the
        // whole series without panicking.
        let _ = detect_events(
            &zigzag(60),
            "BTCUSDT",
            &EventConfig {
                structure_lookback: 0,
                spike_average_bars: 0,
                volume_multiplier: 0.0,
                delta_multiplier: f64::NAN,
                fvg_min_fraction: f64::INFINITY,
            },
        );
    }

    #[test]
    fn every_kind_name_is_a_snake_case_wire_value() {
        for kind in EventKind::ALL {
            let json = serde_json::to_value(kind).expect("serializes");
            assert_eq!(json, kind.name(), "{kind:?} renamed on the wire");
        }
    }

    #[test]
    fn an_event_serializes_with_the_fields_the_lane_advertises() {
        let event = MarketEvent {
            kind: EventKind::LiquiditySweep,
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            bar_time: BASE,
            close: 104_100.5,
            price: 104_100.0,
            side: Some(Side::Sell),
            zone_low: None,
            zone_high: None,
            magnitude: Some(2.0),
            formed_at: Some(BASE - MIN),
        };
        let json = serde_json::to_value(&event).expect("serializes");
        assert_eq!(json["kind"], "liquidity_sweep");
        assert_eq!(json["symbol"], "BTCUSDT");
        assert_eq!(json["timeframe"], "5m");
        assert_eq!(json["side"], "Sell");
        assert_eq!(json["price"], 104_100.0);
        // Band fields absent when absent, so the wire carries no nulls to misread.
        assert!(json.get("zone_low").is_none());
        assert!(json.get("magnitude").is_some());
    }
}
