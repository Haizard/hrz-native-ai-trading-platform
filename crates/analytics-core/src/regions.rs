//! Price **regions** -- areas of the chart, not points.
//!
//! ## Why this module exists
//!
//! Every detector in this crate reports a *point*: [`ImbalanceEvent`] carries one
//! `price_level`, [`AbsorptionEvent`] one, [`LiquidityLevel`] one. Structure
//! reports swings as bare `f64` prices with no timestamp. The chart engine can
//! draw points too -- horizontal levels, candles, footprint cells.
//!
//! So a whole class of concepts is inexpressible end to end, not because the
//! analytics cannot measure them but because **nothing in the stack can carry an
//! area**. A supply/demand zone, a fair value gap, an order block and a breaker
//! block are all the same shape: a price band over a span of time. That shape is
//! this type.
//!
//! [`ImbalanceEvent`]: crate::imbalance::ImbalanceEvent
//! [`AbsorptionEvent`]: crate::absorption::AbsorptionEvent
//! [`LiquidityLevel`]: crate::liquidity::LiquidityLevel
//!
//! ## The zone rule, as stated
//!
//! A supply/demand zone is the origin of the impulsive move that broke
//! structure. Concretely, and following the rule this was written against:
//!
//! 1. Structure breaks ([`StructureBreak`]) give the impulse: a candle that
//!    closed through a confirmed swing level.
//! 2. Walk back from the breaking candle over the run of candles moving *with*
//!    the break. That run is the impulse.
//! 3. The candles immediately before it, of the opposite colour, are the origin
//!    -- at most [`ZoneConfig::max_origin_candles`], because a zone drawn from
//!    ten candles is not a zone anyone would trade.
//! 4. The band spans the lowest low to the highest high of those origin candles.
//!
//! The band is then tracked forward: [`Region::mitigated`] is how much of it
//! price has since traded back through, which is what makes "wait for 30%
//! mitigation" a statement about a number rather than about a glance.
//!
//! [`StructureBreak`]: crate::market_structure::StructureBreak

use serde::{Deserialize, Serialize};

use crate::market_structure::{detect_market_structure, BreakKind, StructureConfig};
use crate::types::{Candle, Side};

/// What a region represents.
///
/// Deliberately a small enum with room to grow: the *shape* is what the chart
/// needs, and the kind is a label. Adding a concept is adding a variant, not a
/// new geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    /// Buyers' origin: the base of an impulsive move up that broke a high.
    Demand,
    /// Sellers' origin: the base of an impulsive move down that broke a low.
    Supply,
}

impl RegionKind {
    /// Every variant, for a client's vocabulary and exhaustive tests.
    pub const ALL: &'static [Self] = &[Self::Demand, Self::Supply];

    /// Canonical name, as it appears on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Demand => "demand",
            Self::Supply => "supply",
        }
    }

    /// The side that is expected to react from a region of this kind.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            Self::Demand => Side::Buy,
            Self::Supply => Side::Sell,
        }
    }
}

/// What put a region on the chart.
///
/// Kept because "why is this band here" is the first question anyone asks of a
/// drawing, and a label that cannot answer it is decoration.
///
/// An enum rather than a `break_kind` plus a `broken_level` because not every
/// band comes from a break. A client-defined pattern has no broken level, and
/// carrying those as bare fields would have made every such band invent one --
/// a lie that reads as data until someone queries it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum RegionOrigin {
    /// The impulse that broke structure, so this band is the move's origin.
    StructureBreak {
        /// BOS (continuation) or CHoCH (reversal).
        kind: BreakKind,
        /// The swing level the impulse closed through.
        level: f64,
    },
    /// A candle pattern a concept document defined.
    ///
    /// Deliberately carries nothing else: the pattern is in the document, and
    /// duplicating it here would give two copies of one definition to keep in
    /// step.
    Pattern,
}

impl RegionOrigin {
    /// The broken swing level, when there was one.
    #[must_use]
    pub const fn broken_level(self) -> Option<f64> {
        match self {
            Self::StructureBreak { level, .. } => Some(level),
            Self::Pattern => None,
        }
    }
}

/// A price band over a span of time -- the one shape the rest of the stack lacks.
///
/// Timestamps are unix nanoseconds, as everywhere else in this workspace; a
/// chart converts at its own boundary.
///
/// ## Why the identity is a name and a side
///
/// There is no `kind` field. A band knows which side is expected to react from
/// it and what to call itself, and that is the whole of what the rest of the
/// system needs: the side decides how it fills, and the name is the colour key
/// and the label.
///
/// That is what lets a **client-defined** concept be an ordinary region rather
/// than a special case. The built-in supply/demand detector is not privileged;
/// it is one producer of bands that happen to be called `demand` and `supply`,
/// and a document a client writes is another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// Which side is expected to react from this band.
    pub side: Side,
    /// What to call it -- the colour key and the label.
    ///
    /// Display-ready: `demand`, `supply`, or whatever a client named their
    /// concept. Nothing downstream rewrites it.
    pub name: String,
    /// Bottom of the band.
    pub price_low: f64,
    /// Top of the band.
    pub price_high: f64,
    /// When the region formed -- the first candle of the pattern.
    pub formed_at: i64,
    /// The left edge to draw from. Equal to `formed_at` today, but kept separate
    /// because a band's *origin* and its *extent* are different questions.
    pub from: i64,
    /// The right edge to draw to.
    pub to: i64,
    /// How much of the band price has traded back through, `0.0..=1.0`.
    ///
    /// `0.0` is untouched -- a fresh zone, the only kind worth waiting for.
    /// `1.0` means price has been through the whole band, so there is nothing
    /// left to react to.
    pub mitigated: f64,
    /// What put it there.
    pub origin: RegionOrigin,
}

impl Region {
    /// Whether price has not yet traded into this region.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        self.mitigated <= 0.0
    }

    /// The band's height. Zero for a degenerate region, which callers should
    /// skip rather than draw as an invisible line.
    #[must_use]
    pub fn height(&self) -> f64 {
        self.price_high - self.price_low
    }

    /// Whether `price` is inside the band.
    #[must_use]
    pub fn contains(&self, price: f64) -> bool {
        price >= self.price_low && price <= self.price_high
    }
}

/// Tuning for [`detect_zones`].
///
/// Not `Eq`: `max_origin_ratio` is an `f64`, and the workspace does not pretend
/// floats are exactly comparable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoneConfig {
    /// Candles required on each side of a swing. Passed to structure detection;
    /// larger means fewer, more significant swings, and therefore fewer zones.
    pub structure_lookback: usize,
    /// The most candles an origin may span.
    ///
    /// Three by default, and that is a rule rather than a round number: an
    /// origin is a *pause*, and a "zone" drawn across ten candles is the whole
    /// preceding range, which is not a level anyone would place an order at.
    pub max_origin_candles: usize,
    /// Whether to require the origin to be a real pause.
    ///
    /// A break with no consolidation before it is a trend candle, not a zone.
    /// When set, the origin must span at most this fraction of the impulse's
    /// own range. `None` accepts every break.
    pub max_origin_ratio: Option<f64>,
}

impl Default for ZoneConfig {
    fn default() -> Self {
        Self {
            structure_lookback: 3,
            max_origin_candles: 3,
            // A pause is quieter than the move it precedes, but not silent:
            // half the impulse's range is a generous ceiling that still rejects
            // an "origin" that is really just the start of the same move.
            max_origin_ratio: Some(0.5),
        }
    }
}

/// Find the supply and demand zones in a candle series, oldest first.
///
/// See the module docs for the rule.
///
/// Infallible, like [`detect_market_structure`]: a series with no structure has
/// no zones, and that is a fact about the market rather than a failure. An empty
/// vector means "nothing qualified", and every caller here is drawing something
/// -- a chart with no zones is a chart with no zones, not an error to report.
#[must_use]
pub fn detect_zones(candles: &[Candle], config: ZoneConfig) -> Vec<Region> {
    if candles.is_empty() {
        return Vec::new();
    }

    let structure = detect_market_structure(
        candles,
        StructureConfig {
            lookback: config.structure_lookback,
        },
    );

    // The right edge of every region: the last candle's close time. A zone that
    // stopped at the break would be a historical annotation; the point of the
    // drawing is that it is still in play.
    let last = &candles[candles.len() - 1];
    let to = last.open_time + last.timeframe.nanos();

    let mut regions = Vec::new();
    for brk in &structure.breaks {
        let Some(region) = zone_for(
            candles,
            brk.index,
            brk.kind,
            brk.direction,
            brk.level,
            config,
            to,
        ) else {
            continue;
        };
        regions.push(region);
    }
    regions
}

/// Build the region for one break, or `None` when the break has no usable
/// origin.
fn zone_for(
    candles: &[Candle],
    break_index: usize,
    break_kind: BreakKind,
    direction: Side,
    broken_level: f64,
    config: ZoneConfig,
    to: i64,
) -> Option<Region> {
    // Which side of the book the origin came from is the *breaking candle's*
    // direction, which the structure detector already worked out. It is not
    // inferred from the break kind: a BOS and a CHoCH in the same direction
    // have the same origin.
    let kind = match direction {
        Side::Buy => RegionKind::Demand,
        Side::Sell => RegionKind::Supply,
    };
    let side = kind.side();

    // Step 1: the impulse. Walk back over the candles moving with the break.
    let with_break = |c: &Candle| match side {
        Side::Buy => c.close > c.open,
        Side::Sell => c.close < c.open,
    };
    let mut impulse_start = break_index;
    while impulse_start > 0 && with_break(&candles[impulse_start - 1]) {
        impulse_start -= 1;
    }

    // Step 2: the origin. The opposite-coloured candles immediately before it.
    let against = |c: &Candle| match side {
        Side::Buy => c.close < c.open,
        Side::Sell => c.close > c.open,
    };
    let origin_end = impulse_start.checked_sub(1)?;
    if !against(&candles[origin_end]) {
        // The impulse ran straight out of the previous candle with no pause
        // before it, so there is no origin to draw.
        return None;
    }
    let mut origin_start = origin_end;
    while origin_start > 0
        && against(&candles[origin_start - 1])
        && origin_end - origin_start + 1 < config.max_origin_candles
    {
        origin_start -= 1;
    }

    let origin = &candles[origin_start..=origin_end];
    let price_low = origin.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let price_high = origin
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let height = price_high - price_low;
    if height.is_nan() || height <= 0.0 {
        // Every origin candle had the same high and low. There is no band.
        return None;
    }

    // Step 3: was the origin a pause, or the start of the same move?
    if let Some(max_ratio) = config.max_origin_ratio {
        let impulse = &candles[impulse_start..=break_index];
        let impulse_low = impulse.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
        let impulse_high = impulse
            .iter()
            .map(|c| c.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let impulse_height = impulse_high - impulse_low;
        if impulse_height > 0.0 && height / impulse_height > max_ratio {
            return None;
        }
    }

    Some(Region {
        side,
        name: kind.name().to_owned(),
        price_low,
        price_high,
        formed_at: origin[0].open_time,
        from: origin[0].open_time,
        to,
        mitigated: mitigation(candles, break_index, side, price_low, price_high),
        origin: RegionOrigin::StructureBreak {
            kind: break_kind,
            level: broken_level,
        },
    })
}

/// How much of the band price has traded back through since the band formed.
///
/// Measured as the deepest adverse excursion into the band, as a share of the
/// band. A band price has never returned to is `0.0`; one price has been all the
/// way through is `1.0`.
///
/// The window is `candles[after_index + 1..]`, where `after_index` is the last
/// candle that is *part of* the pattern -- the breaking candle for a zone, the
/// last candle of the window for a client-defined pattern.
///
/// Takes a [`Side`] rather than a region kind, so this one rule serves every
/// producer. A concept a client defines means the same thing by "mitigated" as
/// the built-in detector does, and two copies of that rule would drift.
pub(crate) fn mitigation(
    candles: &[Candle],
    after_index: usize,
    side: Side,
    price_low: f64,
    price_high: f64,
) -> f64 {
    let height = price_high - price_low;
    if height.is_nan() || height <= 0.0 {
        return 1.0;
    }
    let after = &candles[after_index + 1..];
    if after.is_empty() {
        return 0.0;
    }
    // Which way price has to come from to reach the band: a buy-side band sits
    // below the market once price has moved up, so it is filled from the top
    // down.
    let travelled = match side {
        Side::Buy => {
            let deepest = after.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
            price_high - deepest
        }
        Side::Sell => {
            let deepest = after
                .iter()
                .map(|c| c.high)
                .fold(f64::NEG_INFINITY, f64::max);
            deepest - price_low
        }
    };
    (travelled / height).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    const TF: Timeframe = Timeframe::M5;

    /// A candle from open/high/low/close, with the volume fields left at zero.
    fn candle(index: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: TF,
            open_time: index * TF.nanos(),
            open,
            high,
            low,
            close,
            volume: 0.0,
            buy_volume: 0.0,
            sell_volume: 0.0,
        }
    }

    /// The series these tests share.
    ///
    /// Read left to right, with index in brackets:
    ///
    /// ```text
    ///   [0..7]   a decline, creating swing lows structure can confirm
    ///   [8..10]  the origin: three down-close candles, a pause
    ///   [11..15] the impulse: five up candles that close through the high
    ///   [16..19] a pullback that returns into the origin band
    /// ```
    ///
    /// Built by hand rather than from the database so the test states the rule
    /// it is checking instead of inheriting whatever the market did.
    fn series() -> Vec<Candle> {
        let mut out = vec![
            candle(0, 100.0, 101.0, 99.0, 99.5),
            candle(1, 99.5, 100.0, 96.0, 96.5),
            candle(2, 96.5, 98.0, 95.0, 97.5),
            candle(3, 97.5, 98.5, 94.0, 94.5),
            candle(4, 94.5, 95.5, 92.0, 92.5),
            candle(5, 92.5, 93.5, 90.0, 93.0),
            candle(6, 93.0, 96.0, 92.5, 95.5),
            // Down-close, so the run of down candles before the impulse is
            // longer than the origin cap. Its high is unchanged, so it is still
            // the confirmed swing high the impulse breaks -- which is what makes
            // this the series that tests the cap.
            candle(7, 96.5, 97.0, 95.0, 95.5),
        ];
        // The origin: three down-close candles between 94 and 96.
        out.push(candle(8, 96.0, 96.2, 94.6, 94.8));
        out.push(candle(9, 94.8, 95.0, 93.4, 93.6));
        out.push(candle(10, 93.6, 93.9, 92.8, 93.0));
        // The impulse: up candles that close above the swing high at 98.0.
        out.push(candle(11, 93.0, 95.5, 92.9, 95.0));
        out.push(candle(12, 95.0, 97.5, 94.8, 97.0));
        out.push(candle(13, 97.0, 100.0, 96.8, 99.5));
        out.push(candle(14, 99.5, 102.0, 99.0, 101.5));
        out.push(candle(15, 101.5, 104.0, 101.0, 103.5));
        // The pullback, back down into the origin band.
        out.push(candle(16, 103.5, 104.0, 100.0, 100.5));
        out.push(candle(17, 100.5, 101.0, 96.0, 96.5));
        out.push(candle(18, 96.5, 97.0, 93.5, 94.0));
        out.push(candle(19, 94.0, 95.0, 93.8, 94.8));
        out
    }

    #[test]
    fn a_bullish_break_leaves_a_demand_zone_at_its_origin() {
        let zones = detect_zones(&series(), ZoneConfig::default());
        let demand: Vec<&Region> = zones.iter().filter(|z| z.name == "demand").collect();
        assert!(!demand.is_empty(), "no demand zone found: {zones:#?}");

        let zone = demand[0];
        // The band spans the three origin candles: lowest low 92.8, highest
        // high 96.2. Not the impulse's range, and not a single candle.
        assert_eq!(zone.price_low, 92.8, "{zone:#?}");
        assert_eq!(zone.price_high, 96.2, "{zone:#?}");
        assert_eq!(zone.formed_at, 8 * TF.nanos(), "{zone:#?}");
    }

    #[test]
    fn a_zone_records_the_break_it_was_the_origin_of() {
        // "Why is this zone here" is the first question asked of a drawing, and
        // a label that cannot answer it is decoration.
        let zones = detect_zones(&series(), ZoneConfig::default());
        let zone = zones
            .iter()
            .find(|z| z.name == "demand")
            .expect("a demand zone");
        // The most recent confirmed swing high the impulse actually closed
        // through -- candle 7's high of 97.0. Not the older 98.0 at index 2,
        // which the break never tested.
        assert_eq!(
            zone.origin,
            RegionOrigin::StructureBreak {
                kind: BreakKind::Bos,
                level: 97.0,
            },
            "{zone:#?}"
        );
        assert_eq!(zone.origin.broken_level(), Some(97.0), "{zone:#?}");
        assert_eq!(zone.side, Side::Buy, "a demand zone is bought from");
    }

    #[test]
    fn the_origin_is_capped_so_a_zone_is_not_the_whole_range() {
        // The same series twice, with the cap the only difference. The run of
        // down candles before the impulse is four long (7, 8, 9, 10), so
        // without the cap the origin reaches back to candle 7 and the band grows
        // to include its high of 97.0 -- the level the impulse broke, which is
        // the one price a zone must *not* contain if it is to be an origin
        // rather than the move itself.
        //
        // `max_origin_ratio` is disabled in both so the cap is the single
        // variable under test.
        let both = ZoneConfig {
            max_origin_ratio: None,
            ..ZoneConfig::default()
        };
        let capped = detect_zones(&series(), both);
        let uncapped = detect_zones(
            &series(),
            ZoneConfig {
                max_origin_candles: 99,
                ..both
            },
        );

        let capped_zone = capped.iter().find(|z| z.name == "demand");
        let uncapped_zone = uncapped.iter().find(|z| z.name == "demand");
        let (Some(capped_zone), Some(uncapped_zone)) = (capped_zone, uncapped_zone) else {
            panic!("both configs should find a demand zone: {capped:#?} {uncapped:#?}");
        };
        // `96.2 - 92.8` is 3.4000000000000057 in binary floating point, so the
        // band height is compared with a tolerance rather than for equality.
        assert!(
            (capped_zone.height() - 3.4).abs() < 1e-9,
            "{capped_zone:#?}"
        );
        assert!(
            uncapped_zone.height() > capped_zone.height(),
            "the cap must be doing something: capped {capped_zone:#?} uncapped {uncapped_zone:#?}"
        );
        assert_eq!(
            uncapped_zone.price_high, 97.0,
            "uncapped, the origin reaches back to the broken level itself"
        );
    }

    #[test]
    fn an_impulse_with_no_pause_before_it_has_no_zone() {
        // A trend with no consolidation is a trend candle, not a zone. This
        // series rises from the first candle, so every break's "origin" would
        // be part of the same move.
        let mut rising = Vec::new();
        for i in 0..20i64 {
            let base = 100.0 + i as f64;
            rising.push(candle(i, base, base + 2.0, base - 0.5, base + 1.5));
        }
        let zones = detect_zones(
            &rising,
            ZoneConfig {
                max_origin_ratio: None,
                ..ZoneConfig::default()
            },
        );
        assert!(
            zones.is_empty(),
            "an unbroken advance has no origins to draw: {zones:#?}"
        );
    }

    #[test]
    fn mitigation_is_the_share_of_the_band_price_has_been_through() {
        let zones = detect_zones(&series(), ZoneConfig::default());
        let zone = zones
            .iter()
            .find(|z| z.name == "demand")
            .expect("a demand zone");

        // The pullback reached 93.5, and the band is 92.8..96.2 (height 3.4).
        // Travelled from the top: 96.2 - 93.5 = 2.7, so 2.7 / 3.4 = 0.794.
        let expected = (96.2 - 93.5) / (96.2 - 92.8);
        assert!(
            (zone.mitigated - expected).abs() < 1e-9,
            "expected {expected}, got {} for {zone:#?}",
            zone.mitigated
        );
        assert!(!zone.is_fresh(), "{zone:#?}");
    }

    #[test]
    fn a_zone_price_never_returned_to_is_fresh_and_unmitigated() {
        // Truncate the series before the pullback, so nothing has traded back
        // into the band. This is the state the setup actually waits for.
        let mut early = series();
        early.truncate(16);
        let zones = detect_zones(&early, ZoneConfig::default());
        let zone = zones
            .iter()
            .find(|z| z.name == "demand")
            .expect("a demand zone");
        assert_eq!(zone.mitigated, 0.0, "{zone:#?}");
        assert!(zone.is_fresh(), "{zone:#?}");
    }

    #[test]
    fn a_region_is_a_band_and_says_so() {
        // The whole point of the type: it has a height and contains prices. A
        // point could do neither.
        let zones = detect_zones(&series(), ZoneConfig::default());
        let zone = &zones[0];
        assert!(zone.height() > 0.0, "{zone:#?}");
        assert!(zone.contains(zone.price_low), "{zone:#?}");
        assert!(zone.contains(zone.price_high), "{zone:#?}");
        assert!(!zone.contains(zone.price_high + 1.0), "{zone:#?}");
        assert!(!zone.contains(zone.price_low - 1.0), "{zone:#?}");
    }

    #[test]
    fn every_region_extends_to_the_right_edge() {
        // A zone that stopped at the break would be a historical annotation.
        let candles = series();
        let last_close = candles[candles.len() - 1].open_time + TF.nanos();
        for zone in detect_zones(&candles, ZoneConfig::default()) {
            assert_eq!(zone.to, last_close, "{zone:#?}");
            assert!(zone.from <= zone.to, "{zone:#?}");
        }
    }

    #[test]
    fn an_empty_series_has_no_zones_and_is_not_an_error() {
        assert!(detect_zones(&[], ZoneConfig::default()).is_empty());
    }

    #[test]
    fn the_built_in_names_are_the_ones_a_client_reads() {
        // The built-in zone vocabulary. There is no longer a `kind` on a region
        // -- a band carries a name and a side -- so this is where the built-in
        // pair is stated.
        let rendered: Vec<String> = RegionKind::ALL
            .iter()
            .map(|k| serde_json::to_string(k).unwrap())
            .collect();
        assert_eq!(rendered, ["\"demand\"", "\"supply\""]);
        assert_eq!(RegionKind::Demand.side(), Side::Buy);
        assert_eq!(RegionKind::Supply.side(), Side::Sell);
    }

    #[test]
    fn a_region_survives_the_wire_with_its_geometry_intact() {
        // The chart reads these by name. A rename is not a compile error
        // anywhere -- it is a zone that silently stops being drawn.
        let zones = detect_zones(&series(), ZoneConfig::default());
        let value = serde_json::to_value(&zones[0]).unwrap();
        for key in [
            "side",
            "name",
            "price_low",
            "price_high",
            "formed_at",
            "from",
            "to",
            "mitigated",
            "origin",
        ] {
            assert!(value.get(key).is_some(), "`{key}` is missing from {value}");
        }
        assert_eq!(value["side"], "Buy", "the side travels as it always has");
        assert_eq!(value["name"], "demand");
        // And the origin says what it is, rather than leaving a reader to
        // interpret a bare `level` field that only some regions have.
        assert_eq!(value["origin"]["source"], "structure_break", "{value}");
        assert_eq!(value["origin"]["kind"], "Bos", "{value}");
    }

    #[test]
    fn a_pattern_origin_carries_no_level() {
        // The reason the two fields became one enum. Serialised, a
        // client-defined band's origin has no `level` key at all -- so a client
        // cannot read one that was never there.
        let value = serde_json::to_value(RegionOrigin::Pattern).unwrap();
        assert_eq!(value["source"], "pattern", "{value}");
        assert!(value.get("level").is_none(), "{value}");
    }
}
