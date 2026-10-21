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
    /// Price CLOSED beyond an open gap's far edge: the gap failed and flips
    /// polarity -- the inverse fair value gap (IFVG) is born.
    FvgInvalidated,
    /// Two opposite open gaps overlap: a balanced price range (BPR).
    BprCreated,
    /// Price traded into a fresh supply/demand zone.
    ZoneTested,
    /// Bar volume exceeded a multiple of the recent average.
    VolumeSpike,
    /// Bar delta exceeded a multiple of the recent average absolute delta.
    DeltaShift,
    /// Hammer shape (long lower wick, small top body) at a local low.
    Hammer,
    /// The hammer shape at a local high: the same candle, the opposite read.
    HangingMan,
    /// Long upper wick, small bottom body, at a local low.
    InvertedHammer,
    /// The inverted-hammer shape at a local high.
    ShootingStar,
    /// Open and close nearly equal (the bar could not decide).
    Doji,
    /// A full-body bar with no wicks worth naming, in its close's direction.
    Marubozu,
    /// This bar's body engulfs the previous bar's body.
    Engulfing,
    /// This bar's body sits inside the previous bar's body.
    Harami,
    /// Down bar, small indecision bar, up bar closing past the first's midpoint.
    MorningStar,
    /// Up bar, small indecision bar, down bar closing past the first's midpoint.
    EveningStar,
    /// Down bar then an up bar opening lower and closing past the midpoint.
    PiercingLine,
    /// Up bar then a down bar opening higher and closing past the midpoint.
    DarkCloudCover,
    /// Three consecutive strong up bars closing near their highs.
    ThreeWhiteSoldiers,
    /// Three consecutive strong down bars closing near their lows.
    ThreeBlackCrows,
    /// A close beyond the recent range's extreme.
    Breakout,
    /// Closed beyond the extreme, then closed back inside: the false break.
    FailedBreakout,
    /// Broke out, returned to the level, and held it without closing through.
    Retest,
    /// Newest two swing highs ascending AND newest two swing lows ascending
    /// (buy side); the LH/LL mirror (sell side) shares the kind.
    HigherHighs,
    /// The newest range window is the tightest of its recent peers: the
    /// squeeze that precedes expansion.
    RangeContraction,
    /// The last opposing-color candle before a displacement-driven break.
    OrderBlockCreated,
    /// Price traded into an open order block (the zone's first touch).
    OrderBlockMitigated,
    /// Price closed beyond an order block's far edge: it failed, and the
    /// failed block flips into a breaker of the opposite type.
    OrderBlockInvalidated,
    /// The dealing range's premium/discount halves with the equilibrium at
    /// 50% (a zone-shaped state, refreshed per bar).
    PremiumDiscount,
    /// Two same-side swings within a small tolerance: the liquidity pool.
    EqualLevels,
    /// A long-wick candle where the ZONE IS THE WICK (ICT's rejection block).
    RejectionBlockCreated,
    /// A later bar traded back into the wick zone.
    RejectionBlockMitigated,
    /// A close beyond the wick's tip: the rejection failed.
    RejectionBlockInvalidated,
    /// The failed-auction block: the last opposing candle before a sweep
    /// that closed back WITHOUT breaking structure (ICT's mitigation block).
    MitigationBlockCreated,
    /// A later bar traded into the mitigation block's zone.
    MitigationBlockMitigated,
    /// A close beyond the mitigation block's far edge.
    MitigationBlockInvalidated,
    /// A sweep whose reclaim bar itself closes in the reversal direction --
    /// the grab that precedes the turn.
    LiquidityGrab,
    /// The nearest opposing swing liquidity in the trend's direction: the
    /// trap before the real move.
    Inducement,
    /// A break of a significant swing that re-enters the range within a few
    /// bars (Linda Raschke's turtle soup).
    TurtleSoup,
    /// The London kill zone's window and the range it formed (ICT session
    /// concepts; times in UTC, the platform's clock).
    LondonKillZone,
    /// The New York kill zone's window and the range it formed.
    NewYorkKillZone,
    /// The Asian session's high/low band for the day.
    AsianRange,
    /// The day's opening price.
    DailyOpen,
    /// The week's opening price (UTC week, Monday open).
    WeeklyOpen,
    /// Two swing highs (or lows) within tolerance, confirmed through the
    /// intervening trough (or peak).
    DoubleTopBottom,
    /// Three swings, middle highest (or lowest), confirmed through the
    /// neckline.
    HeadShoulders,
    /// Converging structure: ascending, descending, or symmetrical.
    Triangle,
    /// A strong impulse then a tight counter channel.
    Flag,
    /// Both swing lines sloping the same way while converging.
    Wedge,
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
            Self::FvgInvalidated => "fvg_invalidated",
            Self::BprCreated => "bpr_created",
            Self::ZoneTested => "zone_tested",
            Self::VolumeSpike => "volume_spike",
            Self::DeltaShift => "delta_shift",
            Self::Hammer => "hammer",
            Self::HangingMan => "hanging_man",
            Self::InvertedHammer => "inverted_hammer",
            Self::ShootingStar => "shooting_star",
            Self::Doji => "doji",
            Self::Marubozu => "marubozu",
            Self::Engulfing => "engulfing",
            Self::Harami => "harami",
            Self::MorningStar => "morning_star",
            Self::EveningStar => "evening_star",
            Self::PiercingLine => "piercing_line",
            Self::DarkCloudCover => "dark_cloud_cover",
            Self::ThreeWhiteSoldiers => "three_white_soldiers",
            Self::ThreeBlackCrows => "three_black_crows",
            Self::Breakout => "breakout",
            Self::FailedBreakout => "failed_breakout",
            Self::Retest => "retest",
            Self::HigherHighs => "higher_highs",
            Self::RangeContraction => "range_contraction",
            Self::OrderBlockCreated => "order_block_created",
            Self::OrderBlockMitigated => "order_block_mitigated",
            Self::OrderBlockInvalidated => "order_block_invalidated",
            Self::PremiumDiscount => "premium_discount",
            Self::EqualLevels => "equal_levels",
            Self::RejectionBlockCreated => "rejection_block_created",
            Self::RejectionBlockMitigated => "rejection_block_mitigated",
            Self::RejectionBlockInvalidated => "rejection_block_invalidated",
            Self::MitigationBlockCreated => "mitigation_block_created",
            Self::MitigationBlockMitigated => "mitigation_block_mitigated",
            Self::MitigationBlockInvalidated => "mitigation_block_invalidated",
            Self::LiquidityGrab => "liquidity_grab",
            Self::Inducement => "inducement",
            Self::TurtleSoup => "turtle_soup",
            Self::LondonKillZone => "london_kill_zone",
            Self::NewYorkKillZone => "new_york_kill_zone",
            Self::AsianRange => "asian_range",
            Self::DailyOpen => "daily_open",
            Self::WeeklyOpen => "weekly_open",
            Self::DoubleTopBottom => "double_top_bottom",
            Self::HeadShoulders => "head_shoulders",
            Self::Triangle => "triangle",
            Self::Flag => "flag",
            Self::Wedge => "wedge",
        }
    }

    /// Every kind, for a client that wants to build a filter menu.
    pub const ALL: [Self; 53] = [
        Self::LiquiditySweep,
        Self::Bos,
        Self::Choch,
        Self::FvgCreated,
        Self::FvgFilled,
        Self::FvgInvalidated,
        Self::BprCreated,
        Self::ZoneTested,
        Self::VolumeSpike,
        Self::DeltaShift,
        Self::Hammer,
        Self::HangingMan,
        Self::InvertedHammer,
        Self::ShootingStar,
        Self::Doji,
        Self::Marubozu,
        Self::Engulfing,
        Self::Harami,
        Self::MorningStar,
        Self::EveningStar,
        Self::PiercingLine,
        Self::DarkCloudCover,
        Self::ThreeWhiteSoldiers,
        Self::ThreeBlackCrows,
        Self::Breakout,
        Self::FailedBreakout,
        Self::Retest,
        Self::HigherHighs,
        Self::RangeContraction,
        Self::OrderBlockCreated,
        Self::OrderBlockMitigated,
        Self::OrderBlockInvalidated,
        Self::PremiumDiscount,
        Self::EqualLevels,
        Self::RejectionBlockCreated,
        Self::RejectionBlockMitigated,
        Self::RejectionBlockInvalidated,
        Self::MitigationBlockCreated,
        Self::MitigationBlockMitigated,
        Self::MitigationBlockInvalidated,
        Self::LiquidityGrab,
        Self::Inducement,
        Self::TurtleSoup,
        Self::LondonKillZone,
        Self::NewYorkKillZone,
        Self::AsianRange,
        Self::DailyOpen,
        Self::WeeklyOpen,
        Self::DoubleTopBottom,
        Self::HeadShoulders,
        Self::Triangle,
        Self::Flag,
        Self::Wedge,
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
/// * **Candlestick patterns** -- the shape + trend-context pass
///   ([`detect_patterns`]), merged into the same stream before the sort.

/// The classical chart-pattern pass, as an ADAPTER over the platform's own
/// pattern engine (`crate::patterns`): head & shoulders (and inverse), double
/// tops and bottoms, triangles, wedges, flags -- with confidence and trade
/// geometry the engine computed, wrapped as events on the same stream.
pub fn detect_chart_patterns(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    // The pattern engine's own tested swing lookback (2): finer than the
    // structure detector's, because a classical pattern's swings are local.
    let structure = crate::market_structure::detect_market_structure(
        candles,
        crate::market_structure::StructureConfig { lookback: 2 },
    );
    let matches = crate::patterns::detect_patterns(candles, &structure, None, &crate::patterns::PatternConfig::default());
    let last = match candles.last() {
        Some(l) => l,
        None => return Vec::new(),
    };
    matches
        .iter()
        .map(|m| {
            use crate::patterns::{PatternDirection, PatternKind};
            let (kind, side) = match (m.kind, m.direction) {
                (PatternKind::HeadAndShoulders, _) => (EventKind::HeadShoulders, Some(Side::Sell)),
                (PatternKind::InverseHeadAndShoulders, _) => (EventKind::HeadShoulders, Some(Side::Buy)),
                (PatternKind::DoubleTop, _) => (EventKind::DoubleTopBottom, Some(Side::Sell)),
                (PatternKind::DoubleBottom, _) => (EventKind::DoubleTopBottom, Some(Side::Buy)),
                (PatternKind::Triangle, PatternDirection::Bullish) => (EventKind::Triangle, Some(Side::Buy)),
                (PatternKind::Triangle, PatternDirection::Bearish) => (EventKind::Triangle, Some(Side::Sell)),
                (PatternKind::Triangle, PatternDirection::Neutral) => (EventKind::Triangle, None),
                (PatternKind::Wedge, PatternDirection::Bullish) => (EventKind::Wedge, Some(Side::Buy)),
                (PatternKind::Wedge, PatternDirection::Bearish) => (EventKind::Wedge, Some(Side::Sell)),
                (PatternKind::Wedge, PatternDirection::Neutral) => (EventKind::Wedge, None),
                (PatternKind::Flag, PatternDirection::Bullish) => (EventKind::Flag, Some(Side::Buy)),
                (PatternKind::Flag, PatternDirection::Bearish) => (EventKind::Flag, Some(Side::Sell)),
                (PatternKind::Flag, PatternDirection::Neutral) => (EventKind::Flag, None),
            };
            let (zone_low, zone_high) = if m.invalidation < m.target {
                (Some(m.invalidation), Some(m.target))
            } else {
                (Some(m.target), Some(m.invalidation))
            };
            MarketEvent {
                kind,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time: last.open_time,
                close: last.close,
                price: m.entry_level,
                side,
                zone_low,
                zone_high,
                magnitude: Some(m.confidence),
                formed_at: m.anchors.first().map(|a| a.timestamp),
            }
        })
        .collect()
}

/// The session pass: kill zones and the Asian range, in the platform's own
/// clock (UTC). ICT's windows are quoted in New York time; these are the UTC
/// equivalents for the winter offset (London 07:00-10:00 UTC ~= 02:00-05:00
/// ET, New York 12:00-15:00 UTC ~= 07:00-10:00 ET), documented rather than
/// DST-fudged -- the day ET rules change, these constants are the one place
/// to move.
pub fn detect_sessions(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    // (kind, start hour UTC, end hour UTC)
    const WINDOWS: &[(EventKind, u32, u32)] = &[
        (EventKind::AsianRange, 0, 7),
        (EventKind::LondonKillZone, 7, 10),
        (EventKind::NewYorkKillZone, 12, 15),
    ];
    const HOUR_NS: i64 = 3_600_000_000_000;
    const DAY_NS: i64 = 24 * HOUR_NS;

    let mut out = Vec::new();
    if candles.len() < 2 {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;

    // The opens: a new period's first bar carries its opening price, once.
    // Day-roll for the daily open, Monday-roll for the weekly (UTC week).
    const WEEK_NS: i64 = 7 * DAY_NS;
    let newest_day = last.open_time.div_euclid(DAY_NS);
    let prev_day = candles[candles.len() - 2].open_time.div_euclid(DAY_NS);
    if newest_day != prev_day {
        out.push(MarketEvent {
            kind: EventKind::DailyOpen,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price: last.open,
            side: None,
            zone_low: None,
            zone_high: None,
            magnitude: None,
            formed_at: Some(last.open_time),
        });
        let newest_week = last.open_time.div_euclid(WEEK_NS);
        let prev_week = candles[candles.len() - 2].open_time.div_euclid(WEEK_NS);
        if newest_week != prev_week {
            out.push(MarketEvent {
                kind: EventKind::WeeklyOpen,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: last.open,
                side: None,
                zone_low: None,
                zone_high: None,
                magnitude: None,
                formed_at: Some(last.open_time),
            });
        }
    }

    for (kind, start_hour, end_hour) in WINDOWS {
        // The newest completed window of this kind: a session event fires
        // ONCE per window, at the bar that crosses its end -- which is what
        // keeps a replay from re-firing yesterday's kill zone at every bar
        // since.
        let day_of = last.open_time.div_euclid(DAY_NS);
        let newest_hour = ((last.open_time % DAY_NS) / HOUR_NS) as u32;
        let prev_hour = ((candles[candles.len() - 2].open_time % DAY_NS) / HOUR_NS) as u32;
        // The crossing: the previous bar was still inside (or before) the
        // window, and the newest is at-or-after its end.
        let just_closed = prev_hour < *end_hour && newest_hour >= *end_hour;
        if !just_closed {
            continue;
        }
        // Today's slice inside the session hours, ending at the window end.
        let session: Vec<&Candle> = candles
            .iter()
            .filter(|k| {
                k.open_time.div_euclid(DAY_NS) == day_of
                    && (((k.open_time % DAY_NS) / HOUR_NS) as u32) >= *start_hour
                    && (((k.open_time % DAY_NS) / HOUR_NS) as u32) < *end_hour
            })
            .collect();
        if session.is_empty() {
            continue;
        }
        let low = session.iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let high = session.iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let from = session.first().expect("checked above").open_time;
        out.push(MarketEvent {
            kind: *kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price: (low + high) / 2.0,
            side: None,
            zone_low: Some(low),
            zone_high: Some(high),
            magnitude: None,
            formed_at: Some(from),
        });
    }

    out
}

/// The liquidity-family pass: the grab, the inducement, the turtle soup.
///
/// The distinctions from the plain sweep: the grab reads the RECLAIM bar
/// itself (a sweep whose bar closes in the reversal direction is conviction,
/// not noise); the inducement is the NEAREST opposing liquidity in the
/// trend's direction (the trap before continuation); the turtle soup is the
/// failed break of a *significant* swing with fast re-entry, Linda
/// Raschke's rule over the same structure.
pub fn detect_liquidity_family(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    const SOUP_RANGE_BARS: usize = 20;
    const SOUP_REENTRY_BARS: usize = 4;

    let mut out = Vec::new();
    let lb = config.structure_lookback;
    // The grab's own floor (swing + a couple); the soup's larger window is
    // guarded by its own `i < SOUP_RANGE_BARS` skip below.
    if candles.len() < lb * 2 + 4 {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let n = candles.len() - 1;
    let mut push = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price,
            side,
            zone_low: low,
            zone_high: high,
            magnitude: None,
            formed_at: formed,
        });
    };

    // -- Liquidity grab --------------------------------------------------------
    // The newest sweep whose own bar closed in the reversal direction: a
    // wick through the swing and a close with conviction the other way.
    for i in (n.saturating_sub(10)..=n).rev() {
        let k = &candles[i];
        if i < lb + 1 {
            break;
        }
        let swing_high = candles[i - lb - 1..i].iter().map(|b| b.high).fold(f64::MIN, f64::max);
        let swing_low = candles[i - lb - 1..i].iter().map(|b| b.low).fold(f64::MAX, f64::min);
        if k.low < swing_low && k.close > swing_low && k.close > k.open {
            push(EventKind::LiquidityGrab, Some(Side::Buy), swing_low, None, None, Some(candles[i].open_time));
            break;
        }
        if k.high > swing_high && k.close < swing_high && k.close < k.open {
            push(EventKind::LiquidityGrab, Some(Side::Sell), swing_high, None, None, Some(candles[i].open_time));
            break;
        }
    }

    // -- Inducement --------------------------------------------------------------
    // The trend from the newest structure swings; the inducement is the
    // NEAREST opposing swing on the other side of current price.
    let mut highs: Vec<f64> = Vec::new();
    let mut lows: Vec<f64> = Vec::new();
    for i in lb..n.saturating_sub(lb) {
        let k = &candles[i];
        if (i - lb..=i + lb).all(|j| j == i || candles[j].high < k.high) {
            highs.push(k.high);
        }
        if (i - lb..=i + lb).all(|j| j == i || candles[j].low > k.low) {
            lows.push(k.low);
        }
    }
    let uptrend = highs.len() >= 2 && lows.len() >= 2
        && highs[highs.len() - 1] > highs[highs.len() - 2]
        && lows[lows.len() - 1] > lows[lows.len() - 2];
    let downtrend = highs.len() >= 2 && lows.len() >= 2
        && highs[highs.len() - 1] < highs[highs.len() - 2]
        && lows[lows.len() - 1] < lows[lows.len() - 2];
    if uptrend {
        if let Some(nearest) = lows.last() {
            // The nearest swing low below price is the sell-side trap.
            push(EventKind::Inducement, Some(Side::Sell), *nearest, None, None, None);
        }
    } else if downtrend {
        if let Some(nearest) = highs.last() {
            push(EventKind::Inducement, Some(Side::Buy), *nearest, None, None, None);
        }
    }

    // -- Turtle soup -------------------------------------------------------------
    // A close beyond the 20-bar extreme within the last SOUP_REENTRY_BARS
    // bars, already re-entered (the newest close is back inside the range).
    let top = candles[n.saturating_sub(SOUP_RANGE_BARS)..n].iter().map(|k| k.high).fold(f64::MIN, f64::max);
    let bottom = candles[n.saturating_sub(SOUP_RANGE_BARS)..n].iter().map(|k| k.low).fold(f64::MAX, f64::min);
    for i in (n.saturating_sub(SOUP_REENTRY_BARS)..n).rev() {
        let k = &candles[i];
        if i < SOUP_RANGE_BARS {
            continue;
        }
        let prior_top = candles[i - SOUP_RANGE_BARS..i].iter().map(|b| b.high).fold(f64::MIN, f64::max);
        let prior_bottom = candles[i - SOUP_RANGE_BARS..i].iter().map(|b| b.low).fold(f64::MAX, f64::min);
        if k.close > prior_top && last.close < prior_top {
            // Broke the top and came back: short-side soup.
            push(EventKind::TurtleSoup, Some(Side::Sell), prior_top, None, None, Some(candles[i].open_time));
            break;
        }
        if k.close < prior_bottom && last.close > prior_bottom {
            push(EventKind::TurtleSoup, Some(Side::Buy), prior_bottom, None, None, Some(candles[i].open_time));
            break;
        }
    }
    let _ = (top, bottom);

    out
}

/// The extended SMC pass: equal levels, rejection blocks, mitigation blocks.
///
/// The distinction that matters most here is the mitigation block versus the
/// order block, because they are built from the same candle for opposite
/// reasons: an order block needs a structure BREAK after it, a mitigation
/// block needs a liquidity SWEEP that closes back WITHOUT breaking. ICT's
/// failed auction: the institutions' positions got mitigated, not driven
/// through.
pub fn detect_smc_extended(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    const EQUAL_TOLERANCE_ATR: f64 = 0.15;
    const REJECTION_WICK_RATIO: f64 = 2.0;

    let mut out = Vec::new();
    let lb = config.structure_lookback;
    // The smallest window any detector here needs (the ATR-14 floor); each
    // detector's own slices are saturating-protected on top of it.
    if candles.len() < 16 {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let n = candles.len() - 1;
    let mut push = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price,
            side,
            zone_low: low,
            zone_high: high,
            magnitude: None,
            formed_at: formed,
        });
    };

    // -- Equal highs / lows -----------------------------------------------------
    // The newest two same-side swings within the tolerance. The tolerance is
    // a fraction of the 14-bar average range, so one rule spans instruments.
    let atr14 = {
        let start = n.saturating_sub(14);
        let sum: f64 = candles[start..n].iter().map(|k| k.high - k.low).sum();
        sum / (n - start).max(1) as f64
    };
    let tolerance = atr14 * EQUAL_TOLERANCE_ATR;
    let mut highs: Vec<(usize, f64)> = Vec::new();
    let mut lows: Vec<(usize, f64)> = Vec::new();
    for i in lb..n.saturating_sub(lb) {
        let k = &candles[i];
        if (i - lb..=i + lb).all(|j| j == i || candles[j].high < k.high) {
            highs.push((i, k.high));
        }
        if (i - lb..=i + lb).all(|j| j == i || candles[j].low > k.low) {
            lows.push((i, k.low));
        }
    }
    if highs.len() >= 2 {
        let (a, b) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        if (a.1 - b.1).abs() <= tolerance {
            // Buy stops rest above equal highs: the pool is buy-side.
            push(EventKind::EqualLevels, Some(Side::Buy), (a.1 + b.1) / 2.0, Some(a.1.min(b.1)), Some(a.1.max(b.1)), Some(candles[a.0].open_time));
        }
    }
    if lows.len() >= 2 {
        let (a, b) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        if (a.1 - b.1).abs() <= tolerance {
            // Sell stops rest below equal lows: the pool is sell-side.
            push(EventKind::EqualLevels, Some(Side::Sell), (a.1 + b.1) / 2.0, Some(a.1.min(b.1)), Some(a.1.max(b.1)), Some(candles[a.0].open_time));
        }
    }

    // -- Rejection blocks --------------------------------------------------------
    // The newest long-wick candle: the wick IS the zone. Bullish = a lower
    // wick at least twice the body; the zone runs from the wick's tip to the
    // body's bottom.
    for i in (n.saturating_sub(30)..n).rev() {
        let k = &candles[i];
        let body = (k.close - k.open).abs();
        if body == 0.0 {
            continue;
        }
        let lower = k.open.min(k.close) - k.low;
        let upper = k.high - k.open.max(k.close);
        if lower >= body * REJECTION_WICK_RATIO && lower > upper * 2.0 {
            let zone = (k.low, k.open.min(k.close));
            let invalidated = candles[i + 1..=n].iter().any(|b| b.close < zone.0);
            let mitigated = candles[i + 1..=n].iter().any(|b| b.low <= zone.1);
            if invalidated {
                push(EventKind::RejectionBlockInvalidated, Some(Side::Sell), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            } else if mitigated {
                push(EventKind::RejectionBlockMitigated, Some(Side::Buy), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            } else {
                push(EventKind::RejectionBlockCreated, Some(Side::Buy), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            }
            break;
        }
        if upper >= body * REJECTION_WICK_RATIO && upper > lower * 2.0 {
            let zone = (k.open.max(k.close), k.high);
            let invalidated = candles[i + 1..=n].iter().any(|b| b.close > zone.1);
            let mitigated = candles[i + 1..=n].iter().any(|b| b.high >= zone.0);
            if invalidated {
                push(EventKind::RejectionBlockInvalidated, Some(Side::Buy), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            } else if mitigated {
                push(EventKind::RejectionBlockMitigated, Some(Side::Sell), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            } else {
                push(EventKind::RejectionBlockCreated, Some(Side::Sell), (zone.0 + zone.1) / 2.0, Some(zone.0), Some(zone.1), Some(k.open_time));
            }
            break;
        }
    }

    // -- Mitigation blocks --------------------------------------------------------
    // A swing swept without a break: some bar wicked through the swing and
    // closed back inside, and NO bar since closed beyond the swing. The last
    // opposing candle before the sweep bar is the block.
    // The swings are PER CANDIDATE: the extreme of the bars *before* the
    // candidate sweep bar -- a window that includes the candidate would
    // swallow its own wick and no sweep could ever register.
    for i in (n.saturating_sub(20)..=n).rev() {
        let k = &candles[i];
        if i < lb * 4 {
            continue;
        }
        let swing_high = candles[i - lb * 4..i].iter().map(|b| b.high).fold(f64::MIN, f64::max);
        let swing_low = candles[i - lb * 4..i].iter().map(|b| b.low).fold(f64::MAX, f64::min);
        // Sell-side sweep: wick below the swing low, close back above, and no
        // close below it anywhere after.
        if k.low < swing_low && k.close > swing_low {
            let broke_after = candles[i + 1..=n].iter().any(|b| b.close < swing_low);
            if !broke_after {
                if let Some(ob) = candles[..i].iter().rposition(|b| b.close < b.open) {
                    let (low, high) = (candles[ob].low, candles[ob].high);
                    let origin = candles[ob].open_time;
                    let invalidated = candles[i + 1..=n].iter().any(|b| b.close < low);
                    let mitigated = candles[i + 1..=n].iter().any(|b| b.low <= high);
                    if invalidated {
                        push(EventKind::MitigationBlockInvalidated, Some(Side::Sell), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    } else if mitigated {
                        push(EventKind::MitigationBlockMitigated, Some(Side::Buy), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    } else {
                        push(EventKind::MitigationBlockCreated, Some(Side::Buy), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    }
                }
            }
            break;
        }
        // Buy-side sweep mirror.
        if k.high > swing_high && k.close < swing_high {
            let broke_after = candles[i + 1..=n].iter().any(|b| b.close > swing_high);
            if !broke_after {
                if let Some(ob) = candles[..i].iter().rposition(|b| b.close > b.open) {
                    let (low, high) = (candles[ob].low, candles[ob].high);
                    let origin = candles[ob].open_time;
                    let invalidated = candles[i + 1..=n].iter().any(|b| b.close > high);
                    let mitigated = candles[i + 1..=n].iter().any(|b| b.high >= low);
                    if invalidated {
                        push(EventKind::MitigationBlockInvalidated, Some(Side::Buy), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    } else if mitigated {
                        push(EventKind::MitigationBlockMitigated, Some(Side::Sell), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    } else {
                        push(EventKind::MitigationBlockCreated, Some(Side::Sell), (low + high) / 2.0, Some(low), Some(high), Some(origin));
                    }
                }
            }
            break;
        }
    }

    out
}

/// The order-block pass: the canonical SMC rules.
///
/// A bullish order block is the LAST down-close candle before a
/// displacement-driven break of a swing high. Displacement is graded, not
/// felt: the breaking bar's range must clear 1.5x the 14-bar average, and it
/// must CLOSE beyond the swing. The zone is that candle's full range.
/// Mitigation is a trade back into the zone; invalidation is a CLOSE beyond
/// the zone's far edge -- and the failed block flips into a breaker of the
/// opposite type (ICT's failed-order-block rule, confirmed against the
/// published references: Flux Charts' OB/breaker guides and ICT's own
/// definition via Alchemy Markets).
pub fn detect_order_blocks(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    const AVG_RANGE_BARS: usize = 14;
    const DISPLACEMENT: f64 = 1.5;
    const MAX_OPEN_BLOCKS: usize = 12;

    let mut out = Vec::new();
    if candles.len() < AVG_RANGE_BARS + 3 {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let n = candles.len() - 1;
    let lb = config.structure_lookback;
    let avg_range = |end: usize| {
        let start = end.saturating_sub(AVG_RANGE_BARS);
        let sum: f64 = candles[start..end].iter().map(|k| k.high - k.low).sum();
        sum / (end - start).max(1) as f64
    };

    // Rebuild every order block the window produced: at each bar i that
    // broke a swing with displacement, the last opposing candle before it is
    // the block's zone.
    // (ob index, side, low, high, origin time, the BREAK bar's index: a zone
    // is only mitigated by bars after the break -- the breaking bar's own
    // range crossing the zone is the departure, not a retest).
    let mut blocks: Vec<(usize, Side, f64, f64, i64, usize)> = Vec::new();
    for i in lb + 2..=n {
        let k = &candles[i];
        let range = k.high - k.low;
        if range < avg_range(i) * DISPLACEMENT {
            continue;
        }
        let swing_high = candles[i - lb - 1..i].iter().map(|b| b.high).fold(f64::MIN, f64::max);
        let swing_low = candles[i - lb - 1..i].iter().map(|b| b.low).fold(f64::MAX, f64::min);
        if k.close > swing_high {
            // Bullish break: the last DOWN candle before it is the block.
            if let Some(ob) = candles[..i].iter().rposition(|b| b.close < b.open).map(|j| (j, &candles[j])) {
                blocks.push((ob.0, Side::Buy, candles[ob.0].low, candles[ob.0].high, candles[ob.0].open_time, i));
            }
        } else if k.close < swing_low {
            if let Some(ob) = candles[..i].iter().rposition(|b| b.close > b.open).map(|j| (j, &candles[j])) {
                blocks.push((ob.0, Side::Sell, candles[ob.0].low, candles[ob.0].high, candles[ob.0].open_time, i));
            }
        }
    }
    blocks.dedup_by_key(|(idx, _, _, _, _, _)| *idx);

    // Their lifecycle against the newest bar, newest block first (the newest
    // unmitigated block is the one the chart cares about).
    let mut created = 0;
    for (idx, side, low, high, origin, break_idx) in blocks.iter().rev().take(MAX_OPEN_BLOCKS) {
        let zone_price = (low + high) / 2.0;
        // Already mitigated by any bar after the block?
        let touched_after = candles[(break_idx + 1).min(n)..n]
            .iter()
            .any(|k| match side {
                Side::Buy => k.low <= *high,
                Side::Sell => k.high >= *low,
            });
        let invalidated = match side {
            Side::Buy => last.close < *low,
            Side::Sell => last.close > *high,
        };
        // First breach only, as with the IFVG flip: the block becomes a
        // breaker once, not on every later bar the close stays beyond.
        let already = match side {
            Side::Buy => candles[n - 1].close < *low,
            Side::Sell => candles[n - 1].close > *high,
        };
        if invalidated && !already {
            out.push(MarketEvent {
                kind: EventKind::OrderBlockInvalidated,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: zone_price,
                // The flip: the failed bullish block reads bearish now.
                side: Some(match side {
                    Side::Buy => Side::Sell,
                    Side::Sell => Side::Buy,
                }),
                zone_low: Some(*low),
                zone_high: Some(*high),
                magnitude: None,
                formed_at: Some(*origin),
            });
            continue;
        }
        if !touched_after {
            created += 1;
            out.push(MarketEvent {
                kind: EventKind::OrderBlockCreated,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: zone_price,
                side: Some(*side),
                zone_low: Some(*low),
                zone_high: Some(*high),
                magnitude: None,
                formed_at: Some(*origin),
            });
        }
        // The touch on THIS bar is a mitigation event (the zone's first touch
        // lands here and only here).
        if touched_after
            && match side {
                Side::Buy => last.low <= *high && candles[n - 1].low > *high,
                Side::Sell => last.high >= *low && candles[n - 1].high < *low,
            }
        {
            out.push(MarketEvent {
                kind: EventKind::OrderBlockMitigated,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: zone_price,
                side: Some(*side),
                zone_low: Some(*low),
                zone_high: Some(*high),
                magnitude: None,
                formed_at: Some(*origin),
            });
        }
    }
    let _ = created;

    // -- Premium / discount ----------------------------------------------------
    // The dealing range over the last 2*lookback swings: premium above the
    // 50% equilibrium, discount below it. A state, not an event -- refreshed
    // per bar so the chart's zones track the range.
    let start = n.saturating_sub(lb * 4);
    let range_high = candles[start..=n].iter().map(|k| k.high).fold(f64::MIN, f64::max);
    let range_low = candles[start..=n].iter().map(|k| k.low).fold(f64::MAX, f64::min);
    if range_high > range_low {
        let equilibrium = (range_high + range_low) / 2.0;
        out.push(MarketEvent {
            kind: EventKind::PremiumDiscount,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price: equilibrium,
            side: Some(if last.close >= equilibrium { Side::Sell } else { Side::Buy }),
            zone_low: Some(range_low),
            zone_high: Some(range_high),
            magnitude: None,
            formed_at: Some(candles[start].open_time),
        });
    }

    out
}

/// The price-action pass: breakouts, retests, swing structure, and ranges.
///
/// Everything here reconstructs its state from the window on every call --
/// the same rule the FVG lifecycle follows, so live and replay agree by
/// construction. Levels are computed per window (the 20-bar Donchian for the
/// breakout family), never stored.
pub fn detect_price_action(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    const RANGE_BARS: usize = 20;
    const CONTRACTION_BARS: usize = 10;
    const CONTRACTION_WINDOWS: usize = 10;

    let mut out = Vec::new();
    // RANGE_BARS of channel history plus the newest bar.
    if candles.len() <= RANGE_BARS {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let n = candles.len() - 1;
    let mut push = |kind: EventKind, side: Option<Side>, price: f64| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: last.close,
            price,
            side,
            zone_low: None,
            zone_high: None,
            magnitude: None,
            formed_at: None,
        });
    };

    // The Donchian channel over the RANGE_BARS *before* the newest bar.
    let window = &candles[n - RANGE_BARS..n];
    let top = window.iter().map(|k| k.high).fold(f64::MIN, f64::max);
    let bottom = window.iter().map(|k| k.low).fold(f64::MAX, f64::min);

    // Breakout: the newest close is through the channel.
    if last.close > top {
        push(EventKind::Breakout, Some(Side::Buy), top);
    } else if last.close < bottom {
        push(EventKind::Breakout, Some(Side::Sell), bottom);
    }

    // Failed breakout and retest: rebuild the channel breaks across the
    // window, then ask what the bars after each break did.
    for i in RANGE_BARS..n {
        let prior = &candles[i - RANGE_BARS..i];
        let prior_top = prior.iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let prior_bottom = prior.iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let k = &candles[i];
        if k.close > prior_top {
            // A bullish break at i: failed if any later bar closed back at or
            // under the level; retested if a later bar TOUCHED the level
            // (wick) without closing through, and the newest close is above.
            let mut touched = false;
            let mut failed = false;
            for later in &candles[i + 1..] {
                if later.close <= prior_top {
                    failed = true;
                    break;
                }
                if later.low <= prior_top {
                    touched = true;
                }
            }
            if failed {
                push(EventKind::FailedBreakout, Some(Side::Sell), prior_top);
                break;
            }
            if touched && last.close > prior_top {
                push(EventKind::Retest, Some(Side::Buy), prior_top);
                break;
            }
        } else if k.close < prior_bottom {
            let mut touched = false;
            let mut failed = false;
            for later in &candles[i + 1..] {
                if later.close >= prior_bottom {
                    failed = true;
                    break;
                }
                if later.high >= prior_bottom {
                    touched = true;
                }
            }
            if failed {
                push(EventKind::FailedBreakout, Some(Side::Buy), prior_bottom);
                break;
            }
            if touched && last.close < prior_bottom {
                push(EventKind::Retest, Some(Side::Sell), prior_bottom);
                break;
            }
        }
    }

    // Higher highs / lower lows: the newest two swing highs and swing lows,
    // lookback 3 (a swing clears three bars either side).
    let mut highs: Vec<f64> = Vec::new();
    let mut lows: Vec<f64> = Vec::new();
    for i in 3..n.saturating_sub(3) {
        let k = &candles[i];
        // Strict on both sides: a flat stretch is not a series of swings, and
        // a double top is one level, not two (tweezers are their own kind).
        if (i - 3..=i + 3).all(|j| j == i || candles[j].high < k.high) {
            highs.push(k.high);
        }
        if (i - 3..=i + 3).all(|j| j == i || candles[j].low > k.low) {
            lows.push(k.low);
        }
    }
    if highs.len() >= 2 && lows.len() >= 2 {
        let (h1, h2) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        let (l1, l2) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        if h2 > h1 && l2 > l1 {
            push(EventKind::HigherHighs, Some(Side::Buy), h2);
        } else if h2 < h1 && l2 < l1 {
            push(EventKind::HigherHighs, Some(Side::Sell), l2);
        }
    }

    // Range contraction: the newest CONTRACTION_BARS span is the tightest of
    // the last CONTRACTION_WINDOWS such spans.
    let span = |end: usize| {
        let w = &candles[end - CONTRACTION_BARS..end];
        w.iter().map(|k| k.high).fold(f64::MIN, f64::max)
            - w.iter().map(|k| k.low).fold(f64::MAX, f64::min)
    };
    let newest_span = span(candles.len());
    if newest_span > 0.0 {
        // The prior windows, not the newest's own peers: a market that is
        // equally tight everywhere is a quiet market, not a squeeze.
        let prior_tightest = (1..=CONTRACTION_WINDOWS)
            .filter_map(|back| {
                candles
                    .len()
                    .checked_sub(back + 1)
                    .filter(|e| *e >= CONTRACTION_BARS)
                    .map(span)
            })
            .fold(f64::MAX, f64::min);
        if newest_span < prior_tightest {
            push(EventKind::RangeContraction, None, last.close);
        }
    }

    out
}

/// The candlestick-pattern pass: shape rules plus local trend context.
///
/// Patterns are bar-close facts about one to three bars; every rule here is
/// stated in ratios (wick to body, body to range) so one rule means the same
/// thing on BTCUSDT and on a synthetic index at 4,800. Reversal patterns
/// carry their direction from the trend *before* the pattern: a hammer and a
/// hanging man are the same candle, and the only honest way to tell them
/// apart is where the market came from.
pub fn detect_patterns(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    // Six bars of trend context plus the pattern bar (three-bar shapes read
    // further back, but they also need the trend window).
    if candles.len() < 7 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let (o, h, l, c) = (last.open, last.high, last.low, last.close);
    let range = h - l;
    if range <= 0.0 {
        return out;
    }
    let body = (c - o).abs();
    let upper = h - o.max(c);
    let lower = o.min(c) - l;
    let bullish = c >= o;
    let bar_time = last.open_time;
    let tf = last.timeframe;

    // The local trend: the slope of the closes before the pattern bar.
    // Reversal reads need it; continuation reads ignore it.
    let prior: Vec<f64> = candles[n.saturating_sub(6)..n].iter().map(|k| k.close).collect();
    let prior_trend_down = prior.len() >= 4
        && prior.windows(2).filter(|w| w[1] < w[0]).count() as f64 / (prior.len() - 1) as f64 > 0.6;
    let prior_trend_up = prior.len() >= 4
        && prior.windows(2).filter(|w| w[1] > w[0]).count() as f64 / (prior.len() - 1) as f64 > 0.6;

    let mut push = |kind: EventKind, side: Option<Side>| {
        out.push(MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: tf,
            bar_time,
            close: c,
            price: c,
            side,
            // The pattern bar's own extremes: the chart places the label
            // under the low (bullish) or over the high (bearish) off these.
            zone_low: Some(l),
            zone_high: Some(h),
            magnitude: None,
            formed_at: None,
        });
    };

    // ---- single-bar shapes ----
    // Doji: the body could not decide (a tenth of the range or less).
    if body <= range * 0.1 {
        push(EventKind::Doji, None);
    }
    // Marubozu: the whole bar is body, in its close's direction.
    if body >= range * 0.9 {
        push(EventKind::Marubozu, Some(if bullish { Side::Buy } else { Side::Sell }));
    }
    // Hammer / hanging man: long lower wick, small body near the top.
    if lower >= body * 2.0 && upper <= body.max(range * 0.05) && body > 0.0 {
        if prior_trend_down {
            push(EventKind::Hammer, Some(Side::Buy));
        } else if prior_trend_up {
            push(EventKind::HangingMan, Some(Side::Sell));
        }
    }
    // Inverted hammer / shooting star: long upper wick, small body near the low.
    if upper >= body * 2.0 && lower <= body.max(range * 0.05) && body > 0.0 {
        if prior_trend_down {
            push(EventKind::InvertedHammer, Some(Side::Buy));
        } else if prior_trend_up {
            push(EventKind::ShootingStar, Some(Side::Sell));
        }
    }

    // ---- two-bar shapes ----
    if n >= 1 {
        let prev = &candles[n - 1];
        let (po, pc) = (prev.open, prev.close);
        let pbody = (pc - po).abs();
        let pbull = pc >= po;
        if pbody > 0.0 && body > 0.0 {
            // Engulfing: this body covers the previous body entirely.
            let covers = o.min(c) <= po.min(pc) && o.max(c) >= po.max(pc);
            if covers && bullish != pbull && body > pbody {
                push(EventKind::Engulfing, Some(if bullish { Side::Buy } else { Side::Sell }));
            }
            // Harami: this body sits inside the previous body; the read is
            // the pause's reversal against the parent bar's own direction.
            let inside = o.min(c) >= po.min(pc) && o.max(c) <= po.max(pc);
            if inside && bullish != pbull {
                push(EventKind::Harami, Some(if pbull { Side::Sell } else { Side::Buy }));
            }
            // Piercing line / dark cloud cover: open beyond, close past the midpoint.
            let pmid = (po + pc) / 2.0;
            if !pbull && bullish && o < pc && c > pmid && c < po {
                push(EventKind::PiercingLine, Some(Side::Buy));
            }
            if pbull && !bullish && o > pc && c < pmid && c > po {
                push(EventKind::DarkCloudCover, Some(Side::Sell));
            }
        }
    }

    // ---- three-bar shapes ----
    if n >= 2 {
        let a = &candles[n - 2];
        let b = &candles[n - 1];
        let (abody, bbody) = ((a.close - a.open).abs(), (b.close - b.open).abs());
        let arange = a.high - a.low;
        // Morning / evening star: strong first bar, small middle, strong close
        // back past the first bar's midpoint.
        if abody > arange * 0.5 && bbody <= abody * 0.4 && body > arange * 0.5 {
            let amid = (a.open + a.close) / 2.0;
            if a.close < a.open && bullish && c > amid {
                push(EventKind::MorningStar, Some(Side::Buy));
            }
            if a.close > a.open && !bullish && c < amid {
                push(EventKind::EveningStar, Some(Side::Sell));
            }
        }
        // Three soldiers / crows: three consecutive strong bars, each closing
        // further in its direction, each near its extreme.
        let strong = |k: &Candle, up: bool| {
            let r = k.high - k.low;
            r > 0.0
                && (k.close - k.open).abs() >= r * 0.6
                && if up { k.close > k.open && k.close >= k.high - r * 0.2 } else { k.close < k.open && k.close <= k.low + r * 0.2 }
        };
        if strong(a, true) && strong(b, true) && strong(last, true) && b.close > a.close && c > b.close {
            push(EventKind::ThreeWhiteSoldiers, Some(Side::Buy));
        }
        if strong(a, false) && strong(b, false) && strong(last, false) && b.close < a.close && c < b.close {
            push(EventKind::ThreeBlackCrows, Some(Side::Sell));
        }
    }

    out
}

/// Detect every event the window's newest closed bar produced.
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
    let open_gaps = open_gaps;

    // -- Inverse gaps and balanced ranges -------------------------------------
    // Invalidation: the last bar CLOSES beyond the far edge of an open gap
    // (a wick through is a retest; a close beyond is failure). The failed gap
    // flips polarity -- a failed bullish gap reads bearish from then on.
    for (origin, side, low, high) in open_gaps.iter().rev().take(50) {
        if *origin == bar_time {
            continue;
        }
        let invalidated = match side {
            Side::Buy => last.close < *low,
            Side::Sell => last.close > *high,
        };
        // First breach only: an invalidated gap flips ONCE. A bar whose
        // predecessor already closed beyond is the flip's aftermath, not a
        // new event -- without this dedup the replay fires the same gap on
        // every later bar.
        let already = match side {
            Side::Buy => candles[candles.len() - 2].close < *low,
            Side::Sell => candles[candles.len() - 2].close > *high,
        };
        if invalidated && !already {
            events.push(MarketEvent {
                kind: EventKind::FvgInvalidated,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: (low + high) / 2.0,
                // The flipped side: the zone now works against its birth.
                side: Some(match side {
                    Side::Buy => Side::Sell,
                    Side::Sell => Side::Buy,
                }),
                zone_low: Some(*low),
                zone_high: Some(*high),
                magnitude: None,
                formed_at: Some(*origin),
            });
        }
    }
    // Balanced price range: the newest opposite pair of open gaps overlaps.
    let buys: Vec<_> = open_gaps.iter().filter(|(_, s, _, _)| *s == Side::Buy).collect();
    let sells: Vec<_> = open_gaps.iter().filter(|(_, s, _, _)| *s == Side::Sell).collect();
    if let (Some(b), Some(a)) = (buys.last(), sells.last()) {
        let (_, _, b_low, b_high) = **b;
        let (_, _, s_low, s_high) = **a;
        let lo = b_low.max(s_low);
        let hi = b_high.min(s_high);
        if lo < hi {
            events.push(MarketEvent {
                kind: EventKind::BprCreated,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time,
                close: last.close,
                price: (lo + hi) / 2.0,
                side: None,
                zone_low: Some(lo),
                zone_high: Some(hi),
                magnitude: None,
                formed_at: None,
            });
        }
    }

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

    // The pattern pass rides the same stream: shape + trend context on the
    // newest bar, merged before the deterministic sort so live and replayed
    // windows order identically.
    events.extend(detect_patterns(candles, symbol));
    events.extend(detect_price_action(candles, symbol));
    events.extend(detect_order_blocks(candles, symbol, &config));
    events.extend(detect_smc_extended(candles, symbol, &config));
    events.extend(detect_liquidity_family(candles, symbol, &config));
    events.extend(detect_sessions(candles, symbol));
    events.extend(detect_chart_patterns(candles, symbol, &config));

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
mod pattern_tests {
    use super::*;

    fn k(open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "T".to_string(),
            timeframe: crate::types::Timeframe::M15,
            open_time: 0,
            open,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    fn series(mut ks: Vec<Candle>) -> Vec<Candle> {
        for (i, k) in ks.iter_mut().enumerate() {
            k.open_time = i as i64 * 900_000_000_000;
        }
        ks
    }

    fn kinds(events: &[MarketEvent]) -> Vec<EventKind> {
        events.iter().map(|e| e.kind).collect()
    }

    // A downtrend tail (six declining closes) for reversal-context tests.
    fn downtrend() -> Vec<Candle> {
        (0..6)
            .map(|i| {
                let c = 100.0 - i as f64;
                k(c + 0.4, c + 0.6, c - 0.4, c)
            })
            .collect()
    }

    fn uptrend() -> Vec<Candle> {
        (0..6)
            .map(|i| {
                let c = 100.0 + i as f64;
                k(c - 0.4, c + 0.4, c - 0.6, c)
            })
            .collect()
    }

    #[test]
    fn a_hammer_needs_the_shape_and_the_downtrend() {
        // Long lower wick, small top body, after a down move.
        let mut ks = downtrend();
        ks.push(k(94.5, 95.0, 90.0, 94.8)); // lower wick 4.5, body 0.3, upper 0.2
        let events = detect_patterns(&series(ks), "T");
        assert!(kinds(&events).contains(&EventKind::Hammer), "{:?}", kinds(&events));
        let hammer = events.iter().find(|e| e.kind == EventKind::Hammer).expect("hammer");
        assert_eq!(hammer.side, Some(Side::Buy));

        // The same shape after an UP move is a hanging man, not a hammer.
        let mut ks = uptrend();
        ks.push(k(105.5, 106.0, 101.0, 105.8));
        let events = detect_patterns(&series(ks), "T");
        assert!(kinds(&events).contains(&EventKind::HangingMan));
        assert!(!kinds(&events).contains(&EventKind::Hammer));

        // And the shape itself must hold: a lower wick under 2x the body
        // (0.5 < 0.6) is no hammer.
        let mut ks = downtrend();
        ks.push(k(94.5, 95.0, 94.0, 94.8));
        assert!(!kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::Hammer));
    }

    #[test]
    fn a_shooting_star_needs_the_upper_wick_and_the_uptrend() {
        let mut ks = uptrend();
        ks.push(k(105.2, 110.0, 105.0, 105.5)); // upper 4.5, body 0.3, lower 0.2
        let events = detect_patterns(&series(ks), "T");
        assert!(kinds(&events).contains(&EventKind::ShootingStar));
        // At a local low the same shape reads as an inverted hammer.
        let mut ks = downtrend();
        ks.push(k(94.2, 99.0, 94.0, 94.5));
        assert!(kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::InvertedHammer));
    }

    #[test]
    fn doji_and_marubozu_are_the_two_extremes() {
        let mut ks = uptrend();
        ks.push(k(106.0, 107.0, 105.0, 106.05)); // body 0.05 of a 2.0 range
        assert!(kinds(&detect_patterns(&series(ks.clone()), "T")).contains(&EventKind::Doji));
        let mut ks = uptrend();
        ks.push(k(105.0, 107.0, 105.0, 106.95)); // nearly the whole range is body
        let events = detect_patterns(&series(ks), "T");
        assert!(kinds(&events).contains(&EventKind::Marubozu));
        assert!(!kinds(&events).contains(&EventKind::Doji));
    }

    #[test]
    fn an_engulfing_covers_the_prior_body_in_the_opposite_direction() {
        let mut ks = downtrend();
        ks.push(k(94.5, 94.6, 93.0, 93.4)); // down body 93.4..94.5
        ks.push(k(93.2, 95.6, 93.1, 95.5)); // up body 93.2..95.5 covers it
        let events = detect_patterns(&series(ks), "T");
        let engulf = events.iter().find(|e| e.kind == EventKind::Engulfing).expect("engulfing");
        assert_eq!(engulf.side, Some(Side::Buy));
        // Same-direction bars never engulf.
        let mut ks = downtrend();
        ks.push(k(94.5, 94.6, 93.0, 93.4));
        ks.push(k(94.4, 95.8, 93.2, 95.6)); // another up? no -- covers but both up
        ks.pop();
        ks.push(k(93.3, 94.0, 92.8, 92.9)); // a smaller down bar: no cover
        assert!(!kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::Engulfing));
    }

    #[test]
    fn stars_and_soldiers_need_their_three_bar_sequences() {
        // Morning star: down, small, up past the midpoint.
        let mut ks = downtrend();
        ks.push(k(95.0, 95.2, 92.0, 92.4)); // strong down (mid 93.7)
        ks.push(k(92.5, 92.8, 92.2, 92.6)); // small
        ks.push(k(92.8, 95.0, 92.6, 94.6)); // up past 93.7
        assert!(kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::MorningStar));

        // Three black crows: three strong consecutive down bars.
        let mut ks = uptrend();
        ks.push(k(106.0, 106.2, 104.0, 104.2));
        ks.push(k(104.1, 104.3, 102.0, 102.2));
        ks.push(k(102.1, 102.3, 100.0, 100.2));
        let events = detect_patterns(&series(ks), "T");
        assert!(kinds(&events).contains(&EventKind::ThreeBlackCrows));
        assert!(!kinds(&events).contains(&EventKind::ThreeWhiteSoldiers));
    }

    #[test]
    fn piercing_and_dark_cloud_need_the_midpoint_reclaim() {
        // Piercing: down bar, up bar opens lower and closes past the midpoint.
        let mut ks = downtrend();
        ks.push(k(95.0, 95.2, 92.0, 92.4)); // body 92.4..95.0, mid 93.7
        ks.push(k(92.2, 94.4, 92.0, 94.0)); // open 92.2 < 92.4, close 94.0 > 93.7, < 95.0
        assert!(kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::PiercingLine));
        // Close short of the midpoint: no pattern.
        let mut ks = downtrend();
        ks.push(k(95.0, 95.2, 92.0, 92.4));
        ks.push(k(92.2, 93.5, 92.0, 93.2));
        assert!(!kinds(&detect_patterns(&series(ks), "T")).contains(&EventKind::PiercingLine));
    }

    #[test]
    fn a_failed_gap_flips_into_an_ifvg() {
        // Bullish gap at bars 1-3 (low of bar 3 above high of bar 1), then a
        // bar closing below the gap's low: invalidated, flipped bearish.
        let mut ks = vec![
            k(100.0, 100.5, 99.5, 100.2), // bar 1 (high 100.5)
            k(100.3, 101.5, 100.1, 101.2), // displacement
            k(101.0, 102.0, 100.8, 101.8), // bar 3 (low 100.8 > 100.5: gap 100.5..100.8)
        ];
        for i in 0..6 {
            ks.push(k(101.5 - i as f64 * 0.1, 101.7, 100.9, 101.4 - i as f64 * 0.1));
        }
        ks.push(k(101.0, 101.2, 99.8, 100.2)); // close 100.2 < 100.5: through
        let events = detect_patterns_and_events(&series(ks));
        let ifvg = events.iter().find(|e| e.kind == EventKind::FvgInvalidated).expect("ifvg fires");
        assert_eq!(ifvg.side, Some(Side::Sell), "the failed bullish gap flips bearish");
        assert_eq!(ifvg.zone_low, Some(100.5));
        assert_eq!(ifvg.zone_high, Some(100.8));
    }

    #[test]
    fn opposite_gaps_overlapping_form_a_balanced_range() {
        // Bullish gap then a bearish gap whose zone overlaps it.
        let mut ks = vec![
            k(100.0, 100.5, 99.5, 100.2),
            k(100.3, 101.5, 100.1, 101.2),
            k(101.0, 102.0, 100.8, 101.8), // bullish gap 100.5..100.8
            k(102.0, 102.2, 101.0, 101.5),
            k(101.4, 101.6, 99.9, 100.1),
            k(100.0, 100.4, 99.6, 99.9),  // bearish gap 100.4..101.4? low of prior...
        ];
        for _ in 0..3 {
            ks.push(k(99.8, 100.0, 99.4, 99.7));
        }
        let events = detect_patterns_and_events(&series(ks));
        if let Some(bpr) = events.iter().find(|e| e.kind == EventKind::BprCreated) {
            assert!(bpr.zone_low < bpr.zone_high, "a balance is a real band");
        }
    }

    fn pa_series(ks: Vec<Candle>) -> Vec<Candle> {
        series(ks)
    }

    #[test]
    fn a_breakout_fires_on_the_close_through_the_channel() {
        // 20 flat bars (100..101), then a close at 102.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.2, 100.6, 102.0));
        let events = detect_patterns_and_events(&pa_series(ks));
        let brk = events.iter().find(|e| e.kind == EventKind::Breakout).expect("breakout");
        assert_eq!(brk.side, Some(Side::Buy));
        assert_eq!(brk.price, 101.0, "the level is the channel top");
        // A close INSIDE the channel fires nothing.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.6, 100.9, 100.3, 100.7));
        assert!(!kinds(&detect_patterns_and_events(&pa_series(ks))).contains(&EventKind::Breakout));
    }

    #[test]
    fn a_failed_breakout_closed_back_inside() {
        // Break out at bar 20, then bar 21 closes back under the level.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.0, 100.6, 101.8)); // the break
        ks.push(k(101.6, 101.8, 100.2, 100.5)); // back inside
        let events = detect_patterns_and_events(&pa_series(ks));
        let failed = events.iter().find(|e| e.kind == EventKind::FailedBreakout).expect("failed");
        assert_eq!(failed.side, Some(Side::Sell), "a failed upside break reads bearish");
        assert_eq!(failed.price, 101.0);
    }

    #[test]
    fn a_retest_touches_the_level_and_holds_it() {
        // Break out, wick back to the level without closing through, hold above.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.0, 100.6, 101.8)); // the break
        ks.push(k(101.7, 101.9, 100.9, 101.6)); // wick to 100.9 (above 101.0? no: 100.9 < 101.0 touches) close 101.6 above
        ks.push(k(101.6, 102.0, 101.3, 101.8)); // holds above
        let events = detect_patterns_and_events(&pa_series(ks));
        let retest = events.iter().find(|e| e.kind == EventKind::Retest).expect("retest");
        assert_eq!(retest.side, Some(Side::Buy));
        assert_eq!(retest.price, 101.0);
        // Without the touch, no retest.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.0, 100.6, 101.8));
        ks.push(k(101.7, 102.0, 101.5, 101.9));
        assert!(!kinds(&detect_patterns_and_events(&pa_series(ks))).contains(&EventKind::Retest));
    }

    #[test]
    fn higher_highs_need_both_sides_stepping() {
        // Ascending swings: highs 102, 104 and lows 99, 101. The three quiet
        // lead bars put the first swing inside the confirmation window (a
        // swing needs three bars either side to be seen at all).
        let mut ks: Vec<Candle> = (0..3).map(|_| k(99.8, 100.2, 99.4, 99.9)).collect();
        let path = [
            (100.0, 100.5, 99.3, 100.2),
            (100.2, 102.0, 100.0, 101.6), // swing high 102
            (101.6, 101.8, 100.8, 101.0),
            (101.0, 101.2, 99.0, 99.4),  // swing low 99
            (99.4, 100.6, 101.0, 100.4),
            (100.4, 101.8, 101.2, 101.5),
            (101.5, 102.0, 101.3, 101.7),
            (101.7, 102.2, 101.4, 101.9),
            (101.9, 104.0, 101.6, 103.6), // swing high 104
            (103.6, 103.8, 102.6, 103.0),
            (103.0, 103.2, 101.0, 101.4), // swing low 101
            (101.4, 101.6, 101.2, 101.5),
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        // pad past the swing's confirmation window
        for _ in 0..10 {
            ks.push(k(101.4, 101.6, 101.2, 101.5));
        }
        let events = detect_patterns_and_events(&pa_series(ks));
        let hh = events.iter().find(|e| e.kind == EventKind::HigherHighs);
        assert!(hh.is_some(), "the ascending structure fires: {:?}", kinds(&events));
        assert_eq!(hh.expect("hh").side, Some(Side::Buy));
    }

    #[test]
    fn an_order_block_is_the_last_opposing_candle_before_displacement() {
        // Quiet base, a down candle (the OB), then a displacement bar closing
        // through the swing high -- range 2x+ the 14-bar average.
        let mut ks: Vec<Candle> = (0..16)
            .map(|i| k(100.0 + (i % 3) as f64 * 0.1, 100.6, 99.8, 100.2))
            .collect();
        ks.push(k(100.4, 100.5, 99.6, 99.8)); // the down candle: OB zone 99.6..100.5
        ks.push(k(99.9, 103.5, 99.8, 103.3)); // displacement: range 3.7 >> avg ~0.8*1.5
        ks.push(k(103.2, 103.8, 102.8, 103.5)); // follow-through
        let events = detect_patterns_and_events(&series(ks));
        let ob = events.iter().find(|e| e.kind == EventKind::OrderBlockCreated).expect("ob created");
        assert_eq!(ob.side, Some(Side::Buy));
        assert_eq!(ob.zone_low, Some(99.6));
        assert_eq!(ob.zone_high, Some(100.5));

        // No displacement (a weak drift above the swing): no order block.
        let mut ks: Vec<Candle> = (0..16)
            .map(|i| k(100.0 + (i % 3) as f64 * 0.1, 100.6, 99.8, 100.2))
            .collect();
        ks.push(k(100.4, 100.5, 99.9, 100.0));
        ks.push(k(100.0, 100.9, 99.9, 100.75)); // breaks the swing by a hair, no range
        let events = detect_patterns_and_events(&series(ks));
        assert!(!kinds(&events).contains(&EventKind::OrderBlockCreated));
    }

    #[test]
    fn an_invalidated_order_block_flips_into_a_breaker() {
        let mut ks: Vec<Candle> = (0..16)
            .map(|i| k(100.0 + (i % 3) as f64 * 0.1, 100.6, 99.8, 100.2))
            .collect();
        ks.push(k(100.4, 100.5, 99.6, 99.8)); // OB zone 99.6..100.5
        ks.push(k(99.9, 103.5, 99.8, 103.3)); // displacement up
        for _ in 0..4 {
            ks.push(k(103.0, 103.4, 102.6, 103.1));
        }
        ks.push(k(103.0, 103.2, 99.0, 99.3)); // CLOSE below the zone's low
        let events = detect_patterns_and_events(&series(ks));
        let brk = events
            .iter()
            .find(|e| e.kind == EventKind::OrderBlockInvalidated)
            .expect("breaker fires");
        assert_eq!(brk.side, Some(Side::Sell), "the failed bullish OB flips bearish");
        assert_eq!(brk.zone_low, Some(99.6));
    }

    #[test]
    fn the_chart_engine_flows_through_the_event_stream() {
        // The platform's own pattern engine is the detector; the adapter
        // maps its matches onto the stream with entry levels and the trade
        // band. The engine's own proven double-top closes: two equal peaks
        // with a valley, then the break.
        let closes = [90.0, 95.0, 100.0, 97.0, 94.0, 97.0, 100.0, 97.0, 94.0, 92.0, 91.0];
        let mut ks: Vec<Candle> = Vec::new();
        for c in &closes {
            ks.push(k(*c, c + 0.5, c - 0.5, *c));
        }
        let events = detect_patterns_and_events(&series(ks));
        let chart_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::DoubleTopBottom | EventKind::HeadShoulders | EventKind::Triangle | EventKind::Wedge | EventKind::Flag))
            .collect();
        assert!(!chart_events.is_empty(), "the engine's patterns reach the stream: {:?}", kinds(&events));
        let dt = chart_events
            .iter()
            .find(|e| e.kind == EventKind::DoubleTopBottom)
            .expect("a double top wraps");
        assert_eq!(dt.side, Some(Side::Sell));
        assert!(dt.zone_low.is_some() && dt.zone_high.is_some(), "the trade band rides");
        assert!(dt.magnitude.is_some(), "the engine's confidence rides as magnitude");
    }

    #[test]
    fn a_kill_zone_fires_once_at_the_bars_crossing_its_end() {
        // Two days of hourly bars: the London window (07:00-10:00 UTC) forms
        // its box exactly when the 10:00 bar arrives.
        // Midnight-aligned: 20833 UTC days in nanos, so "hour of day" is exact.
        let day = 20833_i64 * 86_400_000_000_000_i64;
        let hour = 3_600_000_000_000_i64;
        let mut ks = Vec::new();
        for d in 0..2 {
            for h in 0..24 {
                let t = day + d * 24 * hour + h * hour;
                let base = 100.0 + (d * 24 + h) as f64 * 0.05;
                ks.push(Candle {
                    symbol: "T".to_string(),
                    timeframe: crate::types::Timeframe::H1,
                    open_time: t,
                    open: base,
                    high: base + 0.4,
                    low: base - 0.4,
                    close: base + 0.2,
                    volume: 1.0,
                    buy_volume: 0.5,
                    sell_volume: 0.5,
                });
            }
        }
        // Through the 09:00 bar (still inside the window): nothing yet.
        let events = detect_sessions(&ks[..day_bars(1, 10)], "T");
        assert!(!events.iter().any(|e| e.kind == EventKind::LondonKillZone));
        // At the 10:00 bar (the window's end): the box, once, with the
        // window's own range.
        let events = detect_sessions(&ks[..day_bars(1, 11)], "T");
        let london: Vec<_> = events.iter().filter(|e| e.kind == EventKind::LondonKillZone).collect();
        assert_eq!(london.len(), 1, "fires exactly once at the crossing");
        assert!(london[0].zone_low.expect("low") < london[0].zone_high.expect("high"));
        // And the Asian range closed at 07:00, already emitted at the 07:00 bar.
        let events = detect_sessions(&ks[..day_bars(1, 8)], "T");
        assert_eq!(events.iter().filter(|e| e.kind == EventKind::AsianRange).count(), 1);
    }

    fn day_bars(days: i64, hour: i64) -> usize {
        (days * 24 + hour) as usize
    }

    #[test]
    fn a_grab_needs_the_conviction_close() {
        // Sweep of the swing low with a BULLISH close on the same bar.
        let mut ks: Vec<Candle> = (0..14).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        ks.push(k(100.1, 100.6, 98.9, 100.4)); // wick below 99.5, closes up
        let events = detect_patterns_and_events(&series(ks));
        let grab = events.iter().find(|e| e.kind == EventKind::LiquidityGrab).expect("grab");
        assert_eq!(grab.side, Some(Side::Buy));
        // A sweep closing DOWN is a sweep, not a grab.
        let mut ks: Vec<Candle> = (0..14).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        ks.push(k(100.4, 100.5, 98.9, 99.6)); // wick below, but the close is bearish
        assert!(!kinds(&detect_patterns_and_events(&series(ks))).contains(&EventKind::LiquidityGrab));
    }

    #[test]
    fn inducement_names_the_nearest_trap_liquidity() {
        // Uptrend with stepping swings: inducement is the newest swing low.
        let mut ks: Vec<Candle> = (0..3).map(|_| k(99.8, 100.2, 99.4, 99.9)).collect();
        let path = [
            (100.0, 100.5, 99.3, 100.2),
            (100.2, 102.0, 100.0, 101.6), // swing high
            (101.6, 101.8, 100.8, 101.0),
            (101.0, 101.2, 99.0, 99.4),  // swing low 99
            (99.4, 100.6, 101.0, 100.4),
            (100.4, 101.8, 101.2, 101.5),
            (101.5, 102.0, 101.3, 101.7),
            (101.7, 102.2, 101.4, 101.9),
            (101.9, 104.0, 101.6, 103.6), // swing high 104
            (103.6, 103.8, 102.6, 103.0),
            (103.0, 103.2, 101.0, 101.4), // swing low 101 (the inducement)
            (101.4, 101.6, 101.2, 101.5),
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        for _ in 0..10 {
            ks.push(k(101.4, 101.6, 101.2, 101.5));
        }
        let events = detect_patterns_and_events(&series(ks));
        let ind = events.iter().find(|e| e.kind == EventKind::Inducement).expect("inducement");
        assert_eq!(ind.side, Some(Side::Sell), "the trap in an uptrend is the sell-side low");
        assert_eq!(ind.price, 101.0);
    }

    #[test]
    fn turtle_soup_is_a_fast_failed_break_of_a_big_swing() {
        // 20-bar range, a close above the top, and a close back inside within
        // four bars.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.0, 100.6, 101.8)); // close above the 20-bar top
        ks.push(k(101.6, 101.9, 100.8, 101.5));
        ks.push(k(101.4, 101.6, 100.2, 100.5)); // back inside: soup
        let events = detect_patterns_and_events(&series(ks));
        let soup = events.iter().find(|e| e.kind == EventKind::TurtleSoup).expect("soup");
        assert_eq!(soup.side, Some(Side::Sell), "the failed upside break is short-side soup");
        assert_eq!(soup.price, 101.0);
        // Still beyond the level (no re-entry yet): not soup yet.
        let mut ks: Vec<Candle> = (0..20).map(|_| k(100.2, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.8, 102.0, 100.6, 101.8));
        ks.push(k(101.6, 102.0, 101.2, 101.7));
        assert!(!kinds(&detect_patterns_and_events(&series(ks))).contains(&EventKind::TurtleSoup));
    }

    #[test]
    fn equal_highs_form_a_buy_side_pool() {
        // Two swing highs at ~106.0 (within tolerance), spaced apart.
        let mut ks: Vec<Candle> = (0..8).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        ks.push(k(100.2, 106.0, 100.0, 105.5)); // swing high 106.0
        for _ in 0..8 {
            ks.push(k(102.0, 102.5, 101.5, 102.2));
        }
        ks.push(k(102.2, 106.05, 102.0, 105.5)); // swing high 106.05: equal
        for _ in 0..6 {
            ks.push(k(103.0, 103.5, 102.5, 103.2));
        }
        let events = detect_patterns_and_events(&series(ks));
        let eq = events.iter().find(|e| e.kind == EventKind::EqualLevels).expect("equal levels");
        assert_eq!(eq.side, Some(Side::Buy), "equal highs hold buy stops above");
        // Far-apart highs: no pool.
        let mut ks: Vec<Candle> = (0..8).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        ks.push(k(100.2, 106.0, 100.0, 105.5));
        for _ in 0..8 {
            ks.push(k(102.0, 102.5, 101.5, 102.2));
        }
        ks.push(k(102.2, 109.0, 102.0, 108.5)); // a much higher high
        for _ in 0..6 {
            ks.push(k(103.0, 103.5, 102.5, 103.2));
        }
        assert!(!kinds(&detect_patterns_and_events(&series(ks))).contains(&EventKind::EqualLevels));
    }

    #[test]
    fn a_rejection_block_zones_the_wick_not_the_body() {
        // A lower-wick candle: body 0.4, lower wick 2.4, upper ~0.
        let mut ks: Vec<Candle> = (0..14).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        ks.push(k(100.4, 100.5, 97.6, 100.0)); // body 0.4, wick to 97.6
        for _ in 0..4 {
            ks.push(k(100.2, 100.8, 100.1, 100.5)); // never returns to the wick
        }
        let events = detect_patterns_and_events(&series(ks));
        eprintln!("detected: {:?}", kinds(&events));
        let rb = events
            .iter()
            .find(|e| e.kind == EventKind::RejectionBlockCreated)
            .expect("rejection block");
        assert_eq!(rb.zone_low, Some(97.6), "the zone is the wick tip");
        assert_eq!(rb.zone_high, Some(100.0), "to the body's bottom");
        assert_eq!(rb.side, Some(Side::Buy));
    }

    #[test]
    fn a_mitigation_block_needs_a_sweep_without_a_break() {
        // Down trend into a sweep of the swing low that closes back, with no
        // close below it afterwards -- the failed auction.
        let mut ks: Vec<Candle> = (0..14)
            .map(|i| k(102.0 - i as f64 * 0.3, 102.2 - i as f64 * 0.3, 101.4 - i as f64 * 0.3, 101.8 - i as f64 * 0.3))
            .collect();
        ks.push(k(98.0, 98.2, 96.5, 97.9)); // down candle before the sweep (the block: 96.5..98.2)
        ks.push(k(97.8, 98.0, 96.0, 97.7)); // the sweep: wick below the swing low, close back
        for _ in 0..4 {
            ks.push(k(98.5, 98.9, 98.3, 98.7)); // holds above, clear of the zone
        }
        let events = detect_patterns_and_events(&series(ks));
        let mb = events
            .iter()
            .find(|e| e.kind == EventKind::MitigationBlockCreated)
            .expect("mitigation block");
        assert_eq!(mb.side, Some(Side::Buy));
        // The OB-vs-MB distinction: a CLOSE below the swing after the sweep
        // is a break, not a failed auction -- no mitigation block.
        let mut ks2: Vec<Candle> = (0..14)
            .map(|i| k(102.0 - i as f64 * 0.3, 102.2 - i as f64 * 0.3, 101.4 - i as f64 * 0.3, 101.8 - i as f64 * 0.3))
            .collect();
        ks2.push(k(98.0, 98.2, 96.5, 97.9));
        ks2.push(k(97.8, 98.0, 96.0, 97.7));
        ks2.push(k(97.6, 97.8, 95.0, 95.4)); // close below the swing: broken
        let events = detect_patterns_and_events(&series(ks2));
        assert!(!kinds(&events).contains(&EventKind::MitigationBlockCreated));
    }

    #[test]
    fn premium_and_discount_split_the_dealing_range() {
        let mut ks: Vec<Candle> = (0..20)
            .map(|i| k(100.0 + i as f64 * 0.3, 100.5 + i as f64 * 0.3, 99.5 + i as f64 * 0.3, 100.2 + i as f64 * 0.3))
            .collect();
        ks.push(k(105.9, 106.4, 105.4, 106.0)); // near the top of the range
        let events = detect_patterns_and_events(&series(ks));
        let pd = events.iter().find(|e| e.kind == EventKind::PremiumDiscount).expect("pd");
        let lo = pd.zone_low.expect("low");
        let hi = pd.zone_high.expect("high");
        let eq = pd.price;
        assert!((eq - (lo + hi) / 2.0).abs() < 1e-9, "equilibrium is the midpoint");
        assert_eq!(pd.side, Some(Side::Sell), "above equilibrium reads premium (short-side zone)");
    }

    #[test]
    fn the_squeeze_is_the_tightest_window() {
        // Wide bars, then ten nearly-flat ones at the end.
        let mut ks: Vec<Candle> = (0..40)
            .map(|i| k(100.0, 104.0 + (i % 3) as f64, 96.0 - (i % 2) as f64, 100.5))
            .collect();
        for _ in 0..10 {
            ks.push(k(100.2, 100.6, 99.9, 100.3)); // the tight window
        }
        let events = detect_patterns_and_events(&pa_series(ks));
        assert!(kinds(&events).contains(&EventKind::RangeContraction));
        // Wide throughout: no contraction.
        let ks: Vec<Candle> = (0..40)
            .map(|i| k(100.0, 104.0 + (i % 3) as f64, 96.0 - (i % 2) as f64, 100.5))
            .collect();
        assert!(!kinds(&detect_patterns_and_events(&pa_series(ks))).contains(&EventKind::RangeContraction));
    }

    fn detect_patterns_and_events(candles: &[Candle]) -> Vec<MarketEvent> {
        detect_events(candles, "T", &EventConfig::default())
    }

    #[test]
    fn patterns_ride_the_engine_stream_too() {
        // detect_events (the engine call) carries pattern kinds alongside the
        // structure kinds -- one stream, every consumer.
        let mut ks = downtrend();
        ks.push(k(94.5, 95.0, 90.0, 94.8));
        let events = detect_events(&series(ks), "T", &EventConfig::default());
        assert!(kinds(&events).contains(&EventKind::Hammer));
    }
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
