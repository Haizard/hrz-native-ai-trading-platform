//! The Concept Registry: every detector as a registered, versioned,
//! self-describing component.
//!
//! One registry, many consumers: the chart's Concept Explorer, the strategy
//! composer, the thesis agent, the math tools, and the skills system all read
//! the same catalog and the same event contract -- so what the chart draws,
//! the strategy trades, and the agent cites can never drift apart.
//!
//! A concept declares three things, and they are deliberately separate:
//!
//! - **The definition** (`name`, `description`): what the concept means.
//! - **The algorithm** (`parameters`, `event_kinds`): how this variant
//!   detects it, versioned (`version`) because two traders define an order
//!   block differently and both are right.
//! - **The interpretation**: left to the consumer -- a strategy trades it,
//!   the thesis cites it, the chart draws it. The registry never prescribes.
//!
//! Every concept also declares the data it honestly needs
//! (`required_channels`), wired to the same `DataChannels` the broker
//! registry uses: a concept that needs aggressor trades is unavailable on a
//! Deriv synthetic by construction, not by a runtime surprise.

use analytics_core::events::{EventKind, MarketEvent};
use analytics_core::types::Side;
use serde::Serialize;

use crate::broker::DataChannels;

/// The library a concept belongs to -- the catalog's top-level grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Library {
    /// Smart Money Concepts: structure, liquidity, imbalance.
    Smc,
    /// Supply and demand zones and their lifecycle.
    SupplyDemand,
    /// Volume-derived facts (spikes and friends).
    Volume,
    /// Order-flow facts needing aggressor data.
    OrderFlow,
    /// Candlestick patterns: shape plus local trend context.
    Candles,
    /// Price action: breakouts, retests, structure, ranges.
    PriceAction,
    /// Session concepts: kill zones and session ranges, in UTC.
    Session,
    /// Classical chart patterns: tops, shoulders, triangles, flags.
    Chart,
    /// Harmonic patterns: the platform's calibrated XABCD family.
    Harmonics,
    /// Fibonacci levels of the newest swings.
    Fibonacci,
    /// Technical indicators and their compositions (RSI-PCA and friends).
    Indicators,
}

impl Library {
    /// The catalog id prefix, e.g. `smc` for `smc.bos`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Smc => "smc",
            Self::SupplyDemand => "sd",
            Self::Volume => "volume",
            Self::OrderFlow => "order_flow",
            Self::Candles => "candles",
            Self::PriceAction => "pa",
            Self::Session => "session",
            Self::Chart => "chart",
            Self::Harmonics => "harmonics",
            Self::Fibonacci => "fib",
            Self::Indicators => "indicators",
        }
    }
}

/// One tunable parameter of a detector variant.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ConceptParam {
    /// The parameter name (`swing_lookback`).
    pub name: &'static str,
    /// Its default.
    pub default: f64,
    /// The smallest sane value.
    pub min: f64,
    /// The largest sane value.
    pub max: f64,
    /// What it changes, in one clause.
    pub description: &'static str,
}

/// A registered concept: definition, variant, and data declaration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ConceptSpec {
    /// The catalog id, `library.concept` (`smc.bos`).
    pub id: &'static str,
    /// This variant's version (`1.0.0`): a changed detector is a new version.
    pub version: &'static str,
    /// The library it belongs to.
    pub library: Library,
    /// The display name.
    pub name: &'static str,
    /// What the concept means, in one sentence a trader can act on.
    pub description: &'static str,
    /// The variant's tunable parameters.
    pub parameters: &'static [ConceptParam],
    /// The data channels this concept honestly needs (gated per symbol).
    pub required_channels: DataChannels,
    /// Whether this concept renders as an indicator sub-pane below the
    /// chart: the shell fetches its series (GET /indicator-series) and the
    /// engine positions it. Event-only concepts leave this false.
    pub pane: bool,
    /// Whether this concept renders as overlay lines ON the price plot
    /// (EMAs, Bollinger bands): same series fetch, drawn in the price frame.
    pub overlay: bool,
    /// The analytics event kinds this variant emits (the existing detections
    /// it wraps; empty while a concept is declared but not yet detectable).
    pub event_kinds: &'static [EventKind],
}

/// The direction a concept event reads as, from the consumer's chair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Bullish evidence (a BOS up, a swept sell side).
    Bullish,
    /// Bearish evidence.
    Bearish,
    /// No directional reading (a volume spike).
    Neutral,
}

/// A concept event's lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConceptStatus {
    /// Just created, still open to development (a fresh gap).
    Forming,
    /// Confirmed by its own rule (a candle-close break, a sweep close).
    Confirmed,
    /// Its defining condition broke (a retested zone's rule violated).
    Invalidated,
    /// Its lifecycle completed (a filled gap, an aged-out zone).
    Expired,
}

/// The standardized event contract -- the one shape every consumer reads.
///
/// Carries levels, never prose: `price_level` and the zone band are numbers
/// a chart draws and an agent cites verbatim. `evidence` holds the
/// kind-specific measured facts (the swept touches, the spike multiple).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptEvent {
    /// The emitting concept's catalog id.
    pub concept_id: String,
    /// The emitting variant's version.
    pub concept_version: String,
    /// The market the bar closed on.
    pub symbol: String,
    /// The resolution that produced the bar.
    pub timeframe: String,
    /// The consumer-side direction.
    pub direction: Direction,
    /// The lifecycle status.
    pub status: ConceptStatus,
    /// Open time of the bar that caused the event, unix nanoseconds.
    pub event_time_ns: i64,
    /// The level the event is about, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_level: Option<f64>,
    /// The bottom of a band, for band-shaped concepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_low: Option<f64>,
    /// The top of a band, for band-shaped concepts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_high: Option<f64>,
    /// The measured facts, kind-specific (never prose).
    pub evidence: serde_json::Value,
    /// The bar range the determination used: (start_ns, end_ns).
    pub bars: (i64, i64),
}

// ---------------------------------------------------------------------------
// The first catalog: the existing detections, registered as concepts.

/// Shared lookback tuning (the engine's own `structure_lookback`).
const STRUCTURE_PARAMS: &[ConceptParam] = &[ConceptParam {
    name: "swing_lookback",
    default: 10.0,
    min: 3.0,
    max: 60.0,
    description: "bars either side a swing must clear to count",
}];

/// The volume spike tuning.
const SPIKE_PARAMS: &[ConceptParam] = &[ConceptParam {
    name: "spike_multiple",
    default: 2.5,
    min: 1.2,
    max: 20.0,
    description: "multiple of the recent average that counts as a spike",
}];

/// The registry itself: every concept the platform knows, newest-last within
/// a library so the id order reads as the catalog was built.
#[must_use]
pub fn concept_registry() -> &'static [ConceptSpec] {
    &[
        ConceptSpec {
            id: "smc.bos",
            version: "1.0.0",
            library: Library::Smc,
            name: "Break of Structure",
            description: "a close through the last swing in the trend's direction -- continuation with a level to cite",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Bos],
        },
        ConceptSpec {
            id: "smc.choch",
            version: "1.0.0",
            library: Library::Smc,
            name: "Change of Character",
            description: "a close through the last swing against the trend's direction -- the first structure-level reversal evidence",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Choch],
        },
        ConceptSpec {
            id: "smc.liquidity_sweep",
            version: "1.0.0",
            library: Library::Smc,
            name: "Liquidity Sweep",
            description: "price ran beyond a liquidity level and closed back inside it -- the pool was taken without acceptance",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LiquiditySweep],
        },
        ConceptSpec {
            id: "smc.fvg",
            version: "1.0.0",
            library: Library::Smc,
            name: "Fair Value Gap",
            description: "an imbalance left by displacement, tracked from creation to fill -- the zone price may return to",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FvgCreated, EventKind::FvgFilled],
        },
        ConceptSpec {
            id: "smc.ifvg",
            version: "1.0.0",
            library: Library::Smc,
            name: "Inverse Fair Value Gap",
            description: "a gap whose close-through failed it flips polarity -- the zone now works against its birth",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FvgInvalidated],
        },
        ConceptSpec {
            id: "smc.bpr",
            version: "1.0.0",
            library: Library::Smc,
            name: "Balanced Price Range",
            description: "two opposite open gaps overlapping -- the overlap is the balance price gravitates back to",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::BprCreated],
        },
        ConceptSpec {
            id: "smc.order_block",
            version: "1.0.0",
            library: Library::Smc,
            name: "Order Block",
            description: "the last opposing candle before a displacement-driven break -- where the move was loaded, and where it may return",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::OrderBlockCreated, EventKind::OrderBlockMitigated],
        },
        ConceptSpec {
            id: "smc.breaker_block",
            version: "1.0.0",
            library: Library::Smc,
            name: "Breaker Block",
            description: "an order block whose close-through failed it flips type -- the failed support becomes the resistance",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::OrderBlockInvalidated],
        },
        ConceptSpec {
            id: "smc.premium_discount",
            version: "1.0.0",
            library: Library::Smc,
            name: "Premium / Discount",
            description: "the dealing range split at its 50% equilibrium -- longs hunt in discount, shorts in premium",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::PremiumDiscount],
        },
        ConceptSpec {
            id: "smc.equal_levels",
            version: "1.0.0",
            library: Library::Smc,
            name: "Equal Highs / Equal Lows",
            description: "two same-side swings within a hair of each other -- the stops resting there are the pool price comes for",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::EqualLevels],
        },
        ConceptSpec {
            id: "smc.rejection_block",
            version: "1.0.0",
            library: Library::Smc,
            name: "Rejection Block",
            description: "a long-wick candle where the wick IS the zone -- the rejection lives in the part that got refused",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::RejectionBlockCreated, EventKind::RejectionBlockMitigated],
        },
        ConceptSpec {
            id: "smc.mitigation_block",
            version: "1.0.0",
            library: Library::Smc,
            name: "Mitigation Block",
            description: "the failed-auction block: liquidity swept without a structure break -- institutions mitigated, price reverses",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::MitigationBlockCreated, EventKind::MitigationBlockMitigated, EventKind::MitigationBlockInvalidated],
        },
        ConceptSpec {
            id: "smc.liquidity_grab",
            version: "1.0.0",
            library: Library::Smc,
            name: "Liquidity Grab",
            description: "a sweep whose own bar closes with conviction the other way -- the pool was taken and the turn is already moving",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LiquidityGrab],
        },
        ConceptSpec {
            id: "smc.inducement",
            version: "1.0.0",
            library: Library::Smc,
            name: "Inducement",
            description: "the nearest opposing liquidity in the trend's direction -- the trap set before the real move continues",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Inducement],
        },
        ConceptSpec {
            id: "smc.turtle_soup",
            version: "1.0.0",
            library: Library::Smc,
            name: "Turtle Soup",
            description: "a break of a significant swing that re-enters the range within bars -- the false breakout Linda Raschke named",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::TurtleSoup],
        },
        ConceptSpec {
            id: "session.london_killzone",
            version: "1.0.0",
            library: Library::Session,
            name: "London Kill Zone",
            description: "07:00-10:00 UTC (ICT's London window) -- the range the session formed, drawn as its own box",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LondonKillZone],
        },
        ConceptSpec {
            id: "session.new_york_killzone",
            version: "1.0.0",
            library: Library::Session,
            name: "New York Kill Zone",
            description: "12:00-15:00 UTC (ICT's New York window) -- the range the session formed, drawn as its own box",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::NewYorkKillZone],
        },
        ConceptSpec {
            id: "session.asian_range",
            version: "1.0.0",
            library: Library::Session,
            name: "Asian Range",
            description: "00:00-07:00 UTC -- the band the Asian session built, which London and New York trade against",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::AsianRange],
        },
        ConceptSpec {
            id: "session.daily_open",
            version: "1.0.0",
            library: Library::Session,
            name: "Daily Open",
            description: "the day's opening price -- the level the day is judged against",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::DailyOpen],
        },
        ConceptSpec {
            id: "session.weekly_open",
            version: "1.0.0",
            library: Library::Session,
            name: "Weekly Open",
            description: "the week's opening price (UTC Monday) -- the level the week is judged against",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::WeeklyOpen],
        },
        ConceptSpec {
            id: "smc.smt_divergence",
            version: "1.0.0",
            library: Library::Smc,
            name: "SMT Divergence",
            description: "the crack in correlation: one market sweeps a swing its pair does not confirm (a PAIR concept, scanned via /concept-events/smt)",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "chart.double_top_bottom",
            version: "1.0.0",
            library: Library::Chart,
            name: "Double Top / Double Bottom",
            description: "two swings at one level with a real dip between, confirmed through the neckline -- the market said the level twice",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::DoubleTopBottom],
        },
        ConceptSpec {
            id: "chart.head_shoulders",
            version: "1.0.0",
            library: Library::Chart,
            name: "Head & Shoulders",
            description: "three swings with the middle highest and the shoulders symmetric, confirmed through the neckline -- the classic distribution top (and its inverse)",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HeadShoulders],
        },
        ConceptSpec {
            id: "chart.triangle",
            version: "1.0.0",
            library: Library::Chart,
            name: "Triangle",
            description: "converging structure: flat tops with rising lows (ascending), the mirror (descending), or both sides closing in (symmetrical)",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Triangle],
        },
        ConceptSpec {
            id: "chart.flag",
            version: "1.0.0",
            library: Library::Chart,
            name: "Flag",
            description: "a strong impulse then a tight counter channel -- the trend catching its breath before the next leg",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Flag],
        },
        ConceptSpec {
            id: "chart.wedge",
            version: "1.0.0",
            library: Library::Chart,
            name: "Wedge",
            description: "both swing lines sloping the same way while converging -- rising wedges break down, falling wedges break up",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Wedge],
        },
        ConceptSpec {
            id: "chart.triple_top_bottom",
            version: "1.0.0",
            library: Library::Chart,
            name: "Triple Top / Triple Bottom",
            description: "three swings at one level with the neckline broken -- the market said it a third time, and meant it",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::TripleTopBottom],
        },
        ConceptSpec {
            id: "chart.rectangle",
            version: "1.0.0",
            library: Library::Chart,
            name: "Rectangle Breakout",
            description: "flat tops and flat bottoms as the channel, and the side that finally gave way",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::RectangleBreakout],
        },
        ConceptSpec {
            id: "chart.cup_and_handle",
            version: "1.0.0",
            library: Library::Chart,
            name: "Cup and Handle",
            description: "a rounded bottom back to its rim, a shallow handle in the upper half, and the resumption",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::CupAndHandle],
        },
        ConceptSpec {
            id: "chart.pennant",
            version: "1.0.0",
            library: Library::Chart,
            name: "Pennant",
            description: "a strong impulse with a converging small channel -- the flag's tighter cousin",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Pennant],
        },
        ConceptSpec {
            id: "harmonics.gartley",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Gartley",
            description: "the classic XABCD: B at the 0.618 retracement, D completing at the 0.786 -- the harmonic the family is named for",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicGartley],
        },
        ConceptSpec {
            id: "harmonics.bat",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Bat",
            description: "the deep XABCD: B shallow, D completing at the 0.886 of XA -- the pattern that taught the family patience",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicBat],
        },
        ConceptSpec {
            id: "harmonics.butterfly",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Butterfly",
            description: "the extended XABCD: D completing BEYOND X at the 1.272 extension -- the pattern that ends past the start",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicButterfly],
        },
        ConceptSpec {
            id: "harmonics.crab",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Crab",
            description: "the deepest completion: D at the 1.618 extension of XA -- exhaustion, not continuation",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicCrab],
        },
        ConceptSpec {
            id: "harmonics.shark",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Shark",
            description: "the newer harmonic: C beyond A, D at the 0.886-1.13 of the extended range",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicShark],
        },
        ConceptSpec {
            id: "harmonics.cypher",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Cypher",
            description: "the pattern with the B inside XA and the C beyond it: D completing at the 0.786 of XC",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicCypher],
        },
        ConceptSpec {
            id: "harmonics.abcd",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "AB=CD",
            description: "the measured move: the CD leg mirrors the AB leg in price and time -- symmetry as the signal",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicAbcd],
        },
        ConceptSpec {
            id: "harmonics.three_drives",
            version: "1.0.0",
            library: Library::Harmonics,
            name: "Three Drives",
            description: "three pushes at Fibonacci extensions (1.272-1.618): the trend exhausting itself in threes",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HarmonicThreeDrives],
        },
        ConceptSpec {
            id: "fib.retracement",
            version: "1.0.0",
            library: Library::Fibonacci,
            name: "Fibonacci Retracement",
            description: "the newest swing's retracement ladder (23.6 through 78.6) -- the levels, from the swing's own prices",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FibRetracement],
        },
        ConceptSpec {
            id: "fib.golden_pocket",
            version: "1.0.0",
            library: Library::Fibonacci,
            name: "Golden Pocket",
            description: "the 0.618-0.65 band of the newest swing -- the pocket traders actually mean",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::GoldenPocket],
        },
        ConceptSpec {
            id: "fib.extension",
            version: "1.0.0",
            library: Library::Fibonacci,
            name: "Fibonacci Extension",
            description: "the 1.272 and 1.618 levels beyond the newest swing -- where the leg projects",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FibExtension],
        },
        ConceptSpec {
            id: "fib.confluence",
            version: "1.0.0",
            library: Library::Fibonacci,
            name: "Fibonacci Confluence",
            description: "two swings' levels agreeing within tolerance -- the band where the math agrees with itself",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FibConfluence],
        },
        ConceptSpec {
            id: "fib.time_zones",
            version: "1.0.0",
            library: Library::Fibonacci,
            name: "Fibonacci Time Zones",
            description: "the swing's Fibonacci-count bars (3, 5, 8, 13, 21, 34, 55) -- time measured from the origin",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FibTimeZone],
        },
        ConceptSpec {
            id: "indicators.rsi_pca",
            version: "1.0.0",
            library: Library::Indicators,
            name: "RSI-PCA Model",
            description: "the neurotrader model, in-house: RSI at 2..24, principal components of the family, a least-squares read of the next 6 bars, walk-forward with no leak -- the prediction's quantile extremes are the signal",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[EventKind::RsiPcaSignal],
        },
        ConceptSpec {
            id: "indicators.rsi",
            version: "1.0.0",
            library: Library::Indicators,
            name: "RSI",
            description: "Wilder's relative strength index over 14 bars -- the momentum measurement itself, in its own pane with the 30/70 bands",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.rsi_divergence",
            version: "1.0.0",
            library: Library::Indicators,
            name: "RSI Divergence",
            description: "the RSI line plus every confirmed RSI/price divergence in view -- the oscillator and its disagreement with price, one pane",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.rsi_family",
            version: "1.0.0",
            library: Library::Indicators,
            name: "RSI Family",
            description: "every RSI period 2..24 as one ribbon -- clustered lines are a consensus trend, a fanning ribbon is a transition; the information the PCA model quantifies, visible",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.macd",
            version: "1.0.0",
            library: Library::Indicators,
            name: "MACD",
            description: "the 12/26 EMA difference, its 9-bar signal, and the histogram between them -- momentum as two lines and bars in one pane",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.atr",
            version: "1.0.0",
            library: Library::Indicators,
            name: "ATR",
            description: "Wilder's average true range over 14 bars -- the volatility measurement itself, one line in its own pane",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: true,
            overlay: false,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.ema",
            version: "1.0.0",
            library: Library::Indicators,
            name: "EMA Stack",
            description: "the 20/50/200 exponential averages on the price plot -- the trend stack traders actually watch",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: true,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.sma",
            version: "1.0.0",
            library: Library::Indicators,
            name: "SMA Stack",
            description: "the 20/50/200 simple averages on the price plot -- the slower read of the same trend",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: true,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "indicators.bollinger",
            version: "1.0.0",
            library: Library::Indicators,
            name: "Bollinger Bands",
            description: "the 20-bar average with its two-sigma bands on the price plot -- volatility as a channel around price",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: true,
            event_kinds: &[],
        },
        ConceptSpec {
            id: "sd.zone_tested",
            version: "1.0.0",
            library: Library::SupplyDemand,
            name: "Supply/Demand Zone Test",
            description: "price traded into a fresh supply or demand zone -- the zone's first test is its strongest",
            parameters: STRUCTURE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ZoneTested],
        },
        ConceptSpec {
            id: "volume.spike",
            version: "1.0.0",
            library: Library::Volume,
            name: "Volume Spike",
            description: "bar volume at a multiple of the recent average -- participation arrived",
            parameters: SPIKE_PARAMS,
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::VolumeSpike],
        },
        ConceptSpec {
            id: "volume.poc",
            version: "1.0.0",
            library: Library::Volume,
            name: "Point of Control",
            description: "the heaviest volume bucket -- the price the market accepted most (empty on zero-volume instruments, honestly)",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::VolumePoc],
        },
        ConceptSpec {
            id: "volume.value_area",
            version: "1.0.0",
            library: Library::Volume,
            name: "Value Area",
            description: "the band where the bulk of the volume traded (VAH to VAL) -- acceptance lives inside, rejection outside",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ValueArea],
        },
        ConceptSpec {
            id: "volume.hvn",
            version: "1.0.0",
            library: Library::Volume,
            name: "High-Volume Nodes",
            description: "accepted price: the histogram's local maxima, banded -- where the market was content to trade",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HighVolumeNode],
        },
        ConceptSpec {
            id: "volume.lvn",
            version: "1.0.0",
            library: Library::Volume,
            name: "Low-Volume Nodes",
            description: "rejected price: the histogram's local minima, banded -- the gaps price moves through fast",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LowVolumeNode],
        },
        ConceptSpec {
            id: "order_flow.delta_shift",
            version: "1.0.0",
            library: Library::OrderFlow,
            name: "Delta Shift",
            description: "bar delta at a multiple of its recent average absolute value -- aggressors leaned one way",
            parameters: SPIKE_PARAMS,
            required_channels: DataChannels::TRADES_AGGRESSOR,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::DeltaShift],
        },
        ConceptSpec {
            id: "candles.hammer",
            version: "1.0.0",
            library: Library::Candles,
            name: "Hammer",
            description: "a long lower wick with a small top body at a local low -- rejection of lower prices after a down move",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Hammer],
        },
        ConceptSpec {
            id: "candles.hanging_man",
            version: "1.0.0",
            library: Library::Candles,
            name: "Hanging Man",
            description: "the hammer shape at a local high -- the same candle, and the trend is the only thing that tells them apart",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HangingMan],
        },
        ConceptSpec {
            id: "candles.shooting_star",
            version: "1.0.0",
            library: Library::Candles,
            name: "Shooting Star",
            description: "a long upper wick with a small bottom body at a local high -- rejection of higher prices after an up move",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ShootingStar],
        },
        ConceptSpec {
            id: "candles.inverted_hammer",
            version: "1.0.0",
            library: Library::Candles,
            name: "Inverted Hammer",
            description: "the shooting-star shape at a local low -- buyers tested higher after a down move",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::InvertedHammer],
        },
        ConceptSpec {
            id: "candles.doji",
            version: "1.0.0",
            library: Library::Candles,
            name: "Doji",
            description: "open and close nearly equal -- the bar could not decide",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Doji],
        },
        ConceptSpec {
            id: "candles.marubozu",
            version: "1.0.0",
            library: Library::Candles,
            name: "Marubozu",
            description: "a full-body bar with no wicks worth naming -- one side owned the whole bar",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Marubozu],
        },
        ConceptSpec {
            id: "candles.engulfing",
            version: "1.0.0",
            library: Library::Candles,
            name: "Engulfing",
            description: "this bar's body covers the previous bar's body entirely -- a one-bar takeover",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Engulfing],
        },
        ConceptSpec {
            id: "candles.harami",
            version: "1.0.0",
            library: Library::Candles,
            name: "Harami",
            description: "a small bar inside the previous bar's body -- the move paused, and pauses reverse or resolve",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Harami],
        },
        ConceptSpec {
            id: "candles.morning_star",
            version: "1.0.0",
            library: Library::Candles,
            name: "Morning Star",
            description: "down bar, small indecision bar, up bar back past the first's midpoint -- a three-bar bottoming sequence",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::MorningStar],
        },
        ConceptSpec {
            id: "candles.evening_star",
            version: "1.0.0",
            library: Library::Candles,
            name: "Evening Star",
            description: "up bar, small indecision bar, down bar back past the first's midpoint -- a three-bar topping sequence",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::EveningStar],
        },
        ConceptSpec {
            id: "candles.piercing_line",
            version: "1.0.0",
            library: Library::Candles,
            name: "Piercing Line",
            description: "a down bar, then an up bar opening lower and closing past its midpoint -- buyers reclaimed half the drop in one bar",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::PiercingLine],
        },
        ConceptSpec {
            id: "candles.dark_cloud_cover",
            version: "1.0.0",
            library: Library::Candles,
            name: "Dark Cloud Cover",
            description: "an up bar, then a down bar opening higher and closing past its midpoint -- sellers took back half the rally",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::DarkCloudCover],
        },
        ConceptSpec {
            id: "candles.three_white_soldiers",
            version: "1.0.0",
            library: Library::Candles,
            name: "Three White Soldiers",
            description: "three consecutive strong up bars closing near their highs -- sustained initiative buying",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ThreeWhiteSoldiers],
        },
        ConceptSpec {
            id: "candles.three_black_crows",
            version: "1.0.0",
            library: Library::Candles,
            name: "Three Black Crows",
            description: "three consecutive strong down bars closing near their lows -- sustained initiative selling",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ThreeBlackCrows],
        },
        ConceptSpec {
            id: "candles.dragonfly_doji",
            version: "1.0.0",
            library: Library::Candles,
            name: "Dragonfly Doji",
            description: "the doji with the long lower shadow and the close at the top -- the sellers were answered",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::DragonflyDoji],
        },
        ConceptSpec {
            id: "candles.gravestone_doji",
            version: "1.0.0",
            library: Library::Candles,
            name: "Gravestone Doji",
            description: "the doji with the long upper shadow and the close at the bottom -- the buyers were answered",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::GravestoneDoji],
        },
        ConceptSpec {
            id: "candles.long_legged_doji",
            version: "1.0.0",
            library: Library::Candles,
            name: "Long-Legged Doji",
            description: "indecision stretched both ways -- the bar that means the next one decides",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LongLeggedDoji],
        },
        ConceptSpec {
            id: "candles.spinning_top",
            version: "1.0.0",
            library: Library::Candles,
            name: "Spinning Top",
            description: "small body with shadows both sides -- the move caught its breath",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::SpinningTop],
        },
        ConceptSpec {
            id: "candles.tweezer",
            version: "1.0.0",
            library: Library::Candles,
            name: "Tweezer Tops / Bottoms",
            description: "two bars printing the same extreme -- the level was tested twice and held or broke",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Tweezer],
        },
        ConceptSpec {
            id: "candles.kicker",
            version: "1.0.0",
            library: Library::Candles,
            name: "Kicker",
            description: "a gap against the prior bar's direction with a full body through it -- the market changed its mind overnight",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Kicker],
        },
        ConceptSpec {
            id: "candles.matching_low",
            version: "1.0.0",
            library: Library::Candles,
            name: "Matching Low",
            description: "two closes at the same low -- the level absorbed the second try",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::MatchingLow],
        },
        ConceptSpec {
            id: "candles.on_neck",
            version: "1.0.0",
            library: Library::Candles,
            name: "On-Neck",
            description: "the down bar's close met by an up bar closing just above it -- pressure, not reversal yet",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::OnNeck],
        },
        ConceptSpec {
            id: "candles.three_inside",
            version: "1.0.0",
            library: Library::Candles,
            name: "Three Inside Up / Down",
            description: "a harami confirmed by the third bar closing through -- the pause resolves",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ThreeInside],
        },
        ConceptSpec {
            id: "candles.three_outside",
            version: "1.0.0",
            library: Library::Candles,
            name: "Three Outside Up / Down",
            description: "an engulfing confirmed by the third bar continuing -- the takeover holds",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ThreeOutside],
        },
        ConceptSpec {
            id: "candles.three_methods",
            version: "1.0.0",
            library: Library::Candles,
            name: "Rising / Falling Three Methods",
            description: "small counter-bars inside the first bar's body, then the trend resumes -- the textbook continuation",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::ThreeMethods],
        },
        ConceptSpec {
            id: "candles.abandoned_baby",
            version: "1.0.0",
            library: Library::Candles,
            name: "Abandoned Baby",
            description: "a doji gapped away from both neighbors -- the island top or bottom",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::AbandonedBaby],
        },
        ConceptSpec {
            id: "candles.tri_star",
            version: "1.0.0",
            library: Library::Candles,
            name: "Tri-Star",
            description: "three consecutive dojis -- total indecision at the extreme",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::TriStar],
        },
        ConceptSpec {
            id: "candles.stick_sandwich",
            version: "1.0.0",
            library: Library::Candles,
            name: "Stick Sandwich",
            description: "two same-close bars with an opposite bar between -- the market keeps re-testing one price",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::StickSandwich],
        },
        ConceptSpec {
            id: "candles.mat_hold",
            version: "1.0.0",
            library: Library::Candles,
            name: "Mat Hold",
            description: "the gap that never fills while small bars try -- strength the pullbacks cannot touch",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::MatHold],
        },
        ConceptSpec {
            id: "candles.tasuki_gap",
            version: "1.0.0",
            library: Library::Candles,
            name: "Tasuki Gap",
            description: "the gap window after a strong bar, tested but not closed",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::TasukiGap],
        },
        ConceptSpec {
            id: "candles.ladder_bottom",
            version: "1.0.0",
            library: Library::Candles,
            name: "Ladder Bottom",
            description: "consecutive down bars then a gap up -- the decline lost its stairs",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::LadderBottom],
        },
        ConceptSpec {
            id: "candles.belt_hold",
            version: "1.0.0",
            library: Library::Candles,
            name: "Belt-Hold",
            description: "a long body opening at the extreme and closing near the other -- one side owned the whole bar",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::BeltHold],
        },
        ConceptSpec {
            id: "candles.hikkake",
            version: "1.0.0",
            library: Library::Candles,
            name: "Hikkake",
            description: "the inside-bar trap: a false break one way that springs the other",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Hikkake],
        },
        ConceptSpec {
            id: "pa.breakout",
            version: "1.0.0",
            library: Library::PriceAction,
            name: "Breakout",
            description: "a close beyond the recent 20-bar extreme -- the range gave way, in the close's direction",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Breakout],
        },
        ConceptSpec {
            id: "pa.failed_breakout",
            version: "1.0.0",
            library: Library::PriceAction,
            name: "Failed Breakout",
            description: "closed beyond the extreme, then closed back inside -- the break was rejected, and the other side owns it now",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::FailedBreakout],
        },
        ConceptSpec {
            id: "pa.retest",
            version: "1.0.0",
            library: Library::PriceAction,
            name: "Breakout Retest",
            description: "broke out, came back to the level, and held it -- the broken boundary works as its new role",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::Retest],
        },
        ConceptSpec {
            id: "pa.higher_highs",
            version: "1.0.0",
            library: Library::PriceAction,
            name: "Higher Highs / Lower Lows",
            description: "the newest swing highs AND lows stepping the same way -- the trend's structure, confirmed by both sides of it",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::HigherHighs],
        },
        ConceptSpec {
            id: "pa.range_contraction",
            version: "1.0.0",
            library: Library::PriceAction,
            name: "Range Contraction",
            description: "the tightest ten-bar span of its recent peers -- compression, the state expansion is born from",
            parameters: &[],
            required_channels: DataChannels::CANDLES,
            pane: false,
            overlay: false,
            event_kinds: &[EventKind::RangeContraction],
        },
    ]
}

/// One concept by id.
#[must_use]
pub fn concept(id: &str) -> Option<&'static ConceptSpec> {
    concept_registry().iter().find(|c| c.id == id)
}

/// The concepts available on one symbol, by the channel rule: a concept is
/// available exactly when the symbol's product serves every channel the
/// concept requires. Bare symbols parse as binance:spot.
#[must_use]
pub fn available_for(symbol: &str) -> Vec<&'static ConceptSpec> {
    let parsed = crate::broker::VenueSymbol::parse(symbol);
    // Unknown venues get the honest floor: candles only, nothing claimed.
    let channels = parsed.channels().unwrap_or(DataChannels::CANDLES);
    concept_registry()
        .iter()
        .filter(|c| channels.supports(c.required_channels))
        .collect()
}

/// Wrap one analytics event in the contract. Returns None when no registered
/// concept emits that kind (every kind in the engine's list is wrapped, so a
/// None means the catalog drifted from the engine -- the test below pins it).
#[must_use]
pub fn as_concept_event(event: &MarketEvent) -> Option<ConceptEvent> {
    let concept = concept_registry()
        .iter()
        .find(|c| c.event_kinds.contains(&event.kind))?;
    let direction = match event.side {
        // A break carries the direction of the close; a sweep carries the
        // liquidity's side, and taken sell-side liquidity is bullish evidence.
        Some(Side::Buy) if event.kind == EventKind::LiquiditySweep => Direction::Bearish,
        Some(Side::Sell) if event.kind == EventKind::LiquiditySweep => Direction::Bullish,
        Some(Side::Buy) => Direction::Bullish,
        Some(Side::Sell) => Direction::Bearish,
        None => Direction::Neutral,
    };
    let status = match event.kind {
        EventKind::FvgCreated | EventKind::BprCreated => ConceptStatus::Forming,
        EventKind::FvgFilled => ConceptStatus::Expired,
        EventKind::FvgInvalidated => ConceptStatus::Invalidated,
        _ => ConceptStatus::Confirmed,
    };
    let mut evidence = serde_json::Map::new();
    evidence.insert("close".to_string(), serde_json::json!(event.close));
    if let Some(magnitude) = event.magnitude {
        evidence.insert("magnitude".to_string(), serde_json::json!(magnitude));
    }
    Some(ConceptEvent {
        concept_id: concept.id.to_string(),
        concept_version: concept.version.to_string(),
        symbol: event.symbol.clone(),
        timeframe: event.timeframe.to_string(),
        direction,
        status,
        event_time_ns: event.bar_time,
        price_level: Some(event.price),
        zone_low: event.zone_low,
        zone_high: event.zone_high,
        evidence: serde_json::Value::Object(evidence),
        bars: (event.formed_at.unwrap_or(event.bar_time), event.bar_time),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_integrity_checked() {
        let registry = concept_registry();
        assert!(registry.len() >= 7, "the first catalog is seeded");
        let mut ids: Vec<&str> = registry.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "ids are unique");
        for c in registry {
            assert!(c.id.starts_with(c.library.prefix()), "{} under its library", c.id);
            assert!(c.version.split('.').count() == 3, "{} semver", c.id);
            // Every declared kind is wrap-able: the catalog must not drift
            // from the engine it consumes.
            for kind in c.event_kinds {
                assert!(EventKind::ALL.contains(kind), "{kind:?} is an engine kind");
            }
        }
    }

    #[test]
    fn channel_gating_keeps_order_flow_off_synthetics() {
        let deriv = available_for("DERIV:VOLATILITY 10");
        assert!(
            deriv.iter().any(|c| c.id == "smc.bos"),
            "candle concepts run on synthetics"
        );
        assert!(
            !deriv.iter().any(|c| c.id == "order_flow.delta_shift"),
            "delta needs aggressor trades a synthetic does not have"
        );
        let spot = available_for("BTCUSDT");
        assert!(spot.iter().any(|c| c.id == "order_flow.delta_shift"));
    }

    #[test]
    fn the_contract_wraps_a_bos_with_levels_and_evidence() {
        let event = MarketEvent {
            kind: EventKind::Bos,
            symbol: "BTCUSDT".to_string(),
            timeframe: analytics_core::types::Timeframe::M15,
            bar_time: 1_700_000_000_000_000_000,
            close: 108_260.0,
            price: 108_250.0,
            side: Some(Side::Buy),
            zone_low: None,
            zone_high: None,
            magnitude: None,
            formed_at: Some(1_699_999_000_000_000_000),
        };
        let wrapped = as_concept_event(&event).expect("a bos wraps");
        assert_eq!(wrapped.concept_id, "smc.bos");
        assert_eq!(wrapped.concept_version, "1.0.0");
        assert_eq!(wrapped.direction, Direction::Bullish);
        assert_eq!(wrapped.status, ConceptStatus::Confirmed);
        assert_eq!(wrapped.price_level, Some(108_250.0));
        assert_eq!(wrapped.bars.0, 1_699_999_000_000_000_000);
        let json = serde_json::to_string(&wrapped).expect("serializes");
        assert!(json.contains("\"concept_id\":\"smc.bos\""));
        assert!(json.contains("\"direction\":\"bullish\""));

        // A sell-side sweep reads bullish (the liquidity's side was taken).
        let sweep = MarketEvent {
            kind: EventKind::LiquiditySweep,
            side: Some(Side::Sell),
            zone_low: Some(100.0),
            zone_high: Some(101.0),
            ..event.clone()
        };
        let wrapped = as_concept_event(&sweep).expect("a sweep wraps");
        assert_eq!(wrapped.concept_id, "smc.liquidity_sweep");
        assert_eq!(wrapped.direction, Direction::Bullish);
        assert_eq!(wrapped.zone_low, Some(100.0));

        // An FVG is Forming at creation and Expired at fill.
        let fvg = MarketEvent { kind: EventKind::FvgCreated, ..event };
        assert_eq!(as_concept_event(&fvg).expect("wraps").status, ConceptStatus::Forming);
        let fvg = MarketEvent { kind: EventKind::FvgFilled, ..fvg };
        assert_eq!(as_concept_event(&fvg).expect("wraps").status, ConceptStatus::Expired);
    }
}
