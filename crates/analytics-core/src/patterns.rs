//! Chart pattern detection over confirmed swings.
//!
//! ## Why swings, not candles
//!
//! A head-and-shoulders is a statement about *swings*: three highs where the
//! middle one is the extreme. Candles are the evidence; swings are the claim.
//! Every pattern here is therefore detected over [`MarketStructure::points`]
//! from [`crate::market_structure`], which means patterns inherit that
//! module's guarantees: deterministic, confirmation-delayed, backtest-safe.
//!
//! ## What a detection returns
//!
//! A [`PatternMatch`] carries the pattern's own anchors (the swing times and
//! prices that define it), a neckline/entry level, a measured-move target and
//! an invalidation level. Those three numbers are the trade the pattern
//! implies; a detection without them would be a label, not a signal.
//!
//! ## What this module refuses to do
//!
//! No pattern is reported from fewer swings than its definition requires, and
//! tolerances are explicit (`tolerance_pct` of price) rather than tuned per
//! call site. A near-miss is a miss: reporting "almost a double top" teaches
//! the consumer to distrust the detector.

use serde::{Deserialize, Serialize};

use crate::market_structure::{MarketStructure, SwingKind, SwingPoint};
use crate::types::Candle;

/// The patterns this module detects. Wire names are snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Three swing highs, middle one the extreme; bearish reversal.
    HeadAndShoulders,
    /// Mirror of head-and-shoulders at lows; bullish reversal.
    InverseHeadAndShoulders,
    /// Two swing highs within tolerance; bearish reversal.
    DoubleTop,
    /// Two swing lows within tolerance; bullish reversal.
    DoubleBottom,
    /// Converging swing lines: highs descending while lows ascend.
    Triangle,
    /// Both lines sloping the same way while converging. Rising wedges break
    /// down; falling wedges break up.
    Wedge,
    /// A sharp impulse followed by a shallow counter-slope drift.
    Flag,
}

impl PatternKind {
    /// Every kind, for schema enums and exhaustive tests.
    pub const ALL: [Self; 7] = [
        Self::HeadAndShoulders,
        Self::InverseHeadAndShoulders,
        Self::DoubleTop,
        Self::DoubleBottom,
        Self::Triangle,
        Self::Wedge,
        Self::Flag,
    ];

    /// The wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HeadAndShoulders => "head_and_shoulders",
            Self::InverseHeadAndShoulders => "inverse_head_and_shoulders",
            Self::DoubleTop => "double_top",
            Self::DoubleBottom => "double_bottom",
            Self::Triangle => "triangle",
            Self::Wedge => "wedge",
            Self::Flag => "flag",
        }
    }

    /// Parse a wire name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// The direction the completed pattern implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternDirection {
    /// The pattern resolves upward.
    Bullish,
    /// The pattern resolves downward.
    Bearish,
    /// A coil: direction is set by the break, not the shape.
    Neutral,
}

/// One detected pattern with its trade geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternMatch {
    /// What was detected.
    pub kind: PatternKind,
    /// Implied direction on completion.
    pub direction: PatternDirection,
    /// 0..=1. Structure fit only: shoulder symmetry, touch quality, coil
    /// tightness. Volume and higher-timeframe context belong to the caller.
    pub confidence: f64,
    /// The swings that define the pattern, oldest first (times in the candles'
    /// own unit — nanoseconds from the platform's candle store).
    pub anchors: Vec<SwingPoint>,
    /// The level whose break activates the pattern (neckline, coil edge).
    pub entry_level: f64,
    /// The measured-move objective from the entry.
    pub target: f64,
    /// The level whose trade-through cancels the pattern.
    pub invalidation: f64,
    /// One sentence stating the shape in numbers, for the answer that cites it.
    pub summary: String,
}

/// Tuning for [`detect_patterns`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternConfig {
    /// How far two "equal" extremes may differ, as a fraction of price
    /// (0.003 = 0.3%). Shoulders, double-top peaks.
    pub tolerance_pct: f64,
    /// Minimum confirmed swings before any pattern is even attempted. Each
    /// detector additionally checks the count its own definition needs
    /// (a head-and-shoulders needs five), so this is only a guard against
    /// running over trivially short series.
    pub min_swings: usize,
}

impl Default for PatternConfig {
    fn default() -> Self {
        Self {
            tolerance_pct: 0.004,
            min_swings: 3,
        }
    }
}

/// Slopes of the two swing lines, as price-per-index, over the last `count`
/// swings of each kind. Returns `(highs_slope, lows_slope)` when both exist.
fn swing_slopes(points: &[SwingPoint], count: usize) -> Option<(f64, f64)> {
    let slope = |kind: SwingKind| -> Option<f64> {
        let swings: Vec<&SwingPoint> = points
            .iter()
            .filter(|p| p.kind == kind)
            .rev()
            .take(count)
            .collect();
        if swings.len() < 2 {
            return None;
        }
        // Least-squares over (index, price): robust to uneven spacing.
        let xs: Vec<f64> = swings.iter().map(|p| p.index as f64).collect();
        let ys: Vec<f64> = swings.iter().map(|p| p.price).collect();
        let n = xs.len() as f64;
        let mx = xs.iter().sum::<f64>() / n;
        let my = ys.iter().sum::<f64>() / n;
        let denom: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
        if denom == 0.0 {
            return None;
        }
        let num: f64 = xs
            .iter()
            .zip(ys.iter())
            .map(|(x, y)| (x - mx) * (y - my))
            .sum();
        Some(num / denom)
    };
    Some((slope(SwingKind::High)?, slope(SwingKind::Low)?))
}

/// The last `count` swings of one kind, oldest first.
fn recent(points: &[SwingPoint], kind: SwingKind, count: usize) -> Vec<SwingPoint> {
    let mut swings: Vec<SwingPoint> = points
        .iter()
        .filter(|p| p.kind == kind)
        .copied()
        .collect();
    if swings.len() > count {
        swings.drain(..swings.len() - count);
    }
    swings
}

fn detect_head_and_shoulders(
    points: &[SwingPoint],
    candles: &[Candle],
    cfg: &PatternConfig,
) -> Option<PatternMatch> {
    let highs = recent(points, SwingKind::High, 3);
    let lows = recent(points, SwingKind::Low, 2);
    if highs.len() < 3 || lows.len() < 2 {
        return None;
    }
    let (left, head, right) = (highs[0], highs[1], highs[2]);
    // Order: left shoulder, trough, head, trough, right shoulder.
    if !(left.index < lows[0].index
        && lows[0].index < head.index
        && head.index < lows[1].index
        && lows[1].index < right.index)
    {
        return None;
    }
    let tol = head.price * cfg.tolerance_pct;
    let shoulders_equal = (left.price - right.price).abs() <= tol;
    let head_is_extreme = head.price > left.price + tol && head.price > right.price + tol;
    if !shoulders_equal || !head_is_extreme {
        return None;
    }
    // Neckline: the lower of the two troughs (conservative activation).
    let neckline = lows[0].price.min(lows[1].price);
    let height = head.price - neckline;
    let symmetry = 1.0 - (left.price - right.price).abs() / tol.max(f64::EPSILON);
    let confidence = (0.55 + 0.45 * symmetry).min(1.0);
    let _ = candles;
    Some(PatternMatch {
        kind: PatternKind::HeadAndShoulders,
        direction: PatternDirection::Bearish,
        confidence,
        anchors: vec![left, lows[0], head, lows[1], right],
        entry_level: neckline,
        target: neckline - height,
        invalidation: right.price.max(head.price),
        summary: format!(
            "head {:.2} above shoulders {:.2}/{:.2}, neckline {:.2}",
            head.price, left.price, right.price, neckline
        ),
    })
}

fn detect_inverse_head_and_shoulders(
    points: &[SwingPoint],
    cfg: &PatternConfig,
) -> Option<PatternMatch> {
    let lows = recent(points, SwingKind::Low, 3);
    let highs = recent(points, SwingKind::High, 2);
    if lows.len() < 3 || highs.len() < 2 {
        return None;
    }
    let (left, head, right) = (lows[0], lows[1], lows[2]);
    if !(left.index < highs[0].index
        && highs[0].index < head.index
        && head.index < highs[1].index
        && highs[1].index < right.index)
    {
        return None;
    }
    let tol = head.price.abs() * cfg.tolerance_pct;
    let shoulders_equal = (left.price - right.price).abs() <= tol;
    let head_is_extreme = head.price < left.price - tol && head.price < right.price - tol;
    if !shoulders_equal || !head_is_extreme {
        return None;
    }
    let neckline = highs[0].price.max(highs[1].price);
    let height = neckline - head.price;
    let symmetry = 1.0 - (left.price - right.price).abs() / tol.max(f64::EPSILON);
    Some(PatternMatch {
        kind: PatternKind::InverseHeadAndShoulders,
        direction: PatternDirection::Bullish,
        confidence: (0.55 + 0.45 * symmetry).min(1.0),
        anchors: vec![left, highs[0], head, highs[1], right],
        entry_level: neckline,
        target: neckline + height,
        invalidation: right.price.min(head.price),
        summary: format!(
            "inverse head {:.2} below shoulders {:.2}/{:.2}, neckline {:.2}",
            head.price, left.price, right.price, neckline
        ),
    })
}

fn detect_double(points: &[SwingPoint], kind: SwingKind, cfg: &PatternConfig) -> Option<PatternMatch> {
    let extremes = recent(points, kind, 2);
    if extremes.len() < 2 {
        return None;
    }
    let (first, second) = (extremes[0], extremes[1]);
    let mid_kind = match kind {
        SwingKind::High => SwingKind::Low,
        SwingKind::Low => SwingKind::High,
    };
    let between: Vec<SwingPoint> = points
        .iter()
        .copied()
        .filter(|p| p.kind == mid_kind && p.index > first.index && p.index < second.index)
        .collect();
    let middle = between.last()?;
    let base = first.price.abs().max(1.0);
    let tol = base * cfg.tolerance_pct;
    if (first.price - second.price).abs() > tol {
        return None;
    }
    let (top, bottom) = match kind {
        SwingKind::High => (first.price.max(second.price), middle.price),
        SwingKind::Low => (middle.price, first.price.min(second.price)),
    };
    let height = (top - bottom).abs();
    if height <= 0.0 {
        return None;
    }
    let equality = 1.0 - (first.price - second.price).abs() / tol;
    let (pattern_kind, direction, entry, target, invalidation) = match kind {
        SwingKind::High => (
            PatternKind::DoubleTop,
            PatternDirection::Bearish,
            middle.price,
            middle.price - height,
            top,
        ),
        SwingKind::Low => (
            PatternKind::DoubleBottom,
            PatternDirection::Bullish,
            middle.price,
            middle.price + height,
            bottom,
        ),
    };
    Some(PatternMatch {
        kind: pattern_kind,
        direction,
        confidence: (0.5 + 0.5 * equality).min(1.0),
        anchors: vec![first, *middle, second],
        entry_level: entry,
        target,
        invalidation,
        summary: format!(
            "{} {:.2}/{:.2} with {} at {:.2}",
            pattern_kind.name(),
            first.price,
            second.price,
            if kind == SwingKind::High { "valley" } else { "peak" },
            middle.price
        ),
    })
}

fn detect_coil(points: &[SwingPoint], cfg: &PatternConfig) -> Option<PatternMatch> {
    let (highs_slope, lows_slope) = swing_slopes(points, 3)?;
    let highs = recent(points, SwingKind::High, 3);
    let lows = recent(points, SwingKind::Low, 3);
    if highs.len() < 2 || lows.len() < 2 {
        return None;
    }
    let top = highs.iter().map(|p| p.price).fold(f64::INFINITY, f64::min);
    let bottom = lows.iter().map(|p| p.price).fold(0.0_f64, f64::max);
    let span = top - bottom;
    if span <= 0.0 {
        return None;
    }
    // Normalise slopes to price-per-bar relative to span so the thresholds
    // survive across symbols.
    let rel_high = highs_slope / span;
    let rel_low = lows_slope / span;
    let converging = rel_high < 0.0 && rel_low > 0.0;
    let same_direction = rel_high.signum() == rel_low.signum() && rel_high.abs() > 1e-12;
    if !converging && !same_direction {
        return None;
    }
    // Convergence strength: how much the lines close per bar relative to span.
    let closure = (rel_low - rel_high).abs();
    if closure < 0.001 {
        return None;
    }
    let confidence = (0.4 + closure.min(0.2) * 3.0).min(0.9);
    let anchors: Vec<SwingPoint> = highs.into_iter().chain(lows).collect();
    if converging {
        Some(PatternMatch {
            kind: PatternKind::Triangle,
            direction: PatternDirection::Neutral,
            confidence,
            anchors,
            entry_level: top,
            target: top + span,
            invalidation: bottom,
            summary: format!(
                "coil {:.2}-{:.2}, highs falling {:.4}/bar, lows rising {:.4}/bar",
                bottom, top, highs_slope, lows_slope
            ),
        })
    } else {
        let rising = rel_high > 0.0;
        Some(PatternMatch {
            kind: PatternKind::Wedge,
            direction: if rising {
                PatternDirection::Bearish
            } else {
                PatternDirection::Bullish
            },
            confidence,
            anchors,
            entry_level: if rising { bottom } else { top },
            target: if rising { bottom - span } else { top + span },
            invalidation: if rising { top } else { bottom },
            summary: format!(
                "{} wedge {:.2}-{:.2} sloping {:.4}/bar",
                if rising { "rising" } else { "falling" },
                bottom,
                top,
                highs_slope
            ),
        })
    }
    .filter(|_| cfg.min_swings <= points.len())
}

fn detect_flag(points: &[SwingPoint], candles: &[Candle]) -> Option<PatternMatch> {
    // A flag needs an impulse: the leg into the recent swings is several times
    // the average candle range.
    if candles.len() < 20 || points.len() < 4 {
        return None;
    }
    let ranges: Vec<f64> = candles.iter().map(|c| (c.high - c.low).abs()).collect();
    let avg = ranges.iter().sum::<f64>() / ranges.len() as f64;
    if avg <= 0.0 {
        return None;
    }
    let recent_swings = recent(points, SwingKind::High, 2)
        .into_iter()
        .chain(recent(points, SwingKind::Low, 2))
        .collect::<Vec<_>>();
    let first_idx = recent_swings.iter().map(|p| p.index).min()?;
    if first_idx < 5 {
        return None;
    }
    // The impulse: net move over the 10 bars before the flag region.
    let leg_start = candles[first_idx.saturating_sub(10)].close;
    let leg_end = candles[first_idx].close;
    let leg = leg_end - leg_start;
    if leg.abs() < avg * 5.0 {
        return None;
    }
    let (highs_slope, lows_slope) = swing_slopes(points, 2)?;
    // Counter-slope drift: a bullish flag drifts down, a bearish flag drifts up.
    let bullish = leg > 0.0;
    let counter = if bullish {
        highs_slope < 0.0 && lows_slope < 0.0
    } else {
        highs_slope > 0.0 && lows_slope > 0.0
    };
    // Shallow: the drift's slope is a fraction of the impulse's slope.
    let impulse_slope = leg / 10.0;
    let shallow = highs_slope.abs() < impulse_slope.abs() * 0.5;
    if !counter || !shallow {
        return None;
    }
    let highs = recent(points, SwingKind::High, 2);
    let lows = recent(points, SwingKind::Low, 2);
    let top = highs.iter().map(|p| p.price).fold(f64::NEG_INFINITY, f64::max);
    let bottom = lows.iter().map(|p| p.price).fold(f64::INFINITY, f64::min);
    let entry = if bullish { top } else { bottom };
    Some(PatternMatch {
        kind: PatternKind::Flag,
        direction: if bullish {
            PatternDirection::Bullish
        } else {
            PatternDirection::Bearish
        },
        confidence: 0.6,
        anchors: recent_swings,
        entry_level: entry,
        target: entry + leg,
        invalidation: if bullish { bottom } else { top },
        summary: format!(
            "{} flag after impulse {:.2}, drift {:.4}/bar vs impulse {:.4}/bar",
            if bullish { "bull" } else { "bear" },
            leg,
            highs_slope,
            impulse_slope
        ),
    })
}

/// Detect `kind` over the candles, or every kind when `kind` is `None`.
///
/// The structure is detected internally with the platform default lookback;
/// patterns therefore see exactly the swings the rest of the platform sees.
#[must_use]
pub fn detect_patterns(
    candles: &[Candle],
    structure: &MarketStructure,
    kind: Option<PatternKind>,
    cfg: &PatternConfig,
) -> Vec<PatternMatch> {
    let points = &structure.points;
    if points.len() < cfg.min_swings {
        return Vec::new();
    }
    let mut out = Vec::new();
    let kinds: Vec<PatternKind> = match kind {
        Some(k) => vec![k],
        None => PatternKind::ALL.to_vec(),
    };
    for k in kinds {
        let found = match k {
            PatternKind::HeadAndShoulders => detect_head_and_shoulders(points, candles, cfg),
            PatternKind::InverseHeadAndShoulders => detect_inverse_head_and_shoulders(points, cfg),
            PatternKind::DoubleTop => detect_double(points, SwingKind::High, cfg),
            PatternKind::DoubleBottom => detect_double(points, SwingKind::Low, cfg),
            PatternKind::Triangle | PatternKind::Wedge => detect_coil(points, cfg)
                .filter(|m| m.kind == k),
            PatternKind::Flag => detect_flag(points, candles),
        };
        if let Some(m) = found {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_structure::{detect_market_structure, StructureConfig};
    use crate::types::{Candle, Timeframe};

    fn candle(i: usize, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "TEST".into(),
            timeframe: Timeframe::H1,
            open_time: i as i64 * Timeframe::H1.nanos(),
            open: (high + low) / 2.0,
            high,
            low,
            close,
            volume: 1.0,
            buy_volume: 0.5,
            sell_volume: 0.5,
        }
    }

    /// Build candles from a close-price path. High/low bracket the close by a
    /// fixed margin, so a strict local extremum in the closes is a strict
    /// extremum in the highs/lows -- which is what the swing detector needs.
    fn series(closes: &[f64]) -> Vec<Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| candle(i, c + 0.5, c - 0.5, c))
            .collect()
    }

    fn structure_for(candles: &[Candle]) -> MarketStructure {
        detect_market_structure(candles, StructureConfig { lookback: 2 })
    }

    #[test]
    fn head_and_shoulders_is_detected_with_its_geometry() {
        // L-shoulder 100, trough 95, head 105, trough 95, R-shoulder 100.
        let closes = [
            90.0, 95.0, 100.0, 97.0, 95.0, 98.0, 105.0, 98.0, 95.0, 98.0, 100.0, 97.0, 95.0, 93.0,
            92.0,
        ];
        let candles = series(&closes);
        let structure = structure_for(&candles);
        let matches = detect_patterns(
            &candles,
            &structure,
            Some(PatternKind::HeadAndShoulders),
            &PatternConfig::default(),
        );
        assert_eq!(matches.len(), 1, "swings: {:?}", structure.points);
        let m = &matches[0];
        assert_eq!(m.direction, PatternDirection::Bearish);
        assert!(m.entry_level < m.anchors[2].price);
        assert!(m.target < m.entry_level);
        assert!(m.invalidation > m.entry_level);
    }

    #[test]
    fn double_top_requires_a_valley_between_equal_peaks() {
        let closes = [
            90.0, 95.0, 100.0, 97.0, 94.0, 97.0, 100.0, 97.0, 94.0, 92.0, 91.0,
        ];
        let candles = series(&closes);
        let structure = structure_for(&candles);
        let matches = detect_patterns(
            &candles,
            &structure,
            Some(PatternKind::DoubleTop),
            &PatternConfig::default(),
        );
        assert_eq!(matches.len(), 1, "swings: {:?}", structure.points);
        assert_eq!(matches[0].direction, PatternDirection::Bearish);
    }

    #[test]
    fn a_monotonic_series_reports_nothing() {
        let closes: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let candles = series(&closes);
        let structure = structure_for(&candles);
        assert!(
            detect_patterns(&candles, &structure, None, &PatternConfig::default()).is_empty(),
            "no swings, no patterns"
        );
    }

    #[test]
    fn wire_names_round_trip() {
        for kind in PatternKind::ALL {
            assert_eq!(PatternKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(PatternKind::from_name("cup_and_handle"), None);
    }
}
