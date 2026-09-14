//! # `analytics-core`
//!
//! The deterministic heart of the platform (principle #1 in
//! `00-VISION-AND-PRINCIPLES.md`).
//!
//! Every trading calculation the platform performs lives here **once** and is
//! reused by the backend, the WASM frontend, the backtester and the sandbox.
//! That is what guarantees:
//!
//! > what you see on the chart == what the backtester tested == what the bot executed
//!
//! ## Hard constraints
//!
//! * No `async`, no network, no filesystem, no database access.
//! * Every function is pure: same input => same output.
//! * Must compile unchanged for `wasm32-unknown-unknown`.
//!
//! ## Module map
//!
//! | Module | Answers |
//! |---|---|
//! | [`delta`] | who was aggressive in this candle |
//! | [`cvd`] | is aggression accumulating or diverging from price |
//! | [`vwap`] | where is the volume-weighted fair price |
//! | [`volume_profile`] | at which prices did volume actually trade |
//! | [`footprint`] | bid vs ask volume at every price inside a candle |
//! | [`imbalance`] | where is one side overwhelming the other |
//! | [`absorption`] | where is aggression failing to move price |
//! | [`liquidity`] | where are the resting stops |
//! | [`market_structure`] | swing points, BOS and CHoCH |
//! | [`indicators`] | SMA, EMA, RSI, ATR |
//! | [`state`] | the `MarketState` aggregate the AI agent's tools return |
//!
//! ## Conventions
//!
//! * Timestamps are unix **nanoseconds, UTC**.
//! * Indicators return `Option<f64>` with `None` for the warm-up period rather
//!   than `NaN` -- `NaN` would propagate silently into the Strategy DSL and
//!   into order sizing.
//! * Functions are pure and allocation-light; nothing here holds a lock or
//!   keeps hidden global state.
//!
//! See `docs/05-ANALYTICS-ENGINE.md`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod absorption;
pub mod cvd;
pub mod delta;
pub mod error;
pub mod footprint;
pub mod imbalance;
pub mod indicators;
pub mod liquidity;
pub mod market_structure;
pub mod resample;
pub mod state;
pub mod types;
pub mod volume_profile;
pub mod vwap;

pub use absorption::{detect_absorption, AbsorptionConfig, AbsorptionEvent};
pub use cvd::{calculate_cvd, detect_cvd_divergence, Cvd, CvdDivergence};
pub use delta::{calculate_delta, calculate_delta_from_trades, calculate_deltas, DeltaReading};
pub use error::AnalyticsError;
pub use footprint::{
    build_footprint, build_footprint_for_candle, build_footprint_from_candle, build_footprints,
    FootprintCandle,
};
pub use imbalance::{detect_imbalances, detect_imbalances_with, ImbalanceConfig, ImbalanceEvent};
pub use liquidity::{
    detect_liquidity_levels, detect_liquidity_levels_with, LiquidityConfig, LiquidityKind,
    LiquidityLevel,
};
pub use market_structure::{
    detect_market_structure, BreakKind, MarketStructure, StructureBreak, StructureConfig,
    SwingKind, SwingPoint, Trend,
};
pub use resample::{resample, resample_all};
pub use state::{build_market_state, MarketState, MarketStateConfig};
pub use types::{Candle, FootprintCell, OrderBookLevel, OrderBookSnapshot, Side, Timeframe, Trade};
pub use volume_profile::{
    round_bucket,
    calculate_volume_profile, calculate_volume_profile_from_candles, VolumeNode, VolumeProfile,
};
pub use vwap::{calculate_anchored_vwap, calculate_vwap, calculate_vwap_from_trades, Vwap};

/// The pieces almost every consumer needs, in one import.
///
/// ```
/// use analytics_core::prelude::*;
/// ```
pub mod prelude {
    pub use crate::absorption::{detect_absorption, AbsorptionConfig, AbsorptionEvent};
    pub use crate::cvd::{calculate_cvd, detect_cvd_divergence, Cvd, CvdDivergence};
    pub use crate::delta::{calculate_delta, calculate_deltas, DeltaReading};
    pub use crate::error::AnalyticsError;
    pub use crate::footprint::{build_footprints, FootprintCandle};
    pub use crate::imbalance::{detect_imbalances, ImbalanceConfig, ImbalanceEvent};
    pub use crate::indicators::{atr, ema, rsi, sma, true_range};
    pub use crate::liquidity::{detect_liquidity_levels, LiquidityLevel};
    pub use crate::market_structure::{detect_market_structure, BreakKind, MarketStructure, Trend};
    pub use crate::resample::{resample, resample_all};
    pub use crate::state::{build_market_state, MarketState, MarketStateConfig};
    pub use crate::types::{Candle, FootprintCell, Side, Timeframe, Trade};
    pub use crate::volume_profile::{calculate_volume_profile, VolumeProfile};
    pub use crate::vwap::{calculate_vwap, typical_price};
}
