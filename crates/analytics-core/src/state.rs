//! `MarketState` -- the single structured snapshot the AI agent reasons over.
//!
//! Every other module in this crate answers one question. This one answers all
//! of them at once and hands back a flat, serializable object: price, delta,
//! CVD, the volume profile levels, the structural trend, and the recent
//! imbalances, absorption and liquidity.
//!
//! That shape is deliberate. The AI Agent Engine's tools return a
//! `MarketState`, and the agent then *reasons* over it -- it never recomputes
//! any of the numbers (principle #1: the LLM reasons, the deterministic core
//! calculates). If a field is missing here, the agent's only option is to
//! guess, so the aggregate errs towards including too much rather than too
//! little.
//!
//! ## Cost
//!
//! Footprint-level analysis needs per-trade data, and building footprints for
//! thousands of candles is not free. Only the last
//! [`MarketStateConfig::footprint_candles`] candles get a footprint; the volume
//! profile and structure use the full window.

use serde::{Deserialize, Serialize};

use crate::absorption::{detect_absorption, AbsorptionConfig, AbsorptionEvent};
use crate::cvd::{calculate_cvd, detect_cvd_divergence, CvdDivergence};
use crate::footprint::{build_footprint_from_candle, build_footprints, FootprintCandle};
use crate::imbalance::{detect_imbalances_with, ImbalanceConfig, ImbalanceEvent};
use crate::liquidity::{
    detect_liquidity_levels_with, nearest_liquidity_above, nearest_liquidity_below,
    LiquidityConfig, LiquidityLevel,
};
use crate::market_structure::{
    detect_market_structure, MarketStructure, StructureBreak, StructureConfig, Trend,
};
use crate::rsi_divergence::{latest_rsi_divergence, RsiDivergence, RsiDivergenceConfig};
use crate::sessions::{SessionEngine, SessionKind, SessionWindow};
use crate::types::{Candle, Trade};
use crate::volume_score::{latest_volume_score, VolumeScoreConfig};
use crate::volume_profile::{
    calculate_volume_profile, calculate_volume_profile_from_candles, VolumeProfile,
};
use crate::vwap::calculate_vwap_series;

/// Tuning for [`build_market_state`].
///
/// Not `Copy` since the session windows became part of the tuning: a `Vec`
/// cannot be copied, and cloning a config is a rare, cold-path event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketStateConfig {
    /// Price bucket width for the volume profile and footprints.
    pub bucket_size: f64,
    /// Restart CVD and VWAP at each UTC day boundary.
    pub cvd_reset_at_session: bool,
    /// Trailing candles compared when looking for CVD divergence.
    pub divergence_lookback: usize,
    /// How many trailing candles get a full footprint.
    pub footprint_candles: usize,
    /// How many recent structural breaks the state carries.
    ///
    /// Bounded because the state is serialized into the agent's prompt: a
    /// 200-candle window can break a dozen levels, and the tenth-oldest break
    /// costs tokens to say nothing the newest one does not.
    pub structure_breaks: usize,
    /// RSI/price divergence tuning.
    pub rsi_divergence: RsiDivergenceConfig,
    /// Volume-weighted aggression score tuning.
    pub volume_score: VolumeScoreConfig,
    /// The session windows a candle is matched against, earliest wins.
    pub session_windows: Vec<SessionWindow>,
    /// Swing-detection tuning.
    pub structure: StructureConfig,
    /// Imbalance tuning.
    pub imbalance: ImbalanceConfig,
    /// Absorption tuning.
    pub absorption: AbsorptionConfig,
    /// Liquidity tuning.
    pub liquidity: LiquidityConfig,
}

impl Default for MarketStateConfig {
    fn default() -> Self {
        Self {
            bucket_size: 10.0,
            cvd_reset_at_session: true,
            divergence_lookback: 20,
            footprint_candles: 200,
            structure_breaks: 8,
            rsi_divergence: RsiDivergenceConfig::default(),
            volume_score: VolumeScoreConfig::default(),
            session_windows: SessionWindow::defaults().to_vec(),
            structure: StructureConfig::default(),
            imbalance: ImbalanceConfig::default(),
            absorption: AbsorptionConfig::default(),
            liquidity: LiquidityConfig::default(),
        }
    }
}

/// What the volume profile was computed from.
///
/// The two are not the same answer: trades place real volume at real prices,
/// while candles spread each bar's volume uniformly across its range -- fine
/// for the value-area shape, not evidence for per-level order flow. Named in
/// the state so a reader never has to guess which it is looking at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileBasis {
    /// Built from the trades behind the window.
    Trades,
    /// Spread from candles, because no trades were in the window.
    #[default]
    Candles,
}

/// The data the state was actually built from (`docs/39`).
///
/// ## Why this block exists
///
/// Every section of a `MarketState` is a claim about the inputs, and two of
/// the inputs are optional in practice: trades (often absent for older
/// windows) and a candle buy/sell split that means something (a venue fact
/// this crate cannot see -- attribution happens at the venue boundary, and
/// `Candle` carries plain numbers). What this crate *can* see is presence:
/// how many bars, how many trades, and therefore which sections could say
/// anything at all. That is what the block records, computed in
/// [`build_market_state`] where the slices are still in hand.
///
/// What it deliberately does **not** record is venue fidelity ("is this split
/// real or attributed"): that is a declaration about the provider, not a
/// property of a slice, and it lives in the capability registry
/// (`capabilities` crate). The two meet at render time in the agent's tool
/// layer -- this block says *what was read*, the registry says *what the
/// provider can honestly supply*, and neither pretends to know the other's
/// half.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateProvenance {
    /// Candles in the window the state was built from.
    pub window_bars: usize,
    /// Trades in that window. Zero is the load-bearing case: it makes every
    /// footprint-level section structurally empty rather than "quiet".
    pub trades: usize,
    /// What the volume profile rests on.
    pub profile: ProfileBasis,
    /// Whether footprint-level sections (imbalances, absorption) could detect
    /// anything: exactly `trades > 0`. Carried rather than derived at the
    /// render site so the label and the detector can never disagree about why
    /// the list is empty.
    pub footprint_level: bool,
}

/// A complete order-flow read of one symbol on one timeframe.
///
/// All fields are finite: nothing in this crate emits `NaN` or an infinity into
/// the state, because the agent's tools serialize it and a `NaN` would either
/// fail serialization or silently poison the model's reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketState {
    /// Symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Timeframe as its exchange-style string, e.g. `"1m"`.
    pub timeframe: String,
    /// Open time of the newest candle, unix nanoseconds UTC.
    pub timestamp: i64,
    /// Last traded price (the newest candle's close).
    pub price: f64,
    /// Newest candle's delta.
    pub delta: f64,
    /// Cumulative volume delta over the window.
    pub cvd: f64,
    /// Session VWAP at the newest candle, if any volume traded.
    pub vwap: Option<f64>,
    /// Volume profile Point of Control.
    pub poc: f64,
    /// Volume profile Value Area High.
    pub vah: f64,
    /// Volume profile Value Area Low.
    pub val: f64,
    /// Newest candle's total volume.
    pub volume: f64,
    /// Newest candle's buy-aggressed volume.
    pub buy_volume: f64,
    /// Newest candle's sell-aggressed volume.
    pub sell_volume: f64,
    /// CVD/price divergence over the trailing window.
    pub divergence: CvdDivergence,
    /// Structural trend.
    pub trend: Trend,
    /// Imbalances found in the recent footprints.
    pub imbalances: Vec<ImbalanceEvent>,
    /// Absorption events found in the recent footprints.
    pub absorption: Vec<AbsorptionEvent>,
    /// Detected liquidity levels.
    pub liquidity: Vec<LiquidityLevel>,
    /// Confirmed swing-high prices, oldest first.
    pub swing_highs: Vec<f64>,
    /// Confirmed swing-low prices, oldest first.
    pub swing_lows: Vec<f64>,
    /// Recent BOS/CHoCH breaks, oldest first.
    ///
    /// ## Why the breaks are carried and not just the trend
    ///
    /// `detect_market_structure` has always computed these. `MarketState` kept
    /// `trend` and the bare swing prices and dropped the breaks -- so the
    /// aggregate lost a deliverable of the module it was aggregating, both of
    /// them inside this crate.
    ///
    /// The loss is not cosmetic. A close above the last swing high *while the
    /// trend was up* is a BOS: continuation, and the usual place to add. The
    /// same close *while the trend was down* is a CHoCH: reversal evidence, and
    /// the usual place to reverse. With only `trend`, both reads arrive as
    /// "bullish" and the agent cannot tell which it is looking at -- which is
    /// precisely the distinction a structure-based method is built on.
    ///
    /// Bounded to [`MarketStateConfig::structure_breaks`], most recent last.
    pub breaks: Vec<StructureBreak>,
    /// Bars between the newest candle and the most recent break.
    ///
    /// Stored rather than derived: [`StructureBreak::index`] counts into the
    /// candle slice the detector was handed, and by the time anything reads the
    /// state that slice is gone. `None` when nothing has broken.
    pub bars_since_break: Option<usize>,
    /// The session the newest candle belongs to, if any.
    ///
    /// Crypto trades 24/7, so `None` is off-session -- a fact, not a failure.
    /// The three aggregates below reset at every session boundary, which is
    /// what makes today's London VWAP comparable to yesterday's.
    #[serde(default)]
    pub session: Option<SessionKind>,
    /// VWAP accumulated within the current session; `None` off-session or
    /// before any volume traded.
    #[serde(default)]
    pub session_vwap: Option<f64>,
    /// The session's first open; `None` off-session.
    #[serde(default)]
    pub session_open: Option<f64>,
    /// The session's cumulative delta.
    #[serde(default)]
    pub session_delta: Option<f64>,
    /// The most recent RSI/price divergence in the lookback window.
    #[serde(default)]
    pub rsi_divergence: Option<RsiDivergence>,
    /// The newest candle's volume-weighted aggression score, `[-1, 1]`.
    /// `None` while the volume baseline is still warming up.
    #[serde(default)]
    pub volume_score: Option<f64>,
    /// What this state was built from (`docs/39`).
    ///
    /// `serde(default)` because the state is stored as JSON in places that
    /// predate the block: an old row must still load, and reads as "provenance
    /// unknown" rather than failing.
    #[serde(default)]
    pub provenance: StateProvenance,
}

impl MarketState {
    /// Whether price is inside the value area.
    #[must_use]
    pub fn in_value_area(&self) -> bool {
        self.val <= self.price && self.price <= self.vah
    }

    /// Whether price is above VWAP; `None` when VWAP is unavailable.
    #[must_use]
    pub fn above_vwap(&self) -> Option<bool> {
        self.vwap.map(|vwap| self.price > vwap)
    }

    /// Signed distance from the POC as a fraction of the POC.
    #[must_use]
    pub fn poc_deviation(&self) -> Option<f64> {
        if self.poc.abs() < f64::EPSILON {
            return None;
        }
        Some((self.price - self.poc) / self.poc)
    }

    /// Nearest liquidity level above the current price.
    #[must_use]
    pub fn nearest_liquidity_above(&self) -> Option<&LiquidityLevel> {
        nearest_liquidity_above(&self.liquidity, self.price)
    }

    /// Nearest liquidity level below the current price.
    #[must_use]
    pub fn nearest_liquidity_below(&self) -> Option<&LiquidityLevel> {
        nearest_liquidity_below(&self.liquidity, self.price)
    }

    /// Sum of the signed imbalance volumes: positive means buyers dominated
    /// the recent footprint levels overall.
    #[must_use]
    pub fn net_imbalance_volume(&self) -> f64 {
        self.imbalances
            .iter()
            .map(|e| if e.is_buy() { e.volume } else { -e.volume })
            .sum()
    }

    /// The most recent absorption event, if any.
    #[must_use]
    pub fn latest_absorption(&self) -> Option<&AbsorptionEvent> {
        self.absorption.last()
    }

    /// The most recent structural break, if any.
    #[must_use]
    pub fn latest_break(&self) -> Option<&StructureBreak> {
        self.breaks.last()
    }

    /// How many bars ago the most recent break happened.
    ///
    /// `None` when nothing has broken; `Some(0)` means it happened on the
    /// newest candle. Recency is most of what makes a break actionable -- a
    /// CHoCH nine bars ago is history, the same CHoCH on the current bar is a
    /// decision -- and the state carries no other way to tell them apart,
    /// because [`StructureBreak::index`] is an index into a candle slice the
    /// caller no longer holds.
    #[must_use]
    pub fn bars_since_break(&self) -> Option<usize> {
        self.bars_since_break
    }

    /// Signed distance from the newest close to the most recent break's level.
    ///
    /// Positive means price has held above the broken level. `None` when
    /// nothing has broken -- absent rather than zero, so a comparison against
    /// it is false rather than true against a price of zero.
    #[must_use]
    pub fn break_distance(&self) -> Option<f64> {
        self.breaks.last().map(|b| self.price - b.level)
    }

    /// Whether the newest candle is part of the value area and above VWAP --
    /// the cheap definition of "healthy uptrend context".
    #[must_use]
    pub fn is_constructive(&self) -> bool {
        self.in_value_area() && self.above_vwap().unwrap_or(false)
    }
}

/// Build a [`MarketState`] from candles and (optionally) the trades behind them.
///
/// Returns `None` when `candles` is empty -- there is no state to describe.
///
/// `trades` may be empty, in which case the volume profile and footprints are
/// derived from candles and no footprint-level signal (imbalance, absorption)
/// can be found. When it is non-empty it must be sorted ascending by timestamp.
///
/// # Example
///
/// ```
/// use analytics_core::state::{build_market_state, MarketStateConfig};
/// use analytics_core::types::{Candle, Timeframe};
///
/// fn c(high: f64, low: f64, close: f64) -> Candle {
///     Candle {
///         symbol: "BTCUSDT".into(),
///         timeframe: Timeframe::M1,
///         open_time: 0,
///         open: close,
///         high,
///         low,
///         close,
///         volume: 2.0,
///         buy_volume: 1.5,
///         sell_volume: 0.5,
///     }
/// }
///
/// let candles = vec![
///     c(10.0, 8.0, 9.0),
///     c(12.0, 9.0, 11.0),
///     c(11.0, 9.0, 10.0),
///     c(13.0, 10.0, 13.0),
/// ];
/// let state = build_market_state(&candles, &[], &MarketStateConfig::default()).unwrap();
///
/// assert_eq!(state.symbol, "BTCUSDT");
/// assert_eq!(state.timeframe, "1m");
/// assert!((state.price - 13.0).abs() < 1e-9);
/// assert!((state.delta - 1.0).abs() < 1e-9);
/// // No trades => no footprint-level signals.
/// assert!(state.imbalances.is_empty());
/// ```
#[must_use]
pub fn build_market_state(
    candles: &[Candle],
    trades: &[Trade],
    config: &MarketStateConfig,
) -> Option<MarketState> {
    let last = candles.last()?;

    let cvd_series = calculate_cvd(candles, config.cvd_reset_at_session);
    let cvd = cvd_series.last().copied().unwrap_or(0.0);
    let divergence = detect_cvd_divergence(candles, &cvd_series, config.divergence_lookback);

    let vwap = calculate_vwap_series(candles, config.cvd_reset_at_session)
        .last()
        .copied()
        .flatten();

    let profile = if trades.is_empty() {
        calculate_volume_profile_from_candles(candles, config.bucket_size)
    } else {
        calculate_volume_profile(trades, config.bucket_size)
    };

    let structure = detect_market_structure(candles, config.structure);
    let liquidity = detect_liquidity_levels_with(candles, config.liquidity);

    let footprints = recent_footprints(candles, trades, config);

    // Footprint-level signals need tick data. A candle-derived footprint
    // reproduces the candle's aggregate buy/sell ratio at every level, so
    // running the detector over it would invent a stack of imbalances that
    // nothing in the data supports. Skip it entirely instead.
    let mut imbalances = Vec::new();
    if !trades.is_empty() {
        for footprint in &footprints {
            imbalances.extend(detect_imbalances_with(footprint, &config.imbalance));
        }
    }
    let absorption = detect_absorption(&footprints, config.absorption);

    // Session aggregates walk the whole window so the values describe *this*
    // session so far, not the last candle's slice of it.
    let mut session_engine = SessionEngine::new(config.session_windows.clone());
    for candle in candles {
        session_engine.update(candle);
    }
    let rsi_divergence = latest_rsi_divergence(candles, &config.rsi_divergence);
    let volume_score = latest_volume_score(candles, &config.volume_score);

    // Computed here, where the input slices are still in hand: by the time
    // `assemble` runs, how much data produced the state is otherwise lost.
    let provenance = StateProvenance {
        window_bars: candles.len(),
        trades: trades.len(),
        profile: if trades.is_empty() {
            ProfileBasis::Candles
        } else {
            ProfileBasis::Trades
        },
        footprint_level: !trades.is_empty(),
    };

    Some(assemble(
        last,
        cvd,
        vwap,
        &profile,
        divergence,
        &structure,
        config.structure_breaks,
        imbalances,
        absorption,
        liquidity,
        session_engine,
        rsi_divergence,
        volume_score,
        provenance,
    ))
}

/// Footprints for the trailing window of candles.
fn recent_footprints(
    candles: &[Candle],
    trades: &[Trade],
    config: &MarketStateConfig,
) -> Vec<FootprintCandle> {
    let start = candles
        .len()
        .saturating_sub(config.footprint_candles.max(1));
    let window = &candles[start..];

    if trades.is_empty() {
        window
            .iter()
            .map(|candle| build_footprint_from_candle(candle, config.bucket_size))
            .collect()
    } else {
        build_footprints(window, trades, config.bucket_size)
    }
}

/// Assemble the state. Split out so the public function reads as a recipe.
#[allow(clippy::too_many_arguments)]
fn assemble(
    last: &Candle,
    cvd: f64,
    vwap: Option<f64>,
    profile: &VolumeProfile,
    divergence: CvdDivergence,
    structure: &MarketStructure,
    structure_breaks: usize,
    imbalances: Vec<ImbalanceEvent>,
    absorption: Vec<AbsorptionEvent>,
    liquidity: Vec<LiquidityLevel>,
    session: SessionEngine,
    rsi_divergence: Option<RsiDivergence>,
    volume_score: Option<f64>,
    provenance: StateProvenance,
) -> MarketState {
    // The newest break, and how far back it is. Computed here because this is
    // the last frame that knows how long the candle slice was; `index` alone
    // means nothing to a reader of the state.
    //
    // The age is derived from `breaks` *after* bounding, not from the full
    // structure. Otherwise a bound of zero leaves `latest_break()` returning
    // `None` while `bars_since_break()` still reports a number -- and a
    // strategy reading `market_structure.break_age` would act on a break that
    // `market_structure.break` says does not exist.
    let breaks = tail_of(&structure.breaks, structure_breaks);
    let bars_since_break = breaks.last().map(|b| {
        structure
            .candle_count
            .saturating_sub(1)
            .saturating_sub(b.index)
    });

    MarketState {
        symbol: last.symbol.clone(),
        timeframe: last.timeframe.to_string(),
        timestamp: last.open_time,
        price: last.close,
        delta: last.delta(),
        cvd,
        vwap,
        poc: profile.poc,
        vah: profile.vah,
        val: profile.val,
        volume: last.volume,
        buy_volume: last.buy_volume,
        sell_volume: last.sell_volume,
        divergence,
        trend: structure.trend,
        imbalances,
        absorption,
        liquidity,
        swing_highs: structure.swing_highs.clone(),
        swing_lows: structure.swing_lows.clone(),
        breaks,
        bars_since_break,
        session: session.session(),
        session_vwap: session.vwap(),
        session_open: session.open(),
        session_delta: session
            .session()
            .map(|_| session.delta()),
        rsi_divergence,
        volume_score,
        provenance,
    }
}

/// The newest `n` items of `items`, oldest first.
///
/// `n == 0` yields an empty vector rather than everything: a bound of zero is a
/// request for none, and reading it as "no bound" would put the whole break
/// history back into the prompt.
fn tail_of<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    let start = items.len().saturating_sub(n);
    items[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_structure::BreakKind;
    use crate::types::{Side, Timeframe};

    fn candle(open_time: i64, high: f64, low: f64, close: f64, buy: f64, sell: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time,
            open: close,
            high,
            low,
            close,
            volume: buy + sell,
            buy_volume: buy,
            sell_volume: sell,
        }
    }

    fn trade(price: f64, quantity: f64, buyer_maker: bool, timestamp: i64) -> Trade {
        Trade {
            symbol: "BTCUSDT".into(),
            trade_id: 0,
            price,
            quantity,
            is_buyer_maker: buyer_maker,
            timestamp,
        }
    }

    /// Fine buckets and a 1-bar swing lookback, so small fixtures exercise
    /// real structure instead of falling under the confirmation threshold.
    fn config() -> MarketStateConfig {
        MarketStateConfig {
            bucket_size: 1.0,
            structure: StructureConfig { lookback: 1 },
            liquidity: LiquidityConfig {
                lookback: 1,
                ..LiquidityConfig::default()
            },
            ..MarketStateConfig::default()
        }
    }

    /// A rising series with a clear swing high at 12 and a break above it.
    fn rising() -> Vec<Candle> {
        vec![
            candle(0, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(60, 12.0, 9.0, 11.0, 3.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 3.0),
            candle(180, 13.0, 10.0, 13.0, 4.0, 1.0),
        ]
    }

    #[test]
    fn empty_candles_yield_no_state() {
        assert!(build_market_state(&[], &[], &config()).is_none());
    }

    #[test]
    fn provenance_records_what_the_state_was_built_from() {
        let candles = rising();
        let without = build_market_state(&candles, &[], &config()).unwrap();
        assert_eq!(without.provenance.window_bars, 4);
        assert_eq!(without.provenance.trades, 0);
        assert_eq!(without.provenance.profile, ProfileBasis::Candles);
        assert!(
            !without.provenance.footprint_level,
            "no trades means footprint-level sections are structurally empty"
        );

        let trades = vec![trade(10.0, 1.0, true, 60), trade(11.0, 100.0, false, 61)];
        let with = build_market_state(&candles, &trades, &config()).unwrap();
        assert_eq!(with.provenance.trades, 2);
        assert_eq!(with.provenance.profile, ProfileBasis::Trades);
        assert!(with.provenance.footprint_level);
    }

    #[test]
    fn a_state_stored_before_provenance_existed_still_loads() {
        // The state is persisted as JSON in the bot lane; a row from before
        // this block must read as "provenance unknown", not fail to parse.
        let candles = rising();
        let state = build_market_state(&candles, &[], &config()).unwrap();
        let mut json = serde_json::to_value(&state).expect("serializes");
        json.as_object_mut().unwrap().remove("provenance");
        let loaded: MarketState = serde_json::from_value(json).expect("old rows still load");
        assert_eq!(loaded.provenance, StateProvenance::default());
        assert_eq!(loaded.provenance.profile, ProfileBasis::Candles);
    }

    #[test]
    fn state_reflects_the_newest_candle() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        assert_eq!(state.symbol, "BTCUSDT");
        assert_eq!(state.timeframe, "1m");
        assert_eq!(state.timestamp, 180);
        assert!((state.price - 13.0).abs() < 1e-9);
        assert!((state.delta - 3.0).abs() < 1e-9);
        assert!((state.volume - 5.0).abs() < 1e-9);
        assert!((state.buy_volume - 4.0).abs() < 1e-9);
        assert!((state.sell_volume - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cvd_is_cumulative_across_the_window() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        // deltas: 0, +2, -2, +3 -> 3
        assert!((state.cvd - 3.0).abs() < 1e-9);
    }

    #[test]
    fn structure_is_carried_through() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        assert_eq!(state.trend, Trend::Bullish);
        assert_eq!(state.swing_highs, vec![12.0]);
    }

    /// Two confirmed swing highs, each closed through on the next leg up.
    ///
    /// `rising()` happens to break exactly once, so it cannot show that the
    /// list is a *history* rather than a single slot.
    fn staircase() -> Vec<Candle> {
        vec![
            candle(0, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(60, 12.0, 9.0, 11.0, 1.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(180, 14.0, 10.0, 14.0, 1.0, 1.0),
            candle(240, 13.0, 11.0, 12.0, 1.0, 1.0),
            candle(300, 15.0, 12.0, 15.0, 1.0, 1.0),
        ]
    }

    /// The gap this closes: `detect_market_structure` computed the breaks,
    /// `MarketState` dropped them, and both live in this crate.
    ///
    /// `rising()`'s last candle closes at 13 through the swing high at 12, with
    /// `trend` still `Ranging` at the time of the break -- so the detector
    /// labels it a BOS. The state must now say so, and say *where*.
    #[test]
    fn breaks_are_carried_with_their_kind_and_level() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();

        assert_eq!(
            state.breaks.len(),
            1,
            "one close through the swing high at 12"
        );
        let brk = state
            .latest_break()
            .expect("the break must survive assembly");
        assert_eq!(brk.kind, BreakKind::Bos);
        assert_eq!(brk.direction, Side::Buy);
        assert!((brk.level - 12.0).abs() < 1e-9);
        assert!((brk.price - 13.0).abs() < 1e-9);
    }

    /// `break.index` is meaningless without the length of the slice it indexes
    /// into, so the age is computed while that length is still in hand.
    #[test]
    fn bars_since_break_counts_back_from_the_newest_candle() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        assert_eq!(
            state.bars_since_break(),
            Some(0),
            "the break is the newest candle in this fixture"
        );

        let older = build_market_state(&staircase(), &[], &config()).unwrap();
        assert_eq!(
            older.bars_since_break(),
            Some(0),
            "the second break is newest"
        );
    }

    #[test]
    fn a_state_with_no_break_reports_absence_not_zero() {
        // Two candles, one swing lookback: the window is too short for any
        // swing to be confirmed, let alone broken.
        let candles = vec![
            candle(0, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(60, 11.0, 9.0, 10.0, 1.0, 1.0),
        ];
        let state = build_market_state(&candles, &[], &config()).unwrap();

        assert!(state.breaks.is_empty());
        assert!(state.latest_break().is_none());
        // Absent, not `Some(0)`: a condition `market_structure.break_age < 3`
        // must be false on a series that never broke anything, and `Some(0)`
        // would make it true.
        assert_eq!(state.bars_since_break(), None);
        assert_eq!(state.break_distance(), None);
    }

    #[test]
    fn structure_breaks_bounds_the_carried_history() {
        let mut bounded = config();
        bounded.structure_breaks = 1;
        let state = build_market_state(&staircase(), &[], &bounded).unwrap();

        assert_eq!(state.breaks.len(), 1, "the bound must be honoured");
        let brk = state
            .latest_break()
            .expect("the newest break is the one kept");
        assert!(
            (brk.level - 14.0).abs() < 1e-9,
            "bounding keeps the *newest* breaks, not the oldest: got level {}",
            brk.level
        );

        // And the unbounded read really did see both, so the assertion above is
        // not passing because the fixture only ever produced one.
        assert_eq!(
            build_market_state(&staircase(), &[], &config())
                .unwrap()
                .breaks
                .len(),
            2
        );
    }

    /// A bound of zero means "carry none", not "carry everything".
    ///
    /// The distinction matters because the state is serialized into the agent's
    /// prompt: reading zero as unbounded would put the entire break history
    /// back in, which is the thing the bound exists to prevent.
    #[test]
    fn a_bound_of_zero_carries_no_breaks() {
        let mut none = config();
        none.structure_breaks = 0;
        let state = build_market_state(&staircase(), &[], &none).unwrap();

        assert!(state.breaks.is_empty());
        assert!(state.latest_break().is_none());
        assert_eq!(state.bars_since_break(), None);
        // `trend` is independent of the carried history, so it survives.
        assert_eq!(state.trend, Trend::Bullish);
    }

    #[test]
    fn break_distance_is_signed_from_the_newest_close() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        // Close 13, level 12: price held 1.0 above the broken level.
        let distance = state.break_distance().expect("a break exists");
        assert!((distance - 1.0).abs() < 1e-9);
    }

    #[test]
    fn profile_levels_bracket_the_poc() {
        let state = build_market_state(&rising(), &[], &config()).unwrap();
        assert!(state.val <= state.poc);
        assert!(state.poc <= state.vah);
    }

    #[test]
    fn trades_drive_the_volume_profile_when_present() {
        let candles = rising();
        let trades = vec![
            trade(10.0, 1.0, false, 0),
            trade(11.0, 5.0, false, 60),
            trade(13.0, 1.0, false, 180),
        ];
        let state = build_market_state(&candles, &trades, &config()).unwrap();
        // 1-wide buckets over 10..13 -> midpoints 10.5, 11.5, 12.5, 13.5.
        // Volume 1/5/0/1, so the POC is 11.5.
        assert!((state.poc - 11.5).abs() < 1e-9);
        // 5 of 7 total volume already clears 70%, so the value area is the POC.
        assert!((state.poc - state.vah).abs() < 1e-9);
    }

    #[test]
    fn footprint_signals_appear_only_with_trades() {
        let candles = rising();
        let no_trades = build_market_state(&candles, &[], &config()).unwrap();
        assert!(no_trades.imbalances.is_empty());
        assert!(no_trades.absorption.is_empty());

        // Both trades land in candle 1, one bucket apart: bid 1 at 10.5,
        // ask 100 at 11.5 -> a 100x diagonal buy imbalance.
        let trades = vec![trade(10.0, 1.0, true, 60), trade(11.0, 100.0, false, 61)];
        let with_trades = build_market_state(&candles, &trades, &config()).unwrap();
        assert!(
            !with_trades.imbalances.is_empty(),
            "a 100x level should register as an imbalance"
        );
        assert!(with_trades.imbalances.iter().any(ImbalanceEvent::is_buy));
    }

    #[test]
    fn vwap_is_none_without_volume() {
        let candles = vec![
            candle(0, 10.0, 10.0, 10.0, 0.0, 0.0),
            candle(60, 11.0, 11.0, 11.0, 0.0, 0.0),
        ];
        let state = build_market_state(&candles, &[], &config()).unwrap();
        assert!(state.vwap.is_none());
        assert!(state.above_vwap().is_none());
    }

    #[test]
    fn vwap_is_the_session_average_when_volume_exists() {
        let candles = vec![
            candle(0, 100.0, 100.0, 100.0, 1.0, 1.0),
            candle(60, 200.0, 200.0, 200.0, 1.0, 1.0),
        ];
        let state = build_market_state(&candles, &[], &config()).unwrap();
        // (100*2 + 200*2) / 4 = 150, and the newest close is 200.
        assert!((state.vwap.unwrap() - 150.0).abs() < 1e-9);
        assert_eq!(state.above_vwap(), Some(true));
    }

    #[test]
    fn every_reported_number_is_finite_and_serializable() {
        let candles = rising();
        let trades = vec![trade(10.0, 1.0, true, 60), trade(11.0, 100.0, false, 61)];
        let state = build_market_state(&candles, &trades, &config()).unwrap();

        let json = serde_json::to_string(&state).expect("state must serialize");
        assert!(json.contains("\"price\""), "unexpected json: {json}");

        assert!(state.price.is_finite());
        assert!(state.cvd.is_finite());
        assert!(state.poc.is_finite() && state.vah.is_finite() && state.val.is_finite());
        assert!(state.vwap.unwrap().is_finite());
        assert!(state.net_imbalance_volume().is_finite());
        for event in &state.imbalances {
            assert!(event.ratio.is_finite(), "ratio must not be an infinity");
        }
    }

    #[test]
    fn nearest_liquidity_helpers_are_relative_to_price() {
        // A swing high at 12 that a later candle sweeps.
        let candles = vec![
            candle(0, 10.0, 8.0, 9.0, 1.0, 1.0),
            candle(60, 12.0, 9.0, 11.0, 1.0, 1.0),
            candle(120, 11.0, 9.0, 10.0, 1.0, 1.0),
            candle(180, 13.0, 11.0, 12.0, 1.0, 1.0),
        ];
        let state = build_market_state(&candles, &[], &config()).unwrap();
        assert!(!state.liquidity.is_empty());
        if let Some(level) = state.nearest_liquidity_below() {
            assert!(level.price < state.price);
        }
        if let Some(level) = state.nearest_liquidity_above() {
            assert!(level.price > state.price);
        }
    }

    #[test]
    fn helpers_do_not_panic_on_degenerate_state() {
        let candles = vec![candle(0, 10.0, 10.0, 10.0, 0.0, 0.0)];
        let state = build_market_state(&candles, &[], &config()).unwrap();
        assert!(state.poc_deviation().is_none());
        assert!(!state.is_constructive());
        assert!(state.latest_absorption().is_none());
    }
}
