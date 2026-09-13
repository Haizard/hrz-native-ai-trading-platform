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
use crate::market_structure::{detect_market_structure, MarketStructure, StructureConfig, Trend};
use crate::types::{Candle, Trade};
use crate::volume_profile::{
    calculate_volume_profile, calculate_volume_profile_from_candles, VolumeProfile,
};
use crate::vwap::calculate_vwap_series;

/// Tuning for [`build_market_state`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarketStateConfig {
    /// Price bucket width for the volume profile and footprints.
    pub bucket_size: f64,
    /// Restart CVD and VWAP at each UTC day boundary.
    pub cvd_reset_at_session: bool,
    /// Trailing candles compared when looking for CVD divergence.
    pub divergence_lookback: usize,
    /// How many trailing candles get a full footprint.
    pub footprint_candles: usize,
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
            structure: StructureConfig::default(),
            imbalance: ImbalanceConfig::default(),
            absorption: AbsorptionConfig::default(),
            liquidity: LiquidityConfig::default(),
        }
    }
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

    Some(assemble(
        last, cvd, vwap, &profile, divergence, &structure, imbalances, absorption, liquidity,
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
    imbalances: Vec<ImbalanceEvent>,
    absorption: Vec<AbsorptionEvent>,
    liquidity: Vec<LiquidityLevel>,
) -> MarketState {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

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
