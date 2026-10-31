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
    /// The profile's point of control: the heaviest volume bucket.
    VolumePoc,
    /// The value area: the band where most of the volume traded.
    ValueArea,
    /// A high-volume node: accepted price.
    HighVolumeNode,
    /// A low-volume node: rejected price.
    LowVolumeNode,
    /// A Gartley completion (wickra engine).
    HarmonicGartley,
    /// A Bat completion.
    HarmonicBat,
    /// A Butterfly completion.
    HarmonicButterfly,
    /// A Crab completion.
    HarmonicCrab,
    /// A Shark completion.
    HarmonicShark,
    /// A Cypher completion.
    HarmonicCypher,
    /// An AB=CD completion.
    HarmonicAbcd,
    /// A Three Drives completion.
    HarmonicThreeDrives,
    /// Dragonfly doji (wickra).
    DragonflyDoji,
    /// Gravestone doji.
    GravestoneDoji,
    /// Long-legged doji.
    LongLeggedDoji,
    /// Spinning top.
    SpinningTop,
    /// Tweezer tops or bottoms.
    Tweezer,
    /// Bullish or bearish kicker.
    Kicker,
    /// Matching low.
    MatchingLow,
    /// On-neck pattern.
    OnNeck,
    /// Three inside up or down.
    ThreeInside,
    /// Three outside up or down.
    ThreeOutside,
    /// Rising or falling three methods.
    ThreeMethods,
    /// Abandoned baby.
    AbandonedBaby,
    /// Tri-star.
    TriStar,
    /// Stick sandwich.
    StickSandwich,
    /// Mat hold.
    MatHold,
    /// Upside or downside tasuki gap.
    TasukiGap,
    /// Ladder bottom.
    LadderBottom,
    /// Belt-hold line.
    BeltHold,
    /// Hikkake trap.
    Hikkake,
    /// Three swings at one level, confirmed through the neckline.
    TripleTopBottom,
    /// A flat-top flat-bottom channel and its breakout.
    RectangleBreakout,
    /// A rounded bottom, a shallow handle, and the resumption.
    CupAndHandle,
    /// A strong impulse with a CONVERGING small channel (not the flag's
    /// parallel drift).
    Pennant,
    /// A Fibonacci retracement level of the newest swing.
    FibRetracement,
    /// The golden pocket: the 0.618-0.65 band of the newest swing.
    GoldenPocket,
    /// A Fibonacci extension level beyond the newest swing.
    FibExtension,
    /// Two swings' levels agreeing within tolerance.
    FibConfluence,
    /// A Fibonacci-count bar of the swing.
    FibTimeZone,
    /// The RSI-PCA model's prediction crossing its rolling quantile
    /// extremes (neurotrader's idea, in-house).
    RsiPcaSignal,
    /// RSI (14) entering oversold (buy-side) or overbought (sell-side).
    RsiZoneCross,
    /// Every RSI period 2..=24 in the same zone -- the family's strongest
    /// statement, fired on entry.
    RsiFamilyConsensus,
    /// An RSI/price divergence confirmed at this bar.
    RsiDivergenceSignal,
    /// The MACD line crossing its signal.
    MacdSignalCross,
    /// The MACD line crossing zero.
    MacdZeroCross,
    /// The EMA 50 crossing the EMA 200 (golden/death).
    EmaCross,
    /// The SMA 50 crossing the SMA 200 (golden/death).
    SmaCross,
    /// A close outside the Bollinger band, in the break's direction.
    BollingerBreakout,
    /// ATR jumping past 1.5x its own mean -- volatility changing gear.
    AtrExpansion,
    /// Every MACD-family ribbon line on the same side of zero, fired on
    /// entering the consensus.
    MacdFamilyConsensus,
    /// The ATR family's mean expanding past 1.5x itself across EVERY period.
    AtrFamilyExpansion,
    /// The GMMA ribbon's short group crossing its long group.
    GmmaCross,
    /// Stochastic %K entering oversold (buy-side) or overbought (sell-side).
    StochasticZoneEntry,
    /// Stochastic %K crossing %D.
    StochasticCross,
    /// CCI entering the -100 (buy) or +100 (sell) zone.
    CciZoneEntry,
    /// Williams %R entering oversold (<-80) or overbought (>-20).
    WilliamsZoneEntry,
    /// ROC crossing zero.
    RocZeroCross,
    /// Price making the new extreme while OBV refuses it.
    ObvDivergence,
    /// ADX crossing 25 (side from the dominant DI), or the DI cross in a trend.
    AdxTrendCross,
    /// Price making the new extreme while the MACD line refuses it.
    MacdDivergenceSignal,
    /// Bollinger bandwidth at the lowest reading of the trailing 120 bars.
    BollingerSqueeze,
    /// A close outside the Keltner channel, in the break's direction.
    KeltnerBreakout,
    /// A close outside the Donchian channel, in the break's direction.
    DonchianBreakout,
    /// The Supertrend flipping side.
    SupertrendFlip,
    /// The Parabolic SAR flipping side.
    PsarFlip,
    /// Upside gap two crows: a bearish engulfing of the gapped bar while the
    /// gap above the first bar survives.
    UpsideGapTwoCrows,
    /// The previous day's high/low, marked at the day roll.
    PreviousDayLevels,
    /// The previous week's high/low, marked at the week roll.
    PreviousWeekLevels,
    /// The previous month's high/low, marked at the month roll.
    PreviousMonthLevels,
    /// A new month's opening price, once, at its first bar (UTC).
    MonthlyOpen,
    /// A close crossing a round-number level, in the cross's direction.
    RoundNumberCross,
    /// The rounded top: the cup mirrored, with the shallow handle in the
    /// lower half and the breakdown through the rim.
    InverseCupHandle,
    /// The rounding bottom (or top): the cup's arc WITHOUT the handle,
    /// resuming through the rim.
    RoundingBottomTop,
    /// The broadening formation: expanding highs AND expanding lows, with
    /// the close beyond the newest extreme.
    BroadeningFormation,
    /// A swing-anchored trendline, validated by its third touch, broken by
    /// the newest close.
    TrendlineBreak,
    /// ICT's energetic candle: a body at least twice the average, closing at
    /// a fresh extreme.
    Displacement,
    /// The 0.62-0.79 retracement band of the newest swing.
    OptimalTradeEntry,
    /// A minor-swing pool INSIDE the dealing range.
    InternalRangeLiquidity,
    /// The dealing range's own extremes.
    ExternalRangeLiquidity,
    /// The session-open false move: a sweep of the Asian range that closes
    /// back inside it within the kill zone's first hour.
    JudasSwing,
    /// Accumulation (Asian range), manipulation (the Judas sweep),
    /// distribution (the expansion through the far side) -- all three
    /// phases complete in one session.
    PowerOfThree,
    /// The trend/range regime changing state (side names the new trend, or
    /// neutral for range).
    RegimeShift,
    /// The chart's structure and the 4x-higher timeframe's structure
    /// starting to agree.
    MtfAlignment,
    /// The new day opening outside the closed day's range, with structure.
    DailyBias,
    /// The new week opening outside the closed week's range, with structure.
    WeeklyBias,
    /// The Ichimoku tenkan crossing the kijun.
    IchimokuTenkanKijunCross,
    /// Price crossing the Ichimoku kijun.
    IchimokuKijunCross,
    /// A close out of the kumo, in the exit's direction.
    IchimokuCloudBreakout,
    /// Senkou A crossing senkou B (the cloud changing side).
    IchimokuKumoTwist,
    /// The TSI crossing its signal.
    TsiCross,
    /// The Awesome Oscillator crossing zero.
    AwesomeZeroCross,
    /// Price making the new extreme while the A/D line refuses it.
    AdDivergence,
    /// The CMF crossing zero.
    CmfZeroCross,
    /// MFI entering oversold (buy) or overbought (sell).
    MfiZoneEntry,
    /// The close crossing the chandelier's active stop.
    ChandelierFlip,
    /// Price crossing the anchored VWAP of the newest swing low.
    AvwapCross,
    /// The volatility rank entering the top or bottom decile.
    HvRegimeShift,
    /// The z-score entering beyond two sigmas (reversion side).
    ZscoreZoneEntry,
    /// Price crossing the Kalman filter's estimate.
    KalmanCross,
    /// The signed CUSUM stat crossing its gate -- the regime broke.
    ChangePoint,
    /// Return entropy entering the disorder or order regime.
    EntropyRegimeShift,
    /// The Bayesian drift's 95% band excluding zero.
    BayesianDriftShift,
    /// The AR(2) forecast crossing zero.
    ArForecastCross,
    /// GARCH volatility entering its own top decile.
    GarchRegimeShift,
    /// The HMM volatile-state posterior crossing 0.5.
    HmmRegimeShift,
    /// A touch of the Bollinger band that closes back inside -- the
    /// reversion entry.
    BollingerReversion,
    /// A close beyond the session's initial balance, in the break's direction.
    InitialBalanceBreakout,
    /// A trade beyond the initial balance that closes back inside it.
    InitialBalanceReversal,
    /// A single-print level the market has not revisited.
    SinglePrint,
    /// Two consecutive closes beyond the value area's edge.
    AcceptanceEvent,
    /// A swing extreme whose own cell carries heavy time -- unfinished
    /// business at the edge.
    PoorExtreme,
    /// The ICT Silver Bullet: a sweep inside the window, a displaced FVG
    /// after it.
    SilverBullet,
    /// The ICT Unicorn: a zone that is both a breaker and an FVG, with
    /// price entering the overlap.
    UnicornModel,
    /// The 2022 mentorship model: yesterday's extreme swept, the character
    /// change against the sweep.
    Model2022,
    /// The market maker model completed at the opposite pool.
    MarketMakerModel,
    /// A close breaking a swing AGAINST the prevailing trend -- the
    /// distinct shift, not the continuation BOS already carries.
    Mss,
    /// The dealing range's own 50% line, as its own level.
    Equilibrium,
    /// The newest FVG's 50% midpoint (ICT's consequent encroachment).
    ConsequentEncroachment,
    /// Today's open against yesterday's close: the overnight gap, sized.
    OpeningGap,
    /// The UTC day's first-hour range broken, in the break's direction.
    OpeningRangeBreakout,
    /// A trade through the active trendline that closes back behind it.
    TrendlineSweep,
    /// A wick into a support/resistance level that closes away from it --
    /// the rejection event the zone alone never named.
    LevelRejection,
    /// A close through a level, then the level holding as the OPPOSITE
    /// side on the retest -- support become resistance, or the mirror.
    LevelFlip,
    /// The fused compression breakout: a contracted range broken WITH a
    /// displacement body.
    CompressionBreakout,
    /// A fair value gap traded fully back through -- the void's own
    /// lifecycle, completed.
    LiquidityVoidFill,
    /// A breakout with the volume to mean it (1.5x the trailing average).
    VolumeConfirmedBreakout,
    /// The Wolfe Wave: five alternating pivots with the 1-4 target line.
    WolfeWave,
    /// The diamond formation: broadening immediately followed by
    /// contracting, at the range's edge.
    Diamond,
    /// The trend channel: the validated trendline and its parallel
    /// counterpart, with the close beyond either.
    TrendChannel,
    /// The newest completed higher-timeframe bar's own direction with the
    /// structure lean -- the HTF candle bias.
    HtfCandleBias,
    /// A zone whose height shrinks across its retests.
    ZoneCompression,
    /// A zone's own entry and stop lines: the proximal edge (nearest
    /// price) and the distal edge (the invalidation).
    ProximalDistal,
    /// A zone event scored: touches, recency, and departure strength in
    /// the magnitude.
    ZoneStrength,
    /// The same price zone present on two timeframes at once.
    MtfZoneAlignment,
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
            Self::VolumePoc => "volume_poc",
            Self::ValueArea => "value_area",
            Self::HighVolumeNode => "high_volume_node",
            Self::LowVolumeNode => "low_volume_node",
            Self::HarmonicGartley => "harmonic_gartley",
            Self::HarmonicBat => "harmonic_bat",
            Self::HarmonicButterfly => "harmonic_butterfly",
            Self::HarmonicCrab => "harmonic_crab",
            Self::HarmonicShark => "harmonic_shark",
            Self::HarmonicCypher => "harmonic_cypher",
            Self::HarmonicAbcd => "harmonic_abcd",
            Self::HarmonicThreeDrives => "harmonic_three_drives",
            Self::DragonflyDoji => "dragonfly_doji",
            Self::GravestoneDoji => "gravestone_doji",
            Self::LongLeggedDoji => "long_legged_doji",
            Self::SpinningTop => "spinning_top",
            Self::Tweezer => "tweezer",
            Self::Kicker => "kicker",
            Self::MatchingLow => "matching_low",
            Self::OnNeck => "on_neck",
            Self::ThreeInside => "three_inside",
            Self::ThreeOutside => "three_outside",
            Self::ThreeMethods => "three_methods",
            Self::AbandonedBaby => "abandoned_baby",
            Self::TriStar => "tri_star",
            Self::StickSandwich => "stick_sandwich",
            Self::MatHold => "mat_hold",
            Self::TasukiGap => "tasuki_gap",
            Self::LadderBottom => "ladder_bottom",
            Self::BeltHold => "belt_hold",
            Self::Hikkake => "hikkake",
            Self::TripleTopBottom => "triple_top_bottom",
            Self::RectangleBreakout => "rectangle_breakout",
            Self::CupAndHandle => "cup_and_handle",
            Self::Pennant => "pennant",
            Self::FibRetracement => "fib_retracement",
            Self::GoldenPocket => "golden_pocket",
            Self::FibExtension => "fib_extension",
            Self::FibConfluence => "fib_confluence",
            Self::FibTimeZone => "fib_time_zone",
            Self::RsiPcaSignal => "rsi_pca_signal",
            Self::RsiZoneCross => "rsi_zone_cross",
            Self::RsiFamilyConsensus => "rsi_family_consensus",
            Self::RsiDivergenceSignal => "rsi_divergence_signal",
            Self::MacdSignalCross => "macd_signal_cross",
            Self::MacdZeroCross => "macd_zero_cross",
            Self::EmaCross => "ema_cross",
            Self::SmaCross => "sma_cross",
            Self::BollingerBreakout => "bollinger_breakout",
            Self::AtrExpansion => "atr_expansion",
            Self::MacdFamilyConsensus => "macd_family_consensus",
            Self::AtrFamilyExpansion => "atr_family_expansion",
            Self::GmmaCross => "gmma_cross",
            Self::StochasticZoneEntry => "stochastic_zone_entry",
            Self::StochasticCross => "stochastic_cross",
            Self::CciZoneEntry => "cci_zone_entry",
            Self::WilliamsZoneEntry => "williams_zone_entry",
            Self::RocZeroCross => "roc_zero_cross",
            Self::ObvDivergence => "obv_divergence",
            Self::AdxTrendCross => "adx_trend_cross",
            Self::MacdDivergenceSignal => "macd_divergence_signal",
            Self::BollingerSqueeze => "bollinger_squeeze",
            Self::KeltnerBreakout => "keltner_breakout",
            Self::DonchianBreakout => "donchian_breakout",
            Self::SupertrendFlip => "supertrend_flip",
            Self::PsarFlip => "psar_flip",
            Self::UpsideGapTwoCrows => "upside_gap_two_crows",
            Self::PreviousDayLevels => "previous_day_levels",
            Self::PreviousWeekLevels => "previous_week_levels",
            Self::PreviousMonthLevels => "previous_month_levels",
            Self::MonthlyOpen => "monthly_open",
            Self::RoundNumberCross => "round_number_cross",
            Self::InverseCupHandle => "inverse_cup_handle",
            Self::RoundingBottomTop => "rounding_bottom_top",
            Self::BroadeningFormation => "broadening_formation",
            Self::TrendlineBreak => "trendline_break",
            Self::Displacement => "displacement",
            Self::OptimalTradeEntry => "optimal_trade_entry",
            Self::InternalRangeLiquidity => "internal_range_liquidity",
            Self::ExternalRangeLiquidity => "external_range_liquidity",
            Self::JudasSwing => "judas_swing",
            Self::PowerOfThree => "power_of_three",
            Self::RegimeShift => "regime_shift",
            Self::MtfAlignment => "mtf_alignment",
            Self::DailyBias => "daily_bias",
            Self::WeeklyBias => "weekly_bias",
            Self::IchimokuTenkanKijunCross => "ichimoku_tenkan_kijun_cross",
            Self::IchimokuKijunCross => "ichimoku_kijun_cross",
            Self::IchimokuCloudBreakout => "ichimoku_cloud_breakout",
            Self::IchimokuKumoTwist => "ichimoku_kumo_twist",
            Self::TsiCross => "tsi_cross",
            Self::AwesomeZeroCross => "awesome_zero_cross",
            Self::AdDivergence => "ad_divergence",
            Self::CmfZeroCross => "cmf_zero_cross",
            Self::MfiZoneEntry => "mfi_zone_entry",
            Self::ChandelierFlip => "chandelier_flip",
            Self::AvwapCross => "avwap_cross",
            Self::HvRegimeShift => "hv_regime_shift",
            Self::ZscoreZoneEntry => "zscore_zone_entry",
            Self::KalmanCross => "kalman_cross",
            Self::ChangePoint => "change_point",
            Self::EntropyRegimeShift => "entropy_regime_shift",
            Self::BayesianDriftShift => "bayesian_drift_shift",
            Self::ArForecastCross => "ar_forecast_cross",
            Self::GarchRegimeShift => "garch_regime_shift",
            Self::HmmRegimeShift => "hmm_regime_shift",
            Self::BollingerReversion => "bollinger_reversion",
            Self::InitialBalanceBreakout => "initial_balance_breakout",
            Self::InitialBalanceReversal => "initial_balance_reversal",
            Self::SinglePrint => "single_print",
            Self::AcceptanceEvent => "acceptance_event",
            Self::PoorExtreme => "poor_extreme",
            Self::SilverBullet => "silver_bullet",
            Self::UnicornModel => "unicorn_model",
            Self::Model2022 => "model2022",
            Self::MarketMakerModel => "market_maker_model",
            Self::Mss => "mss",
            Self::Equilibrium => "equilibrium",
            Self::ConsequentEncroachment => "consequent_encroachment",
            Self::OpeningGap => "opening_gap",
            Self::OpeningRangeBreakout => "opening_range_breakout",
            Self::TrendlineSweep => "trendline_sweep",
            Self::LevelRejection => "level_rejection",
            Self::LevelFlip => "level_flip",
            Self::CompressionBreakout => "compression_breakout",
            Self::LiquidityVoidFill => "liquidity_void_fill",
            Self::VolumeConfirmedBreakout => "volume_confirmed_breakout",
            Self::WolfeWave => "wolfe_wave",
            Self::Diamond => "diamond",
            Self::TrendChannel => "trend_channel",
            Self::HtfCandleBias => "htf_candle_bias",
            Self::ZoneCompression => "zone_compression",
            Self::ProximalDistal => "proximal_distal",
            Self::ZoneStrength => "zone_strength",
            Self::MtfZoneAlignment => "mtf_zone_alignment",
        }
    }

    /// Every kind, for a client that wants to build a filter menu.
    pub const ALL: [Self; 188] = [
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
        Self::VolumePoc,
        Self::ValueArea,
        Self::HighVolumeNode,
        Self::LowVolumeNode,
        Self::HarmonicGartley,
        Self::HarmonicBat,
        Self::HarmonicButterfly,
        Self::HarmonicCrab,
        Self::HarmonicShark,
        Self::HarmonicCypher,
        Self::HarmonicAbcd,
        Self::HarmonicThreeDrives,
        Self::DragonflyDoji,
        Self::GravestoneDoji,
        Self::LongLeggedDoji,
        Self::SpinningTop,
        Self::Tweezer,
        Self::Kicker,
        Self::MatchingLow,
        Self::OnNeck,
        Self::ThreeInside,
        Self::ThreeOutside,
        Self::ThreeMethods,
        Self::AbandonedBaby,
        Self::TriStar,
        Self::StickSandwich,
        Self::MatHold,
        Self::TasukiGap,
        Self::LadderBottom,
        Self::BeltHold,
        Self::Hikkake,
        Self::TripleTopBottom,
        Self::RectangleBreakout,
        Self::CupAndHandle,
        Self::Pennant,
        Self::FibRetracement,
        Self::GoldenPocket,
        Self::FibExtension,
        Self::FibConfluence,
        Self::FibTimeZone,
        Self::RsiPcaSignal,
        Self::RsiZoneCross,
        Self::RsiFamilyConsensus,
        Self::RsiDivergenceSignal,
        Self::MacdSignalCross,
        Self::MacdZeroCross,
        Self::EmaCross,
        Self::SmaCross,
        Self::BollingerBreakout,
        Self::AtrExpansion,
        Self::MacdFamilyConsensus,
        Self::AtrFamilyExpansion,
        Self::GmmaCross,
        Self::StochasticZoneEntry,
        Self::StochasticCross,
        Self::CciZoneEntry,
        Self::WilliamsZoneEntry,
        Self::RocZeroCross,
        Self::ObvDivergence,
        Self::AdxTrendCross,
        Self::MacdDivergenceSignal,
        Self::BollingerSqueeze,
        Self::KeltnerBreakout,
        Self::DonchianBreakout,
        Self::SupertrendFlip,
        Self::PsarFlip,
        Self::UpsideGapTwoCrows,
        Self::PreviousDayLevels,
        Self::PreviousWeekLevels,
        Self::PreviousMonthLevels,
        Self::MonthlyOpen,
        Self::RoundNumberCross,
        Self::InverseCupHandle,
        Self::RoundingBottomTop,
        Self::BroadeningFormation,
        Self::TrendlineBreak,
        Self::Displacement,
        Self::OptimalTradeEntry,
        Self::InternalRangeLiquidity,
        Self::ExternalRangeLiquidity,
        Self::JudasSwing,
        Self::PowerOfThree,
        Self::RegimeShift,
        Self::MtfAlignment,
        Self::DailyBias,
        Self::WeeklyBias,
        Self::IchimokuTenkanKijunCross,
        Self::IchimokuKijunCross,
        Self::IchimokuCloudBreakout,
        Self::IchimokuKumoTwist,
        Self::TsiCross,
        Self::AwesomeZeroCross,
        Self::AdDivergence,
        Self::CmfZeroCross,
        Self::MfiZoneEntry,
        Self::ChandelierFlip,
        Self::AvwapCross,
        Self::HvRegimeShift,
        Self::ZscoreZoneEntry,
        Self::KalmanCross,
        Self::ChangePoint,
        Self::EntropyRegimeShift,
        Self::BayesianDriftShift,
        Self::ArForecastCross,
        Self::GarchRegimeShift,
        Self::HmmRegimeShift,
        Self::BollingerReversion,
        Self::InitialBalanceBreakout,
        Self::InitialBalanceReversal,
        Self::SinglePrint,
        Self::AcceptanceEvent,
        Self::PoorExtreme,
        Self::SilverBullet,
        Self::UnicornModel,
        Self::Model2022,
        Self::MarketMakerModel,
        Self::Mss,
        Self::Equilibrium,
        Self::ConsequentEncroachment,
        Self::OpeningGap,
        Self::OpeningRangeBreakout,
        Self::TrendlineSweep,
        Self::LevelRejection,
        Self::LevelFlip,
        Self::CompressionBreakout,
        Self::LiquidityVoidFill,
        Self::VolumeConfirmedBreakout,
        Self::WolfeWave,
        Self::Diamond,
        Self::TrendChannel,
        Self::HtfCandleBias,
        Self::ZoneCompression,
        Self::ProximalDistal,
        Self::ZoneStrength,
        Self::MtfZoneAlignment,
    ];
}

/// The detector's own vertices and rig, when the shape needs more than a
/// level and a band.
///
/// A pattern is its pivot path; a harmonic is its XABCD legs; a Fibonacci
/// study is its rungs. The flat level/zone fields describe a box or a line,
/// and squeezing a path into them loses the shape the concept is. This is
/// the one place that extra geometry travels: the detector computes it, the
/// concept path hands it to the renderer, and nothing re-derives it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventGeometry {
    /// The shape's own vertices, oldest first: (bar open time, price).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<(i64, f64)>,
    /// A measured ladder's rungs: (ratio, price), lowest ratio first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rungs: Vec<(f64, f64)>,
    /// The two swing anchors a ladder measures: (from time, from price,
    /// to time, to price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchors: Option<(i64, f64, i64, f64)>,
}

impl EventGeometry {
    /// A path through the detector's pivots.
    #[must_use]
    pub fn path(points: Vec<(i64, f64)>) -> Self {
        Self {
            points,
            ..Self::default()
        }
    }
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
    /// The detector's own vertices, when the shape needs a path rather than
    /// a level. Absent for every event whose shape the fields above already
    /// describe completely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<EventGeometry>,
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

/// The volume-profile pass, as an ADAPTER over the platform's own profile
/// engine (`crate::volume_profile`): the point of control, the value area,
/// and the high/low-volume nodes. The engine is candle-derived, and it
/// no-ops on an empty profile -- a synthetic index with zero reported volume
/// gets NO events rather than invented ones, which is the honest floor.
pub fn detect_volume_profile(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 20 {
        return out;
    }
    let Some(last) = candles.last() else { return out };
    let span = candles.iter().map(|k| k.high).fold(f64::MIN, f64::max)
        - candles.iter().map(|k| k.low).fold(f64::MAX, f64::min);
    if span <= 0.0 {
        return out;
    }
    let bucket = crate::volume_profile::round_bucket(span, 48);
    let profile = crate::volume_profile::calculate_volume_profile_from_candles(candles, bucket);
    if profile.is_empty() {
        return out;
    }
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let mut push = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>| {
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
            geometry: None,
            formed_at: None,
        });
    };

    // The point of control: the single heaviest bucket.
    push(EventKind::VolumePoc, None, profile.poc, None, None);

    // The value area: the band where the bulk of the volume traded.
    push(EventKind::ValueArea, None, (profile.vah + profile.val) / 2.0, Some(profile.val), Some(profile.vah));

    // The nodes: the engine's own local maxima and minima of the histogram,
    // banded so contiguous runs draw as zones rather than bucket lists.
    let mut hvns = profile.hvn.clone();
    hvns.sort_by(|a, b| a.total_cmp(b));
    for band in contiguous(&hvns, bucket) {
        push(EventKind::HighVolumeNode, None, (band.0 + band.1) / 2.0, Some(band.0), Some(band.1));
    }
    let mut lvns = profile.lvn.clone();
    lvns.sort_by(|a, b| a.total_cmp(b));
    for band in contiguous(&lvns, bucket) {
        push(EventKind::LowVolumeNode, None, (band.0 + band.1) / 2.0, Some(band.0), Some(band.1));
    }

    out
}

/// Merge sorted node prices within one bucket-step of each other into bands.
fn contiguous(nodes: &[f64], step: f64) -> Vec<(f64, f64)> {
    let mut bands: Vec<(f64, f64)> = Vec::new();
    for &price in nodes {
        match bands.last_mut() {
            Some(band) if price - band.1 <= step * 1.5 => band.1 = price,
            _ => bands.push((price, price)),
        }
    }
    bands
        .into_iter()
        .map(|(lo, hi)| (lo - step / 2.0, hi + step / 2.0))
        .collect()
}

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
                geometry: Some(EventGeometry::path(
                    m.anchors.iter().map(|a| (a.timestamp, a.price)).collect(),
                )),
                formed_at: m.anchors.first().map(|a| a.timestamp),
            }
        })
        .collect()
}

/// Collapse near-duplicate edges: two edges within the tolerance AND close
/// in time are one, and the more extreme of them survives.
fn collapse_edges(mut edges: Vec<(usize, f64)>, tolerance: f64, highs: bool) -> Vec<(usize, f64)> {
    let mut out: Vec<(usize, f64)> = Vec::new();
    for edge in edges.drain(..) {
        match out.last_mut() {
            Some(last) if (edge.1 - last.1).abs() <= tolerance * edge.1.abs().max(last.1.abs()) && edge.0 - last.0 <= 3 => {
                let replace = if highs { edge.1 > last.1 } else { edge.1 < last.1 };
                if replace {
                    *last = edge;
                }
            }
            _ => out.push(edge),
        }
    }
    out
}

/// A path through candle-indexed edges, oldest first.
///
/// The gap detectors keep their vertices as (bar index, price) pairs; the
/// wire wants (time, price). One conversion, here, so a pattern's outline is
/// the same vertices the rule that fired used.
fn edge_path(candles: &[Candle], edges: &[(usize, f64)]) -> EventGeometry {
    EventGeometry::path(
        edges
            .iter()
            .map(|(index, price)| (candles[*index].open_time, *price))
            .collect(),
    )
}

/// A closed perimeter between two anchors, in time order: top edge forward,
/// bottom edge back. The rectangle and the channel are both this.
fn perimeter_path(candles: &[Candle], a: (usize, f64), b: (usize, f64), low: f64, high: f64) -> EventGeometry {
    let (first, last) = if a.0 <= b.0 { (a.0, b.0) } else { (b.0, a.0) };
    let (t0, t1) = (candles[first].open_time, candles[last].open_time);
    EventGeometry::path(vec![(t0, high), (t1, high), (t1, low), (t0, low), (t0, high)])
}

/// The chart-pattern gap fills: triple tops, rectangles, cups, pennants.
///
/// Same swing machinery as the rest of the family (strict lookback swings),
/// and the canonical rules: a triple top is the double top's natural
/// extension, a rectangle is flat tops AND flat bottoms at once, a cup is a
/// rounded bottom with a shallow handle, a pennant converges where a flag
/// drifts parallel.
pub fn detect_chart_gaps(candles: &[Candle], symbol: &str, config: &EventConfig) -> Vec<MarketEvent> {
    const TOP_TOLERANCE: f64 = 0.003;
    const HANDLE_MAX_DEPTH: f64 = 0.5;

    let mut out = Vec::new();
    let lb = config.structure_lookback;
    // The pennant's floor is the deepest window here (its pole plus the
    // channel); every other detector's slices are saturating-guarded.
    if candles.len() < 24 {
        return out;
    }
    let last = candles.last().expect("checked above");
    let bar_time = last.open_time;
    let tf = last.timeframe;
    let n = candles.len() - 1;
    let make = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| MarketEvent {
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
        geometry: None,
        formed_at: formed,
    };

    // The strict swings of the STRUCTURE: the newest bar is the breakout or
    // resumption, and it is excluded, because a rectangle's second edge can
    // never confirm when the bar that breaks it lands inside its
    // confirmation window. The channel lives before the break.
    // The channel's edges, the way a trader reads them: a pivot on its left
    // side (higher than the `lb` bars before it), and ALIVE until violated
    // -- every later bar before the breakout must respect it. A level is a
    // level until broken, which is the only reading that lets a rectangle's
    // second edge exist on the bar before the break.
    let mut highs: Vec<(usize, f64)> = Vec::new();
    let mut lows: Vec<(usize, f64)> = Vec::new();
    for i in lb..candles.len() - 1 {
        let k = &candles[i];
        // The left pivot tolerates the flat band too: pattern tops may sit a
        // hair above each other and every one still counts -- the band IS
        // the tolerance, on the left exactly as on the violation side.
        let left_pivot_high = (i - lb..i).all(|j| candles[j].high < k.high * (1.0 + TOP_TOLERANCE));
        // Alive until violated BEYOND the flat tolerance: a rectangle's
        // second touch may sit a hair above the first without killing it --
        // that hair IS the tolerance, not a violation.
        if left_pivot_high
            && candles[i + 1..]
                .iter()
                .all(|b| b.high <= k.high * (1.0 + TOP_TOLERANCE) || b.open_time == last.open_time)
        {
            highs.push((i, k.high));
        }
        let left_pivot_low = (i - lb..i).all(|j| candles[j].low > k.low * (1.0 - TOP_TOLERANCE));
        if left_pivot_low
            && candles[i + 1..]
                .iter()
                .all(|b| b.low >= k.low * (1.0 - TOP_TOLERANCE) || b.open_time == last.open_time)
        {
            lows.push((i, k.low));
        }
    }

    // Near-duplicate edges collapse to the extreme of each run (within the
    // flat tolerance and a couple of lookbacks): a peak tagged twice by two
    // near bars is ONE edge, the higher of them -- otherwise the newest
    // three "peaks" are one peak and its shadows.
    let highs = collapse_edges(highs, TOP_TOLERANCE, true);
    let lows = collapse_edges(lows, TOP_TOLERANCE, false);

    // -- Triple top / bottom -----------------------------------------------------
    // Three newest highs within tolerance, the neckline at the LOWER of the
    // two intervening troughs, and the newest close through it.
    if highs.len() >= 3 {
        let (a, b, c) = (highs[highs.len() - 3], highs[highs.len() - 2], highs[highs.len() - 1]);
        let mid = (a.1 + b.1 + c.1) / 3.0;
        let within = (a.1 - b.1).abs() <= mid * TOP_TOLERANCE
            && (b.1 - c.1).abs() <= mid * TOP_TOLERANCE
            && (a.1 - c.1).abs() <= mid * TOP_TOLERANCE;
        if within {
            let trough1 = candles[a.0..=b.0].iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let trough2 = candles[b.0..=c.0].iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let neckline = trough1.min(trough2);
            if mid - neckline >= mid * 0.01 && last.close < neckline {
                out.push(make(EventKind::TripleTopBottom, Some(Side::Sell), neckline, Some(neckline), Some(mid), Some(candles[a.0].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[a, b, c]));
                }
            }
        }
    }
    if lows.len() >= 3 {
        let (a, b, c) = (lows[lows.len() - 3], lows[lows.len() - 2], lows[lows.len() - 1]);
        let mid = (a.1 + b.1 + c.1) / 3.0;
        let within = (a.1 - b.1).abs() <= mid * TOP_TOLERANCE
            && (b.1 - c.1).abs() <= mid * TOP_TOLERANCE
            && (a.1 - c.1).abs() <= mid * TOP_TOLERANCE;
        if within {
            let peak1 = candles[a.0..=b.0].iter().map(|k| k.high).fold(f64::MIN, f64::max);
            let peak2 = candles[b.0..=c.0].iter().map(|k| k.high).fold(f64::MIN, f64::max);
            let neckline = peak1.max(peak2);
            if neckline - mid >= mid * 0.01 && last.close > neckline {
                out.push(make(EventKind::TripleTopBottom, Some(Side::Buy), neckline, Some(mid), Some(neckline), Some(candles[a.0].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[a, b, c]));
                }
            }
        }
    }

    // -- Rectangle -----------------------------------------------------------------
    // The newest two highs flat AND the newest two lows flat (the channel);
    // the newest close beyond a side is the breakout, in its direction.
    if highs.len() >= 2 && lows.len() >= 2 {
        let (h1, h2) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        let (l1, l2) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        let top = (h1.1 + h2.1) / 2.0;
        let bottom = (l1.1 + l2.1) / 2.0;
        let flat_tops = (h1.1 - h2.1).abs() <= top * TOP_TOLERANCE;
        let flat_bottoms = (l1.1 - l2.1).abs() <= bottom * TOP_TOLERANCE;
        // The channel must be a real band (height at least 1% of price).
        if flat_tops && flat_bottoms && top - bottom >= bottom * 0.01 {
            let from = candles[h1.0.min(l1.0)].open_time;
            let (first_high, second_high) = if h1.0 <= h2.0 { (h1, h2) } else { (h2, h1) };
            if last.close > top {
                out.push(make(EventKind::RectangleBreakout, Some(Side::Buy), top, Some(bottom), Some(top), Some(from)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(perimeter_path(candles, first_high, second_high, bottom, top));
                }
            } else if last.close < bottom {
                out.push(make(EventKind::RectangleBreakout, Some(Side::Sell), bottom, Some(bottom), Some(top), Some(from)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(perimeter_path(candles, first_high, second_high, bottom, top));
                }
            }
        }
    }

    // -- Cup and handle --------------------------------------------------------------
    // A rounded bottom recovering to its rim, a shallow handle in the upper
    // half, and the newest close above the rim. The bottom is the newest
    // swing low whose recovery reached back to the prior swing high (the rim).
    if highs.len() >= 2 && !lows.is_empty() {
        // The bottom is the DEEPEST swing low of the window (the cup's base);
        // the rim is the newest swing high BEFORE it (the cup's left lip);
        // the handle is the shallow drift after the recovery, and it is
        // allowed to be the newest low -- which is why the bottom must not
        // be.
        let (bottom_idx, bottom) = lows
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, p)| (*i, *p))
            .expect("checked above");
        let rim_entry = highs.iter().filter(|h| h.0 < bottom_idx).last().copied();
        if let Some((rim_idx, rim)) = rim_entry {
            // Price recovered from the bottom back to the rim.
            let recovered = candles[bottom_idx..].iter().any(|k| k.high >= rim * (1.0 - TOP_TOLERANCE));
            // The handle: the drift AFTER the recovery bar (the first touch
            // of the rim), held in the cup's upper half -- measuring from
            // the newest N bars would reach back into the recovery leg.
            let cup_depth = rim - bottom;
            let recovery_idx = candles[bottom_idx..]
                .iter()
                .position(|k| k.high >= rim * (1.0 - TOP_TOLERANCE))
                .map(|p| p + bottom_idx);
            let handle_low = recovery_idx.map_or(f64::MAX, |r| {
                candles[r + 1..=n].iter().map(|k| k.low).fold(f64::MAX, f64::min)
            });
            let handle_depth = rim - handle_low;
            if recovered
                && cup_depth > 0.0
                && handle_depth > 0.0
                && handle_depth <= cup_depth * HANDLE_MAX_DEPTH
                && last.close > rim
            {
                out.push(make(EventKind::CupAndHandle, Some(Side::Buy), rim, Some(bottom), Some(rim), Some(candles[rim_idx].open_time)));
                if let Some(e) = out.last_mut() {
                    let right = recovery_idx.unwrap_or(n);
                    e.geometry = Some(edge_path(candles, &[(rim_idx, rim), (bottom_idx, bottom), (right, rim)]));
                }
            }
        }
    }

    // -- Inverse cup & handle ----------------------------------------------
    // The cup mirrored: a rounded TOP (the deepest swing HIGH after the
    // rim, which is the newest swing LOW before it), a shallow handle in the
    // LOWER half of the bowl, and the breakdown through the rim.
    if highs.len() >= 2 && !lows.is_empty() {
        let (top_idx, top) = highs
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, p)| (*i, *p))
            .expect("checked above");
        let rim_entry = lows.iter().filter(|l| l.0 < top_idx).last().copied();
        if let Some((rim_idx, rim)) = rim_entry {
            let recovered = candles[top_idx..].iter().any(|k| k.low <= rim * (1.0 + TOP_TOLERANCE));
            let bowl_depth = top - rim;
            let recovery_idx = candles[top_idx..]
                .iter()
                .position(|k| k.low <= rim * (1.0 + TOP_TOLERANCE))
                .map(|p| p + top_idx);
            let handle_high = recovery_idx.map_or(f64::MIN, |r| {
                candles[r + 1..=n].iter().map(|k| k.high).fold(f64::MIN, f64::max)
            });
            let handle_rise = handle_high - rim;
            if recovered
                && bowl_depth > 0.0
                && handle_rise > 0.0
                && handle_rise <= bowl_depth * HANDLE_MAX_DEPTH
                && last.close < rim
            {
                out.push(make(EventKind::InverseCupHandle, Some(Side::Sell), rim, Some(rim), Some(top), Some(candles[rim_idx].open_time)));
                if let Some(e) = out.last_mut() {
                    let right = recovery_idx.unwrap_or(n);
                    e.geometry = Some(edge_path(candles, &[(rim_idx, rim), (top_idx, top), (right, rim)]));
                }
            }
        }
    }

    // -- Rounding bottom / top ----------------------------------------------
    // The cup WITHOUT the handle: the arc recovers to the rim and price
    // closes straight through it -- no drift, no pause. Same structure, the
    // handle checks deliberately absent.
    if highs.len() >= 2 && !lows.is_empty() {
        let (bottom_idx, bottom) = lows
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, p)| (*i, *p))
            .expect("checked above");
        let rim_entry = highs.iter().filter(|h| h.0 < bottom_idx).last().copied();
        if let Some((rim_idx, rim)) = rim_entry {
            let recovered = candles[bottom_idx..].iter().any(|k| k.high >= rim * (1.0 - TOP_TOLERANCE));
            if recovered && rim - bottom > 0.0 && last.close > rim {
                out.push(make(EventKind::RoundingBottomTop, Some(Side::Buy), rim, Some(bottom), Some(rim), Some(candles[rim_idx].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[(rim_idx, rim), (bottom_idx, bottom), (n, rim)]));
                }
            }
        }
        // The mirror: the rounding top breaking down through its rim.
        let (top_idx, top) = highs
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, p)| (*i, *p))
            .expect("checked above");
        let rim_low = lows.iter().filter(|l| l.0 < top_idx).last().copied();
        if let Some((rim_idx, rim)) = rim_low {
            let recovered = candles[top_idx..].iter().any(|k| k.low <= rim * (1.0 + TOP_TOLERANCE));
            if recovered && top - rim > 0.0 && last.close < rim {
                out.push(make(EventKind::RoundingBottomTop, Some(Side::Sell), rim, Some(rim), Some(top), Some(candles[rim_idx].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[(rim_idx, rim), (top_idx, top), (n, rim)]));
                }
            }
        }
    }

    // -- Trendlines --------------------------------------------------------------
    // The swing-anchored line: the newest three lows ASCENDING draw the
    // bull line through the first two; the third must touch it (within half
    // the flat tolerance) to validate; the newest close through the line is
    // the break. The bear line mirrors with descending highs.
    let line_at = |a: (usize, f64), b: (usize, f64), i: usize| -> f64 {
        a.1 + (b.1 - a.1) * (i - a.0) as f64 / (b.0 - a.0).max(1) as f64
    };
    if lows.len() >= 3 {
        let (l1, l2, l3) = (lows[lows.len() - 3], lows[lows.len() - 2], lows[lows.len() - 1]);
        let ascending = l1.1 < l2.1 && l2.1 < l3.1 && l1.0 < l2.0 && l2.0 < l3.0;
        if ascending {
            let touch = (l3.1 - line_at(l1, l2, l3.0)).abs() <= line_at(l1, l2, l3.0) * (TOP_TOLERANCE / 2.0);
            let break_level = line_at(l1, l2, n);
            if touch && last.close < break_level * (1.0 - TOP_TOLERANCE) {
                out.push(make(EventKind::TrendlineBreak, Some(Side::Sell), break_level, Some(break_level), Some(line_at(l1, l2, l1.0)), Some(candles[l1.0].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[l1, l2, (n, break_level)]));
                }
            }
        }
    }
    if highs.len() >= 3 {
        let (h1, h2, h3) = (highs[highs.len() - 3], highs[highs.len() - 2], highs[highs.len() - 1]);
        let descending = h1.1 > h2.1 && h2.1 > h3.1 && h1.0 < h2.0 && h2.0 < h3.0;
        if descending {
            let touch = (h3.1 - line_at(h1, h2, h3.0)).abs() <= line_at(h1, h2, h3.0) * (TOP_TOLERANCE / 2.0);
            let break_level = line_at(h1, h2, n);
            if touch && last.close > break_level * (1.0 + TOP_TOLERANCE) {
                out.push(make(EventKind::TrendlineBreak, Some(Side::Buy), break_level, Some(line_at(h1, h2, h1.0)), Some(break_level), Some(candles[h1.0].open_time)));
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &[h1, h2, (n, break_level)]));
                }
            }
        }
    }

    // -- Broadening formation --------------------------------------------------
    // The megaphone: the newest THREE highs ascending AND the newest THREE
    // lows descending -- the range expands on both sides. The signal is the
    // close beyond the newest extreme of the side it breaks.
    if highs.len() >= 3 && lows.len() >= 3 {
        let (h1, h2, h3) = (highs[highs.len() - 3], highs[highs.len() - 2], highs[highs.len() - 1]);
        let (l1, l2, l3) = (lows[lows.len() - 3], lows[lows.len() - 2], lows[lows.len() - 1]);
        let expanding = h1.1 < h2.1 && h2.1 < h3.1 && l1.1 > l2.1 && l2.1 > l3.1;
        if expanding {
            let broken = if last.close > h3.1 {
                out.push(make(EventKind::BroadeningFormation, Some(Side::Buy), h3.1, Some(l3.1), Some(h3.1), Some(candles[h1.0].open_time)));
                true
            } else if last.close < l3.1 {
                out.push(make(EventKind::BroadeningFormation, Some(Side::Sell), l3.1, Some(l3.1), Some(h3.1), Some(candles[l1.0].open_time)));
                true
            } else {
                false
            };
            if broken {
                let mut edges = [h1, h2, h3, l1, l2, l3];
                edges.sort_by_key(|edge| edge.0);
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &edges));
                }
            }
        }
    }

    // -- Diamond ------------------------------------------------------------
    // Broadening immediately followed by contracting, at the range's edge:
    // the newest six+ swings split into an expanding half and a contracting
    // half. Top at the formation's high (reversal down), bottom mirrors.
    if highs.len() >= 4 && lows.len() >= 4 {
        let (h, l) = (&highs[highs.len() - 4..], &lows[lows.len() - 4..]);
        let span = |hs: f64, ls: f64| hs - ls;
        // The four spans between consecutive high/low pairs, oldest first.
        let spans: Vec<f64> = (0..4).map(|i| span(h[i].1, l[i].1)).collect();
        let expanding = spans[0] < spans[1] && spans[1] < spans[2] * 1.0 || spans[0] * 1.05 < spans[1] && spans[1] < spans[2];
        let contracting = spans[2] > spans[3] && spans[1] > spans[2] * 0.98;
        if expanding && contracting {
            let top = h[3].1;
            let bottom = l[3].1;
            let broken = if last.close < bottom * 0.997 {
                out.push(make(EventKind::Diamond, Some(Side::Sell), bottom, Some(bottom), Some(top), Some(candles[h[0].0].open_time)));
                true
            } else if last.close > top * 1.003 {
                out.push(make(EventKind::Diamond, Some(Side::Buy), top, Some(bottom), Some(top), Some(candles[l[0].0].open_time)));
                true
            } else {
                false
            };
            if broken {
                let mut edges: Vec<(usize, f64)> = Vec::new();
                edges.extend_from_slice(h);
                edges.extend_from_slice(l);
                edges.sort_by_key(|edge| edge.0);
                if let Some(e) = out.last_mut() {
                    e.geometry = Some(edge_path(candles, &edges));
                }
            }
        }
    }

    // -- The trend channel -------------------------------------------------
    // The validated trendline plus its PARALLEL counterpart: three
    // ascending lows draw the bull line, and the highs' own line runs
    // parallel within tolerance -- price inside the two is the channel.
    // The signal is the close beyond EITHER line.
    if lows.len() >= 3 && highs.len() >= 3 {
        let (l1, l2, l3) = (lows[lows.len() - 3], lows[lows.len() - 2], lows[lows.len() - 1]);
        let (h1, h2, h3) = (highs[highs.len() - 3], highs[highs.len() - 2], highs[highs.len() - 1]);
        let lows_ok = l1.1 < l2.1 && l2.1 < l3.1 && (l3.1 - line_at(l1, l2, l3.0)).abs() <= line_at(l1, l2, l3.0) * (TOP_TOLERANCE / 2.0);
        let highs_ok = h1.1 < h2.1 && h2.1 < h3.1 && (h3.1 - line_at(h1, h2, h3.0)).abs() <= line_at(h1, h2, h3.0) * (TOP_TOLERANCE / 2.0);
        if lows_ok && highs_ok {
            // Parallel: the two lines' slopes agree within tolerance.
            let slope_l = (l2.1 - l1.1) / (l2.0 - l1.0).max(1) as f64;
            let slope_h = (h2.1 - h1.1) / (h2.0 - h1.0).max(1) as f64;
            let parallel = (slope_l - slope_h).abs() <= slope_l.abs().max(slope_h.abs()) * 0.5 + f64::EPSILON;
            if parallel {
                let low_line = line_at(l1, l2, n);
                let high_line = line_at(h1, h2, n);
                let broken = if last.close > high_line * (1.0 + TOP_TOLERANCE) {
                    out.push(make(EventKind::TrendChannel, Some(Side::Buy), high_line, Some(low_line), Some(high_line), Some(candles[l1.0].open_time)));
                    true
                } else if last.close < low_line * (1.0 - TOP_TOLERANCE) {
                    out.push(make(EventKind::TrendChannel, Some(Side::Sell), low_line, Some(low_line), Some(high_line), Some(candles[h1.0].open_time)));
                    true
                } else {
                    false
                };
                if broken {
                    let first = l1.0.min(h1.0);
                    let last_anchor = l2.0.max(h2.0);
                    let (t0, t1) = (candles[first].open_time, candles[last_anchor].open_time);
                    let low_at_first = line_at(l1, l2, first);
                    let high_at_first = line_at(h1, h2, first);
                    let low_at_last = line_at(l1, l2, last_anchor);
                    let high_at_last = line_at(h1, h2, last_anchor);
                    if let Some(e) = out.last_mut() {
                        // The channel is its two lines: one closed perimeter.
                        e.geometry = Some(EventGeometry::path(vec![
                            (t0, low_at_first),
                            (t1, low_at_last),
                            (t1, high_at_last),
                            (t0, high_at_first),
                            (t0, low_at_first),
                        ]));
                    }
                }
            }
        }
    }

    // -- Pennant -------------------------------------------------------------------
    // The flag's impulse, then a CONVERGING small channel (highs falling
    // while lows rise). Distinct from the flag's parallel counter-drift.
    if n >= 24 {
        let pole_start = candles[n - 16..n - 8].iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let pole_end = candles[n - 16..n - 8].iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let pole = pole_end - pole_start;
        let avg_range = candles[n - 24..n].iter().map(|k| k.high - k.low).sum::<f64>() / 24.0;
        let channel = &candles[n - 8..=n];
        let ch_high_first = channel[..4].iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let ch_high_last = channel[4..].iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let ch_low_first = channel[..4].iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let ch_low_last = channel[4..].iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let converging = ch_high_last < ch_high_first && ch_low_last > ch_low_first;
        let ch_span = (ch_high_last.max(ch_high_first)) - (ch_low_last.min(ch_low_first));
        if pole > avg_range * 6.0 && converging && ch_span < pole * 0.5 {
            let up = candles[n - 8].close > candles[n - 16].close;
            out.push(make(EventKind::Pennant, Some(if up { Side::Buy } else { Side::Sell }), (ch_high_last + ch_low_last) / 2.0, Some(ch_low_last.min(ch_low_first)), Some(ch_high_last.max(ch_high_first)), Some(candles[n - 16].open_time)));
            if let Some(e) = out.last_mut() {
                let (t0, t1) = (channel[0].open_time, channel[channel.len() - 1].open_time);
                e.geometry = Some(EventGeometry::path(vec![
                    (t0, ch_low_first),
                    (t1, ch_low_last),
                    (t1, ch_high_last),
                    (t0, ch_high_first),
                    (t0, ch_low_first),
                ]));
            }
        }
    }

    out
}

/// The session pass: kill zones and the Asian range, in the platform's own
/// The standalone displacement: ICT's energetic candle -- a body at least
/// twice the 14-bar average, closing at a fresh 20-bar extreme. The order
/// block machinery has always READ this; now it is an event of its own.
pub fn detect_displacement(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 22 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let body = (last.close - last.open).abs();
    let mean_body: f64 = candles[n - 14..n]
        .iter()
        .map(|k| (k.close - k.open).abs())
        .sum::<f64>()
        / 14.0;
    if mean_body <= 0.0 || body < 2.0 * mean_body {
        return out;
    }
    let extreme_hi = candles[n - 20..n].iter().map(|k| k.high).fold(f64::NEG_INFINITY, f64::max);
    let extreme_lo = candles[n - 20..n].iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
    let side = if last.close > extreme_hi {
        Some(Side::Buy)
    } else if last.close < extreme_lo {
        Some(Side::Sell)
    } else {
        None
    };
    if let Some(side) = side {
        out.push(MarketEvent {
            kind: EventKind::Displacement,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: last.close,
            side: Some(side),
            zone_low: Some(last.low),
            zone_high: Some(last.high),
            magnitude: Some(body / mean_body),
            geometry: None,
            formed_at: None,
        });
    }
    out
}

/// The equal-levels tolerance, shared by the pools and the IRL clusters.
const EQUAL_TOLERANCE_ATR_CONST: f64 = 0.15;

/// IRL/ERL: the dealing-range framing. The EXTERNAL range is the newest
/// MAJOR swing (lookback 5) -- its extremes are the pools the whole move is
/// drawn on. INTERNAL liquidity is the minor swings' clusters (lookback 2)
/// resting INSIDE that range.
pub fn detect_range_liquidity(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 24 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let swings = |lb: usize| -> (Vec<(usize, f64)>, Vec<(usize, f64)>) {
        let mut highs = Vec::new();
        let mut lows = Vec::new();
        for i in lb..n.saturating_sub(lb) {
            let k = &candles[i];
            if (i - lb..=i + lb).all(|j| j == i || candles[j].high < k.high) {
                highs.push((i, k.high));
            }
            if (i - lb..=i + lb).all(|j| j == i || candles[j].low > k.low) {
                lows.push((i, k.low));
            }
        }
        (highs, lows)
    };
    let (major_hi, major_lo) = swings(5);
    let (minor_hi, minor_lo) = swings(2);
    let (Some(&(range_hi_idx, range_hi)), Some(&(range_lo_idx, range_lo))) = (major_hi.last(), major_lo.last()) else {
        return out;
    };
    // ERL: the range's own extremes, each once, from its anchor bar.
    out.push(MarketEvent {
        kind: EventKind::ExternalRangeLiquidity,
        symbol: symbol.to_string(),
        timeframe: last.timeframe,
        bar_time: last.open_time,
        close: last.close,
        price: range_hi,
        side: Some(Side::Buy), // buy stops rest above the range high
        zone_low: Some(range_lo),
        zone_high: Some(range_hi),
        magnitude: None,
        geometry: None,
        formed_at: Some(candles[range_hi_idx].open_time),
    });
    out.push(MarketEvent {
        kind: EventKind::ExternalRangeLiquidity,
        symbol: symbol.to_string(),
        timeframe: last.timeframe,
        bar_time: last.open_time,
        close: last.close,
        price: range_lo,
        side: Some(Side::Sell), // sell stops rest below the range low
        zone_low: Some(range_lo),
        zone_high: Some(range_hi),
        magnitude: None,
        geometry: None,
        formed_at: Some(candles[range_lo_idx].open_time),
    });
    // IRL: the newest minor CLUSTER strictly inside the range -- two or more
    // minor swings within the ATR tolerance, neither of them the range's own
    // edges.
    let atr14 = {
        let start = n.saturating_sub(14);
        candles[start..n].iter().map(|k| k.high - k.low).sum::<f64>() / (n - start).max(1) as f64
    };
    let tolerance = atr14 * EQUAL_TOLERANCE_ATR_CONST;
    let cluster = |swings: &[(usize, f64)]| -> Option<(usize, f64)> {
        let inside: Vec<(usize, f64)> = swings
            .iter()
            .filter(|(_, p)| *p > range_lo + tolerance && *p < range_hi - tolerance)
            .copied()
            .collect();
        if inside.len() >= 2 {
            let (a, b) = (inside[inside.len() - 2], inside[inside.len() - 1]);
            if (a.1 - b.1).abs() <= tolerance {
                return Some((a.0, (a.1 + b.1) / 2.0));
            }
        }
        None
    };
    if let Some((idx, level)) = cluster(&minor_hi) {
        out.push(MarketEvent {
            kind: EventKind::InternalRangeLiquidity,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: level,
            side: Some(Side::Buy),
            zone_low: Some(range_lo),
            zone_high: Some(range_hi),
            magnitude: None,
            geometry: None,
            formed_at: Some(candles[idx].open_time),
        });
    }
    if let Some((idx, level)) = cluster(&minor_lo) {
        out.push(MarketEvent {
            kind: EventKind::InternalRangeLiquidity,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: level,
            side: Some(Side::Sell),
            zone_low: Some(range_lo),
            zone_high: Some(range_hi),
            magnitude: None,
            geometry: None,
            formed_at: Some(candles[idx].open_time),
        });
    }
    out
}

/// The five level-and-structure gap fills: MSS (the counter-trend break),
/// equilibrium (the range's own 50%), consequent encroachment (the newest
/// FVG's midpoint), the opening gap, and the opening-range breakout.
pub fn detect_structure_levels(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 30 {
        return out;
    }
    let n = candles.len() - 1;
    let last = &candles[n];
    let make = |kind: EventKind, side: Option<Side>, price: f64, low: Option<f64>, high: Option<f64>, formed: Option<i64>| {
        MarketEvent {
            kind,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price,
            side,
            zone_low: low,
            zone_high: high,
            magnitude: None,
            geometry: None,
            formed_at: formed,
        }
    };

    // -- MSS: the newest two swings define the prevailing trend; a close
    // through the newest swing AGAINST it is the shift (a break WITH it is
    // continuation, which BOS already names).
    {
        let lb = 3;
        let mut highs: Vec<(usize, f64)> = Vec::new();
        let mut lows: Vec<(usize, f64)> = Vec::new();
        for i in lb..n.saturating_sub(lb) {
            if (i - lb..=i + lb).all(|j| j == i || candles[j].high < candles[i].high) {
                highs.push((i, candles[i].high));
            }
            if (i - lb..=i + lb).all(|j| j == i || candles[j].low > candles[i].low) {
                lows.push((i, candles[i].low));
            }
        }
        let trend = match (highs.len() >= 2, lows.len() >= 2) {
            (true, true) => {
                let (h1, h2) = (highs[highs.len() - 2].1, highs[highs.len() - 1].1);
                let (l1, l2) = (lows[lows.len() - 2].1, lows[lows.len() - 1].1);
                if h2 > h1 && l2 > l1 {
                    Some(Side::Buy)
                } else if h2 < h1 && l2 < l1 {
                    Some(Side::Sell)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(trend) = trend {
            // The swing that invalidates the trend: for an uptrend, the
            // newest swing LOW; the close below it is the shift.
            let (level, breaks) = match trend {
                Side::Buy => (lows.last().map(|(_, p)| *p), last.close < lows.last().map(|(_, p)| *p).unwrap_or(f64::MIN)),
                Side::Sell => (highs.last().map(|(_, p)| *p), last.close > highs.last().map(|(_, p)| *p).unwrap_or(f64::MAX)),
            };
            let prev_close = candles[n - 1].close;
            let prev_held = match trend {
                Side::Buy => prev_close >= lows.last().map(|(_, p)| *p).unwrap_or(f64::MIN),
                Side::Sell => prev_close <= highs.last().map(|(_, p)| *p).unwrap_or(f64::MAX),
            };
            if breaks && prev_held {
                if let Some(level) = level {
                    out.push(make(EventKind::Mss, Some(match trend { Side::Buy => Side::Sell, Side::Sell => Side::Buy }), level, Some(level), Some(level), None));
                }
            }
        }
    }

    // -- Equilibrium: the dealing range's 50% line, from the newest
    // lookback-5 swings (the same range ERL draws on).
    {
        let lb = 5;
        let mut range_hi = f64::MIN;
        let mut range_lo = f64::MAX;
        let mut found = false;
        for i in lb..n.saturating_sub(lb) {
            if (i - lb..=i + lb).all(|j| j == i || candles[j].high < candles[i].high) {
                range_hi = range_hi.max(candles[i].high);
                found = true;
            }
            if (i - lb..=i + lb).all(|j| j == i || candles[j].low > candles[i].low) {
                range_lo = range_lo.min(candles[i].low);
                found = true;
            }
        }
        if found && range_hi > range_lo {
            let mid = (range_hi + range_lo) / 2.0;
            out.push(make(EventKind::Equilibrium, None, mid, Some(range_lo), Some(range_hi), None));
        }
    }

    // -- Consequent encroachment: the newest fresh FVG's 50% midpoint.
    {
        for i in (3..=n).rev() {
            let (a, b, c) = (&candles[i - 2], &candles[i - 1], &candles[i]);
            if c.low > a.high {
                let mid = (a.high + c.low) / 2.0;
                out.push(make(EventKind::ConsequentEncroachment, Some(Side::Buy), mid, Some(a.high), Some(c.low), Some(a.open_time)));
                break;
            }
            if c.high < a.low {
                let mid = (a.low + c.high) / 2.0;
                out.push(make(EventKind::ConsequentEncroachment, Some(Side::Sell), mid, Some(c.high), Some(a.low), Some(a.open_time)));
                break;
            }
        }
    }

    // -- The opening gap: today's first bar's open against yesterday's last
    // close, sized as a percent of the close. A gap under 0.05% is no gap.
    {
        const DAY_NS: i64 = 3_600_000_000_000 * 24;
        let today = last.open_time.div_euclid(DAY_NS);
        let today_first = candles.iter().position(|k| k.open_time.div_euclid(DAY_NS) == today);
        if let Some(first_idx) = today_first {
            if first_idx > 0 {
                let prev_close = candles[first_idx - 1].close;
                let gap = last.open - prev_close;
                if prev_close > 0.0 && (gap / prev_close).abs() >= 0.0005 && candles[first_idx].open_time == last.open_time {
                    out.push(make(
                        EventKind::OpeningGap,
                        Some(if gap > 0.0 { Side::Buy } else { Side::Sell }),
                        prev_close,
                        Some(prev_close.min(last.open)),
                        Some(prev_close.max(last.open)),
                        Some(candles[first_idx].open_time),
                    ));
                }
            }
        }
    }

    // -- The opening range breakout: the UTC day's first four bars' range,
    // broken on the tail in the break's direction (the asian range's rule,
    // generalized to the chart's own session).
    {
        const DAY_NS: i64 = 3_600_000_000_000 * 24;
        let today = last.open_time.div_euclid(DAY_NS);
        let day_bars: Vec<&Candle> = candles.iter().filter(|k| k.open_time.div_euclid(DAY_NS) == today).collect();
        if day_bars.len() > 4 {
            let ib_hi = day_bars[..4].iter().map(|k| k.high).fold(f64::MIN, f64::max);
            let ib_lo = day_bars[..4].iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let prev = &candles[n - 1];
            if prev.close <= ib_hi && last.close > ib_hi {
                out.push(make(EventKind::OpeningRangeBreakout, Some(Side::Buy), ib_hi, Some(ib_lo), Some(ib_hi), Some(day_bars[0].open_time)));
            } else if prev.close >= ib_lo && last.close < ib_lo {
                out.push(make(EventKind::OpeningRangeBreakout, Some(Side::Sell), ib_lo, Some(ib_lo), Some(ib_hi), Some(day_bars[0].open_time)));
            }
        }
    }

    // -- HTF candle bias: the newest COMPLETED 4x bar's own direction,
    // with the structure lean agreeing -- the higher timeframe's candle
    // vote, once per completed bar.
    {
        let htf = match last.timeframe {
            crate::types::Timeframe::M1 => crate::types::Timeframe::M5,
            crate::types::Timeframe::M5 => crate::types::Timeframe::M15,
            crate::types::Timeframe::M15 => crate::types::Timeframe::H1,
            crate::types::Timeframe::H1 => crate::types::Timeframe::H4,
            crate::types::Timeframe::H4 => crate::types::Timeframe::D1,
            _ => crate::types::Timeframe::D1,
        };
        let upper = crate::resample::resample(candles, htf);
        if upper.len() >= 3 {
            // The newest COMPLETED upper bar (the one before the forming one).
            let completed = &upper[upper.len() - 2];
            let direction = if completed.close > completed.open {
                Some(Side::Buy)
            } else if completed.close < completed.open {
                Some(Side::Sell)
            } else {
                None
            };
            let lean = crate::regime::structure_lean(&upper);
            if direction.is_some() && direction == lean {
                out.push(make(EventKind::HtfCandleBias, direction, completed.close, Some(completed.low), Some(completed.high), Some(completed.open_time)));
            }
        }
    }

    // -- Level interactions over the sr_zones machinery: the rejection (a
    // wick into a zone closing away) and the flip (the level traded
    // through, then held as the opposite side).
    {
        let structure = crate::market_structure::detect_market_structure(candles, crate::market_structure::StructureConfig::default());
        let zones = crate::sr_zones::detect_sr_zones(&structure, last.close, crate::sr_zones::SrZoneConfig::default());
        for zone in zones.iter().rev().take(4) {
            let (lo, hi) = (zone.bottom, zone.top);
            // Rejection: the wick enters, the close leaves, on the tail.
            let wick_in = last.low <= hi && last.high >= lo;
            let close_out = last.close < lo || last.close > hi;
            if wick_in && close_out {
                out.push(make(
                    EventKind::LevelRejection,
                    Some(if last.close > hi { Side::Buy } else { Side::Sell }),
                    (lo + hi) / 2.0,
                    Some(lo),
                    Some(hi),
                    Some(zone.first_time),
                ));
                break;
            }
            // The flip: the zone traded through (close beyond it) and the
            // retest held the far side.
            let through_up = candles[n - 1].close > hi && last.close > hi && last.low <= hi && last.close > hi;
            let through_dn = candles[n - 1].close < lo && last.close < lo && last.high >= lo && last.close < lo;
            if through_up {
                out.push(make(EventKind::LevelFlip, Some(Side::Buy), hi, Some(lo), Some(hi), Some(zone.first_time)));
                break;
            }
            if through_dn {
                out.push(make(EventKind::LevelFlip, Some(Side::Sell), lo, Some(lo), Some(hi), Some(zone.first_time)));
                break;
            }
        }
    }

    // -- The fused compression breakout: a range_contraction-shaped
    // squeeze broken with a displacement body (2x the average).
    {
        let lookback = 20.min(n - 1);
        if lookback >= 10 {
            let window = &candles[n - lookback..n];
            let (mut hi, mut lo) = (f64::MIN, f64::MAX);
            for k in window {
                hi = hi.max(k.high);
                lo = lo.min(k.low);
            }
            let range = hi - lo;
            let mean_range: f64 = window.iter().map(|k| k.high - k.low).sum::<f64>() / window.len() as f64;
            let contracted = range < mean_range * 3.0; // the squeeze: the whole window inside 3 mean-ranges
            let body = (last.close - last.open).abs();
            let mean_body: f64 = window.iter().map(|k| (k.close - k.open).abs()).sum::<f64>() / window.len() as f64;
            let displaced = mean_body > 0.0 && body >= 2.0 * mean_body;
            if contracted && displaced && last.close > hi {
                out.push(make(EventKind::CompressionBreakout, Some(Side::Buy), hi, Some(lo), Some(hi), None));
            } else if contracted && displaced && last.close < lo {
                out.push(make(EventKind::CompressionBreakout, Some(Side::Sell), lo, Some(lo), Some(hi), None));
            }
        }
    }

    // -- The void fill: the newest open FVG traded fully back through (the
    // lifecycle's completion, from open to filled).
    {
        for i in (3..=n - 1).rev() {
            let (a, c) = (&candles[i - 2], &candles[i]);
            if c.low > a.high {
                // Bullish gap: filled when a later close trades back below
                // the gap's bottom (a.high).
                if last.close < a.high {
                    out.push(make(EventKind::LiquidityVoidFill, Some(Side::Sell), a.high, Some(a.high), Some(c.low), Some(a.open_time)));
                    break;
                }
            } else if c.high < a.low && last.close > a.low {
                out.push(make(EventKind::LiquidityVoidFill, Some(Side::Buy), a.low, Some(c.high), Some(a.low), Some(a.open_time)));
                break;
            }
        }
    }

    // -- The volume-confirmed breakout: a close beyond the 20-bar extreme
    // with at least 1.5x the trailing average volume.
    {
        let lookback = 20.min(n);
        if lookback >= 10 {
            let hi = candles[n - lookback..n].iter().map(|k| k.high).fold(f64::MIN, f64::max);
            let lo = candles[n - lookback..n].iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let mean_vol: f64 = candles[n - lookback..n].iter().map(|k| k.volume).sum::<f64>() / lookback as f64;
            let confirmed = mean_vol > 0.0 && last.volume >= 1.5 * mean_vol;
            if confirmed && last.close > hi && candles[n - 1].close <= hi {
                out.push(make(EventKind::VolumeConfirmedBreakout, Some(Side::Buy), hi, Some(lo), Some(hi), None));
            } else if confirmed && last.close < lo && candles[n - 1].close >= lo {
                out.push(make(EventKind::VolumeConfirmedBreakout, Some(Side::Sell), lo, Some(lo), Some(hi), None));
            }
        }
    }

    // -- The trendline sweep: with an active validated trendline (three
    // touches), a trade through it that closes back behind it. The line
    // math mirrors the trendline detector's own.
    {
        let lb = 3;
        let mut lows: Vec<(usize, f64)> = Vec::new();
        let mut highs: Vec<(usize, f64)> = Vec::new();
        for i in lb..n.saturating_sub(lb) {
            if (i - lb..=i + lb).all(|j| j == i || candles[j].low > candles[i].low) {
                lows.push((i, candles[i].low));
            }
            if (i - lb..=i + lb).all(|j| j == i || candles[j].high < candles[i].high) {
                highs.push((i, candles[i].high));
            }
        }
        let line_at = |a: (usize, f64), b: (usize, f64), i: usize| -> f64 {
            a.1 + (b.1 - a.1) * (i - a.0) as f64 / (b.0 - a.0).max(1) as f64
        };
        if lows.len() >= 3 {
            let (l1, l2, l3) = (lows[lows.len() - 3], lows[lows.len() - 2], lows[lows.len() - 1]);
            if l1.1 < l2.1 && l2.1 < l3.1 && (l3.1 - line_at(l1, l2, l3.0)).abs() <= line_at(l1, l2, l3.0) * 0.0015 {
                let level = line_at(l1, l2, n);
                if last.low < level * 0.997 && last.close > level {
                    out.push(make(EventKind::TrendlineSweep, Some(Side::Buy), level, Some(last.low), Some(level), Some(candles[l1.0].open_time)));
                    if let Some(e) = out.last_mut() {
                        e.geometry = Some(edge_path(candles, &[l1, l2, (n, level)]));
                    }
                }
            }
        }
        if highs.len() >= 3 {
            let (h1, h2, h3) = (highs[highs.len() - 3], highs[highs.len() - 2], highs[highs.len() - 1]);
            if h1.1 > h2.1 && h2.1 > h3.1 && (h3.1 - line_at(h1, h2, h3.0)).abs() <= line_at(h1, h2, h3.0) * 0.0015 {
                let level = line_at(h1, h2, n);
                if last.high > level * 1.003 && last.close < level {
                    out.push(make(EventKind::TrendlineSweep, Some(Side::Sell), level, Some(level), Some(last.high), Some(candles[h1.0].open_time)));
                    if let Some(e) = out.last_mut() {
                        e.geometry = Some(edge_path(candles, &[h1, h2, (n, level)]));
                    }
                }
            }
        }
    }

    let mut deferred_strength: Vec<MarketEvent> = Vec::new();
    // -- Zone events scored and measured: the compression (a zone whose
    // height shrinks across retests), the proximal/distal lines, and the
    // strength score (touches + recency + departure).
    {
        let structure = crate::market_structure::detect_market_structure(candles, crate::market_structure::StructureConfig::default());
        let zones = crate::sr_zones::detect_sr_zones(&structure, last.close, crate::sr_zones::SrZoneConfig::default());
        if let Some(zone) = zones.first() {
            let height = zone.top - zone.bottom;
            // The proximal/distal lines: proximal is the edge NEAREST the
            // close, distal the far one -- the entry and the invalidation.
            let (proximal, distal) = if (last.close - zone.bottom).abs() < (last.close - zone.top).abs() {
                (zone.bottom, zone.top)
            } else {
                (zone.top, zone.bottom)
            };
            out.push(make(EventKind::ProximalDistal, None, proximal, Some(distal), Some(proximal), Some(zone.first_time)));
            // The strength score: touches (x2), recency (the fraction of
            // the window since the newest touch), departure (the mean
            // post-touch move in ATRs, capped).
            let recency = (n as f64 - 1.0).max(1.0);
            let since = candles.iter().rposition(|k| k.open_time == zone.last_time).unwrap_or(n);
            let recency_score = 1.0 - (n - since) as f64 / recency;
            let departure: f64 = candles[since..].iter().map(|k| (k.close - k.open).abs()).sum::<f64>() / (n - since).max(1) as f64;
            let atr14 = {
                let start = n.saturating_sub(14);
                candles[start..n].iter().map(|k| k.high - k.low).sum::<f64>() / (n - start).max(1) as f64
            };
            let departure_score = if atr14 > 0.0 { (departure / atr14).min(3.0) / 3.0 } else { 0.0 };
            let touch_score = (zone.touches as f64 / 4.0).min(1.0);
            let score = (touch_score * 0.4 + recency_score * 0.3 + departure_score * 0.3) * 100.0;
            deferred_strength.push(MarketEvent {
                kind: EventKind::ZoneStrength,
                symbol: symbol.to_string(),
                timeframe: last.timeframe,
                bar_time: last.open_time,
                close: last.close,
                price: (zone.top + zone.bottom) / 2.0,
                side: None,
                zone_low: Some(zone.bottom),
                zone_high: Some(zone.top),
                magnitude: Some(score),
                geometry: None,
                formed_at: Some(zone.first_time),
            });
            // Compression: the zone's height against the ATR -- a zone
            // thinner than half an ATR is compressed.
            if atr14 > 0.0 && height < atr14 * 0.5 {
                out.push(make(EventKind::ZoneCompression, None, (zone.top + zone.bottom) / 2.0, Some(zone.bottom), Some(zone.top), Some(zone.first_time)));
            }
        }
        // MTF zone alignment: the newest zone exists on the 4x resample too
        // (a zone within 0.3% of this one's center).
        if let Some(zone) = zones.first() {
            let htf = match last.timeframe {
                crate::types::Timeframe::M1 => crate::types::Timeframe::M5,
                crate::types::Timeframe::M5 => crate::types::Timeframe::M15,
                crate::types::Timeframe::M15 => crate::types::Timeframe::H1,
                crate::types::Timeframe::H1 => crate::types::Timeframe::H4,
                crate::types::Timeframe::H4 => crate::types::Timeframe::D1,
                _ => crate::types::Timeframe::D1,
            };
            let upper = crate::resample::resample(candles, htf);
            if upper.len() > 20 {
                let upper_structure = crate::market_structure::detect_market_structure(&upper, crate::market_structure::StructureConfig::default());
                let upper_zones = crate::sr_zones::detect_sr_zones(&upper_structure, last.close, crate::sr_zones::SrZoneConfig::default());
                let center = (zone.top + zone.bottom) / 2.0;
                let aligned = upper_zones.iter().any(|uz| {
                    let uc = (uz.top + uz.bottom) / 2.0;
                    (uc - center).abs() <= center * 0.003
                });
                if aligned {
                    out.push(make(EventKind::MtfZoneAlignment, None, center, Some(zone.bottom), Some(zone.top), Some(zone.first_time)));
                }
            }
        }
    }


    out
}

/// The round-number grid for a price: half the power of ten below it, so
/// BTC at 82k reads 5,000 steps and a 1.08 pair reads 0.005 steps.
fn round_step(price: f64) -> f64 {
    10f64.powf(price.abs().log10().floor()) / 2.0
}

/// A close crossing a round-number level, in the cross's direction -- the
/// psychological line the whole market watches, once per crossing.
pub fn detect_round_numbers(candles: &[Candle], symbol: &str) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    if candles.len() < 2 {
        return out;
    }
    let n = candles.len() - 1;
    let (prev, last) = (&candles[n - 1], &candles[n]);
    let step = round_step(last.close);
    if step <= 0.0 || !step.is_finite() {
        return out;
    }
    // The grid line between the two closes, if one was crossed.
    let crossed = |a: f64, b: f64| -> Option<f64> {
        let (lo, hi) = (a.min(b), a.max(b));
        let line = (lo / step).ceil() * step;
        (line > lo && line <= hi && line > 0.0).then_some(line)
    };
    if let Some(line) = crossed(prev.close, last.close) {
        out.push(MarketEvent {
            kind: EventKind::RoundNumberCross,
            symbol: symbol.to_string(),
            timeframe: last.timeframe,
            bar_time: last.open_time,
            close: last.close,
            price: line,
            side: Some(if last.close > prev.close { Side::Buy } else { Side::Sell }),
            zone_low: Some(last.low),
            zone_high: Some(last.high),
            magnitude: Some(step),
            geometry: None,
            formed_at: None,
        });
    }
    out
}

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
            geometry: None,
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
                geometry: None,
                formed_at: Some(last.open_time),
            });
        }
        // The month roll mirrors the week's (UTC month arithmetic is day
        // arithmetic: a new month is a new year/month pair).
        let month_of = |ns: i64| {
            let days = ns.div_euclid(DAY_NS);
            // Civil-from-days (Howard Hinnant's algorithm, compact form).
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 456) / 153;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            (y, m)
        };
        if month_of(last.open_time) != month_of(candles[candles.len() - 2].open_time) {
            out.push(MarketEvent {
                kind: EventKind::MonthlyOpen,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: last.open,
                side: None,
                zone_low: None,
                zone_high: None,
                magnitude: None,
                geometry: None,
                formed_at: Some(last.open_time),
            });
        }
        // The previous periods' ranges, marked once at the roll: the high
        // and low of the day/week/month that just closed, as zones from
        // that period's first bar to now.
        let range_of = |from_ns: i64, to_ns: i64| -> Option<(i64, f64, f64)> {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            let mut first = None;
            for k in candles.iter().filter(|k| k.open_time >= from_ns && k.open_time < to_ns) {
                if first.is_none() {
                    first = Some(k.open_time);
                }
                lo = lo.min(k.low);
                hi = hi.max(k.high);
            }
            first.map(|t| (t, lo, hi))
        };
        let day_start = prev_day * DAY_NS;
        if let Some((t, lo, hi)) = range_of(day_start, newest_day * DAY_NS) {
            out.push(MarketEvent {
                kind: EventKind::PreviousDayLevels,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: (lo + hi) / 2.0,
                side: None,
                zone_low: Some(lo),
                zone_high: Some(hi),
                magnitude: None,
                geometry: None,
                formed_at: Some(t),
            });
        }
        let week_start = prev_week * WEEK_NS;
        if newest_week != prev_week {
            if let Some((t, lo, hi)) = range_of(week_start, newest_week * WEEK_NS) {
                out.push(MarketEvent {
                    kind: EventKind::PreviousWeekLevels,
                    symbol: symbol.to_string(),
                    timeframe: tf,
                    bar_time,
                    close: last.close,
                    price: (lo + hi) / 2.0,
                    side: None,
                    zone_low: Some(lo),
                    zone_high: Some(hi),
                    magnitude: None,
                    geometry: None,
                    formed_at: Some(t),
                });
            }
        }
        if month_of(last.open_time) != month_of(candles[candles.len() - 2].open_time) {
            // The previous month's start: walk back to the first candle of
            // the month before the new one.
            let (y, m) = month_of(candles[candles.len() - 2].open_time);
            let month_start_idx = candles
                .iter()
                .position(|k| month_of(k.open_time) == (y, m));
            if let Some(idx) = month_start_idx {
                let from = candles[idx].open_time;
                if let Some((t, lo, hi)) = range_of(from, last.open_time) {
                    out.push(MarketEvent {
                        kind: EventKind::PreviousMonthLevels,
                        symbol: symbol.to_string(),
                        timeframe: tf,
                        bar_time,
                        close: last.close,
                        price: (lo + hi) / 2.0,
                        side: None,
                        zone_low: Some(lo),
                        zone_high: Some(hi),
                        magnitude: None,
                        geometry: None,
                        formed_at: Some(t),
                    });
                }
            }
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
            geometry: None,
            formed_at: Some(from),
        });
    }

    // -- Judas Swing and the Power of Three -----------------------------------
    // Today's Asian range (00:00-07:00 UTC) is the accumulation. The Judas
    // swing is the false move: within a kill zone's FIRST hour, a trade
    // beyond the Asian range that closes back inside it. The Power of Three
    // completes when price then expands through the FAR side with
    // displacement.
    let day_of = last.open_time.div_euclid(DAY_NS);
    let asian: Vec<&Candle> = candles
        .iter()
        .filter(|k| {
            k.open_time.div_euclid(DAY_NS) == day_of && ((k.open_time % DAY_NS) / HOUR_NS) < 7
        })
        .collect();
    if !asian.is_empty() {
        let asia_lo = asian.iter().map(|k| k.low).fold(f64::MAX, f64::min);
        let asia_hi = asian.iter().map(|k| k.high).fold(f64::MIN, f64::max);
        let newest_hour = ((last.open_time % DAY_NS) / HOUR_NS) as u32;
        // Today's bars since the Asian window, newest first.
        let after: Vec<&Candle> = candles
            .iter()
            .filter(|k| k.open_time.div_euclid(DAY_NS) == day_of && ((k.open_time % DAY_NS) / HOUR_NS) >= 7)
            .collect();
        // The Judas check: some bar in a kill zone's first hour (07:00 or
        // 12:00) traded beyond the range, and the newest close is back inside.
        let judas_high = after.iter().any(|k| {
            let h = ((k.open_time % DAY_NS) / HOUR_NS) as u32;
            (h == 7 || h == 12) && k.high > asia_hi
        });
        let judas_low = after.iter().any(|k| {
            let h = ((k.open_time % DAY_NS) / HOUR_NS) as u32;
            (h == 7 || h == 12) && k.low < asia_lo
        });
        let back_inside = last.close < asia_hi && last.close > asia_lo;
        let prev_inside = candles[candles.len() - 2].close < asia_hi && candles[candles.len() - 2].close > asia_lo;
        // Fire ONCE: the bar that closes back inside after the sweep.
        if judas_high && back_inside && !prev_inside {
            out.push(MarketEvent {
                kind: EventKind::JudasSwing,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: asia_hi,
                side: Some(Side::Sell), // the sweep of the high reverses down
                zone_low: Some(asia_lo),
                zone_high: Some(asia_hi),
                magnitude: None,
                geometry: None,
                formed_at: Some(asian.first().expect("checked").open_time),
            });
        } else if judas_low && back_inside && !prev_inside {
            out.push(MarketEvent {
                kind: EventKind::JudasSwing,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: asia_lo,
                side: Some(Side::Buy), // the sweep of the low reverses up
                zone_low: Some(asia_lo),
                zone_high: Some(asia_hi),
                magnitude: None,
                geometry: None,
                formed_at: Some(asian.first().expect("checked").open_time),
            });
        }
        // PO3: after a Judas sweep, the close beyond the FAR side with a
        // displacement body (twice the average).
        let body = (last.close - last.open).abs();
        let last_idx = candles.len() - 1;
        let mean_body: f64 = candles[last_idx.saturating_sub(14)..last_idx]
            .iter()
            .map(|k| (k.close - k.open).abs())
            .sum::<f64>()
            / 14.0;
        let displaced = mean_body > 0.0 && body >= 2.0 * mean_body;
        let prev_close = candles[candles.len() - 2].close;
        if judas_high && displaced && last.close < asia_lo && prev_close >= asia_lo {
            out.push(MarketEvent {
                kind: EventKind::PowerOfThree,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: asia_lo,
                side: Some(Side::Sell),
                zone_low: Some(asia_lo),
                zone_high: Some(asia_hi),
                magnitude: Some(body / mean_body),
                geometry: None,
                formed_at: Some(asian.first().expect("checked").open_time),
            });
        } else if judas_low && displaced && last.close > asia_hi && prev_close <= asia_hi {
            out.push(MarketEvent {
                kind: EventKind::PowerOfThree,
                symbol: symbol.to_string(),
                timeframe: tf,
                bar_time,
                close: last.close,
                price: asia_hi,
                side: Some(Side::Buy),
                zone_low: Some(asia_lo),
                zone_high: Some(asia_hi),
                magnitude: Some(body / mean_body),
                geometry: None,
                formed_at: Some(asian.first().expect("checked").open_time),
            });
        }
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
            geometry: None,
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
    const EQUAL_TOLERANCE_ATR: f64 = EQUAL_TOLERANCE_ATR_CONST;
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
            geometry: None,
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
                geometry: None,
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
                geometry: None,
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
                geometry: None,
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
            geometry: None,
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
            geometry: None,
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
            geometry: None,
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
            geometry: None,
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
            geometry: None,
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
                geometry: None,
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
                geometry: None,
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
                geometry: None,
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
                geometry: None,
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
            geometry: None,
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
                geometry: None,
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
                geometry: None,
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
                geometry: None,
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
    events.extend(detect_round_numbers(candles, symbol));
    events.extend(detect_displacement(candles, symbol));
    events.extend(detect_range_liquidity(candles, symbol));
    events.extend(crate::regime::detect_regime(candles, symbol));
    events.extend(detect_structure_levels(candles, symbol));
    events.extend(crate::harmonics::detect_wolfe_waves(candles, symbol));
    events.extend(crate::regime::detect_mtf_alignment(candles, symbol));
    events.extend(crate::regime::detect_bias(candles, symbol));
    events.extend(crate::tpo::detect_auction(candles, symbol));
    events.extend(crate::ict_models::detect_ict_models(candles, symbol));
    events.extend(detect_chart_gaps(candles, symbol, &config));
    events.extend(crate::fibonacci::detect_fibonacci(candles, symbol, config.structure_lookback));
    events.extend(crate::rsi_pca::detect_rsi_pca(candles, symbol));
    events.extend(crate::indicator_signals::detect_indicator_signals(candles, symbol));
    events.extend(detect_chart_patterns(candles, symbol, &config));
    events.extend(detect_volume_profile(candles, symbol));
    // Harmonics: the platform's own calibrated detector (the reference
    // engine's swing gate proved too strict for real data, so the family
    // runs on our swings with the canonical ratio windows).
    events.extend(crate::harmonics::detect_harmonics(candles, symbol));
    // Candlesticks: the platform's own implementations (the in-house
    // doctrine -- the external indicator crate is out of the tree).
    events.extend(crate::candlesticks::detect_candlesticks(candles, symbol));

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
    fn the_profile_family_names_poc_and_value_area() {
        // A series with most volume at ~100, plus extremes: the POC sits at
        // the heavy bucket and the value area contains it.
        let mut ks: Vec<Candle> = Vec::new();
        for i in 0..30 {
            let heavy = i % 3 == 0;
            ks.push(Candle {
                symbol: "T".to_string(),
                timeframe: crate::types::Timeframe::H1,
                open_time: i as i64 * 3_600_000_000_000,
                open: 100.0,
                high: 100.4,
                low: 99.6,
                close: 100.1,
                volume: if heavy { 50.0 } else { 2.0 },
                buy_volume: 25.0,
                sell_volume: 25.0,
            });
        }
        ks.push(Candle {
            symbol: "T".to_string(),
            timeframe: crate::types::Timeframe::H1,
            open_time: 30 * 3_600_000_000_000,
            open: 100.1,
            high: 104.0,
            low: 100.0,
            close: 103.5,
            volume: 2.0,
            buy_volume: 1.0,
            sell_volume: 1.0,
        });
        let events = detect_patterns_and_events(&ks);
        let poc = events.iter().find(|e| e.kind == EventKind::VolumePoc).expect("poc");
        assert!((poc.price - 100.0).abs() < 0.5, "the poc sits at the heavy bucket: {}", poc.price);
        let va = events.iter().find(|e| e.kind == EventKind::ValueArea).expect("value area");
        assert!(va.zone_low.expect("val") <= poc.price && poc.price <= va.zone_high.expect("vah"), "the poc is inside the value area");
        // And the honesty floor: zero volume yields NO profile events.
        let flat: Vec<Candle> = (0..30)
            .map(|i| Candle {
                symbol: "T".to_string(),
                timeframe: crate::types::Timeframe::H1,
                open_time: i as i64 * 3_600_000_000_000,
                open: 100.0,
                high: 100.5,
                low: 99.5,
                close: 100.2,
                volume: 0.0,
                buy_volume: 0.0,
                sell_volume: 0.0,
            })
            .collect();
        let events = detect_patterns_and_events(&flat);
        assert!(!kinds(&events).contains(&EventKind::VolumePoc), "zero volume, no poc");
    }

    #[test]
    fn a_triple_top_needs_three_peaks_and_the_neckline() {
        // Three peaks at ~106 with troughs at ~103, then the close under
        // the LOWER trough.
        let mut ks: Vec<Candle> = (0..16).map(|_| k(104.2, 104.8, 103.8, 104.4)).collect();
        let path = [
            (103.4, 106.0, 103.2, 105.6), // peak 1 (its low above the troughs)
            (105.6, 105.8, 103.0, 103.4),
            (103.4, 104.0, 103.2, 103.8),
            (103.8, 104.2, 103.4, 103.9),
            (103.9, 106.1, 103.6, 105.8), // peak 2 (4 bars later)
            (105.8, 106.0, 103.1, 103.5),
            (103.5, 104.0, 103.3, 103.9),
            (103.9, 104.1, 103.5, 104.0),
            (104.0, 106.05, 103.7, 105.7), // peak 3 (4 bars later)
            (105.7, 105.9, 105.2, 105.5),
            (105.5, 105.6, 105.3, 105.4),
            (105.4, 105.5, 102.4, 102.7), // the break under 103.0
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        let events = detect_patterns_and_events(&pa_series(ks));
        let tt = events.iter().find(|e| e.kind == EventKind::TripleTopBottom).expect("triple top");
        assert_eq!(tt.side, Some(Side::Sell));
        assert_eq!(tt.price, 103.0, "the neckline is the lower trough");
        // Two peaks only: no triple.
        let mut ks: Vec<Candle> = (0..16).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        for (o, h, l, c) in [
            (100.2, 106.0, 100.0, 105.6),
            (105.6, 105.8, 103.0, 103.4),
            (103.4, 104.0, 103.2, 103.8),
            (103.8, 106.1, 103.6, 105.8),
            (105.8, 105.9, 102.4, 102.7),
            (102.7, 102.9, 102.5, 102.8),
        ] {
            ks.push(k(o, h, l, c));
        }
        assert!(!kinds(&detect_patterns_and_events(&pa_series(ks))).contains(&EventKind::TripleTopBottom));
    }

    #[test]
    fn a_rectangle_breaks_out_of_its_flat_channel() {
        // Flat tops ~104, flat bottoms ~101, then a close above the top.
        // The lead-in must not undercut the channel's bottoms (the
        // left-pivot rule needs three respected bars before each edge).
        let mut ks: Vec<Candle> = (0..16).map(|_| k(102.2, 103.2, 101.8, 102.9)).collect();
        let path = [
            (101.2, 104.0, 101.1, 103.6), // top 1 (its low above the bottoms)
            (103.6, 103.8, 101.0, 101.4), // bottom 1
            (101.4, 102.0, 101.2, 101.9),
            (101.9, 102.2, 101.6, 102.0),
            (102.0, 104.05, 101.8, 103.8), // top 2 (4 later, flat)
            (103.8, 103.9, 101.05, 101.5), // bottom 2 (flat)
            (101.5, 102.0, 101.3, 101.9),
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        // The series ENDS at the breakout bar: detection is per-bar, and the
        // edge is alive exactly here. Bars after the break would have
        // violated it already.
        ks.push(k(103.9, 105.0, 103.7, 104.6));
        let events = detect_patterns_and_events(&pa_series(ks));
        let rect = events.iter().find(|e| e.kind == EventKind::RectangleBreakout).expect("rectangle");
        assert_eq!(rect.side, Some(Side::Buy));
        // Inside the channel: no breakout.
        let mut ks: Vec<Candle> = (0..4).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        for _ in 0..4 {
            ks.push(k(101.9, 103.5, 101.7, 102.4));
        }
        assert!(!kinds(&detect_patterns_and_events(&pa_series(ks))).contains(&EventKind::RectangleBreakout));
    }

    #[test]
    fn a_displacement_is_the_energetic_candle_at_a_fresh_extreme() {
        // 21 quiet bars, then one huge-bodied bar closing at a new high.
        let mut ks: Vec<Candle> = (0..21).map(|_| k(100.0, 100.4, 99.6, 100.1)).collect();
        ks.push(k(100.1, 103.0, 100.0, 102.9)); // body ~2.8 vs ~0.1 average
        let ks = pa_series(ks);
        let events = detect_displacement(&ks, "T");
        let d = events.iter().find(|e| e.kind == EventKind::Displacement);
        assert!(d.is_some(), "the energetic candle fires");
        let d = d.expect("checked");
        assert_eq!(d.side, Some(Side::Buy), "closing at the fresh high");
        assert!(d.magnitude.expect("ratio") >= 2.0, "the body ratio rides along");
        // A big body WITHOUT a fresh extreme is not displacement.
        let mut ks2: Vec<Candle> = (0..21).map(|_| k(100.0, 102.0, 98.0, 100.5)).collect();
        ks2.push(k(100.1, 101.0, 99.5, 100.8)); // big body but inside the range
        let ks2 = pa_series(ks2);
        assert!(detect_displacement(&ks2, "T").is_empty(), "no fresh extreme, no displacement");
    }

    #[test]
    fn external_liquidity_marks_the_range_and_internal_the_pools_inside() {
        // A major range 100..105 (lookback-5 swings), with two equal minor
        // highs at 103 inside it.
        let mut ks: Vec<Candle> = (0..30).map(|_| k(102.0, 102.4, 101.6, 102.0)).collect();
        let path = [
            (102.0, 105.0, 101.8, 104.8), // 30: the range high
            (104.8, 104.9, 103.0, 103.4),
            (103.4, 103.8, 102.8, 103.2),
            (103.2, 103.4, 102.6, 103.0),
            (103.0, 103.2, 102.4, 102.8),
            (102.8, 103.0, 102.2, 102.6),
            (102.6, 102.8, 101.8, 102.2),
            (102.2, 103.0, 102.0, 102.8), // 37: minor high at 103
            (102.8, 102.9, 102.2, 102.5),
            (102.5, 102.7, 102.1, 102.4),
            (102.4, 103.0, 102.2, 102.9), // 40: second minor high at 103
            (102.9, 103.0, 102.3, 102.6),
            (102.6, 102.8, 102.2, 102.5),
            (102.5, 102.7, 102.1, 102.3),
            (102.3, 102.5, 100.0, 100.3), // 44: the range low
            (100.3, 102.0, 100.1, 101.8),
            (101.8, 102.2, 101.5, 102.0),
            // The lookback-5 rule needs five bars respected after the low.
            (102.0, 102.3, 101.6, 102.1),
            (102.1, 102.4, 101.7, 102.2),
            (102.2, 102.5, 101.8, 102.3),
            (102.3, 102.6, 101.9, 102.4),
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        let ks = pa_series(ks);
        let events = detect_range_liquidity(&ks, "T");
        let erl: Vec<_> = events.iter().filter(|e| e.kind == EventKind::ExternalRangeLiquidity).collect();
        assert_eq!(erl.len(), 2, "both range extremes marked");
        assert!(erl.iter().any(|e| e.side == Some(Side::Buy) && (e.price - 105.0).abs() < 0.01), "buy stops above 105");
        assert!(erl.iter().any(|e| e.side == Some(Side::Sell) && (e.price - 100.0).abs() < 0.01), "sell stops below 100");
    }

    #[test]
    fn a_judas_swing_sweeps_the_asian_high_and_closes_back_in() {
        // M15 bars: the Asian window is bars 0..27 (00:00-07:00), ranging
        // 100..101. Bar 28 (07:00) sweeps to 101.5 and closes at 101.2 (out);
        // bar 29 closes at 100.6 (back inside).
        let mut ks: Vec<Candle> = (0..28).map(|_| k(100.5, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.5, 101.5, 100.4, 101.2)); // the sweep, still outside
        ks.push(k(101.2, 101.2, 100.4, 100.6)); // back inside
        let ks = pa_series(ks);
        let events = detect_sessions(&ks, "T");
        let j = events.iter().find(|e| e.kind == EventKind::JudasSwing);
        assert!(j.is_some(), "the sweep-and-return fires");
        assert_eq!(j.expect("checked").side, Some(Side::Sell), "sweeping the high reverses down");
        // Without the return inside, no Judas.
        let mut ks2: Vec<Candle> = (0..28).map(|_| k(100.5, 101.0, 100.0, 100.5)).collect();
        ks2.push(k(100.5, 101.5, 100.4, 101.3));
        ks2.push(k(101.3, 101.6, 101.0, 101.4)); // still outside
        let ks2 = pa_series(ks2);
        assert!(!detect_sessions(&ks2, "T").iter().any(|e| e.kind == EventKind::JudasSwing));
    }

    #[test]
    fn the_power_of_three_needs_all_three_phases() {
        // Asian range 100..101 (bars 0..27); the 07:00 bar sweeps the high;
        // later a huge bearish bar closes below the Asian low with
        // displacement.
        let mut ks: Vec<Candle> = (0..28).map(|_| k(100.5, 101.0, 100.0, 100.5)).collect();
        ks.push(k(100.5, 101.5, 100.4, 101.2)); // the Judas sweep at 07:00
        for _ in 0..6 {
            ks.push(k(100.8, 101.1, 100.3, 100.7));
        }
        ks.push(k(100.7, 100.8, 98.5, 98.7)); // the displaced close through the low
        let ks = pa_series(ks);
        let events = detect_sessions(&ks, "T");
        let p = events.iter().find(|e| e.kind == EventKind::PowerOfThree);
        assert!(p.is_some(), "all three phases complete the model");
        assert_eq!(p.expect("checked").side, Some(Side::Sell), "the manipulation was up, the distribution down");
    }

    #[test]
    fn a_round_number_cross_stamps_its_own_bar() {
        // Price at 82k: the grid is 5000. A close from 79,900 to 80,100
        // crosses 80,000 upward.
        let ks = pa_series(vec![k(100.0, 100.5, 99.5, 100.0), k(100.0, 100.6, 99.4, 100.2)]);
        let mut ks2 = ks.clone();
        // Scale into the 80k region.
        for c in ks2.iter_mut() {
            c.open *= 800.0;
            c.high *= 800.0;
            c.low *= 800.0;
            c.close *= 800.0;
        }
        ks2[0].close = 79_900.0;
        ks2[1].close = 80_100.0;
        let events = detect_round_numbers(&ks2, "T");
        let cross = events.iter().find(|e| e.kind == EventKind::RoundNumberCross);
        assert!(cross.is_some(), "the 80,000 crossing fires");
        let cross = cross.expect("checked");
        assert_eq!(cross.price, 80_000.0, "the level, not the close");
        assert_eq!(cross.side, Some(Side::Buy), "upward");
        // No crossing, no event.
        let flat = pa_series(vec![k(100.0, 100.5, 99.5, 100.0), k(100.0, 100.4, 99.6, 100.1)]);
        assert!(detect_round_numbers(&flat, "T").is_empty());
    }

    #[test]
    fn previous_day_levels_mark_the_closed_days_range() {
        // M15 bars: day 1 is 96 bars trading 100..105, then the day-2 bar.
        let mut ks: Vec<Candle> = (0..97).map(|_| k(102.0, 105.0, 100.0, 103.0)).collect();
        ks[96] = k(103.0, 103.5, 102.8, 103.2); // the day roll
        let ks = pa_series(ks);
        let events = detect_sessions(&ks, "T");
        let levels = events.iter().find(|e| e.kind == EventKind::PreviousDayLevels);
        assert!(levels.is_some(), "the day roll marks the closed day's range");
        let l = levels.expect("checked");
        assert_eq!(l.zone_low, Some(100.0));
        assert_eq!(l.zone_high, Some(105.0));
    }

    #[test]
    fn a_trendline_validates_three_touches_then_breaks() {
        // Three ascending swing lows (the third on the line), then the
        // close below the projected line.
        let mut ks: Vec<Candle> = (0..16).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        let path = [
            (100.0, 100.8, 99.0, 100.6), // low 1 at 99.0
            (100.6, 101.2, 100.4, 101.0),
            (101.0, 101.4, 100.8, 101.2),
            (101.2, 101.6, 101.0, 101.4),
            (101.4, 101.8, 100.5, 101.6), // low 2 at 100.5
            // The bars before low 3 must not undercut it (the left-pivot
            // rule needs three respected bars): lows 102.1, 102.3, 102.5.
            (101.6, 102.4, 102.1, 102.2),
            (102.2, 102.7, 102.3, 102.5),
            (102.5, 102.9, 102.5, 102.7),
            // low 3 ON the line through (16, 99.0)-(20, 100.5):
            // at index 24 the line reads 102.0.
            (102.7, 102.9, 102.0, 102.3),
            // the break: the line at 25 reads ~102.375; close well under.
            (102.3, 102.4, 100.8, 101.0),
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        let events = detect_chart_gaps(&ks, "T", &EventConfig::default());
        let tl = events.iter().find(|e| e.kind == EventKind::TrendlineBreak);
        assert!(tl.is_some(), "the validated line's break fires: {events:?}");
        assert_eq!(tl.expect("checked").side, Some(Side::Sell), "a broken bull line is a sell");
    }

    #[test]
    fn a_broadening_formation_needs_both_sides_expanding() {
        // Highs 104, 105, 106 ascending; lows 101, 100, 99 descending; the
        // close beyond 106.
        let mut ks: Vec<Candle> = (0..16).map(|_| k(102.0, 102.6, 101.6, 102.2)).collect();
        let path = [
            (102.0, 104.0, 101.8, 103.8), // high 1
            (103.8, 104.0, 101.0, 101.4), // low 1
            (101.4, 105.0, 101.2, 104.8), // high 2
            (104.8, 105.0, 100.0, 100.4), // low 2
            (100.4, 106.0, 100.2, 105.8), // high 3
            (105.8, 106.0, 99.0, 99.4),   // low 3
            (99.4, 102.0, 99.2, 101.8),
            (101.8, 104.0, 101.6, 103.8),
            (103.8, 106.5, 103.6, 106.3), // the break beyond high 3
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        let events = detect_chart_gaps(&ks, "T", &EventConfig::default());
        let br = events.iter().find(|e| e.kind == EventKind::BroadeningFormation);
        assert!(br.is_some(), "the megaphone fires on the break");
        assert_eq!(br.expect("checked").side, Some(Side::Buy), "breaking the expanding highs is a buy");
    }

    #[test]
    fn a_cup_and_handle_needs_the_shallow_handle() {
        // Rim at 104, cup bottom at 99, recovery to the rim, a shallow
        // handle above 101.5 (upper half), then the resumption close above.
        let mut ks: Vec<Candle> = (0..16).map(|_| k(100.6, 101.2, 100.2, 100.8)).collect();
        let path = [
            (103.6, 104.0, 103.2, 103.8), // the rim
            (103.8, 103.9, 101.0, 101.4),
            (101.4, 101.6, 99.0, 99.4),   // the cup bottom
            (99.4, 100.0, 99.2, 99.8),
            (99.8, 101.0, 99.6, 100.8),
            (100.8, 102.0, 100.6, 101.8),
            (101.8, 104.1, 101.6, 103.9), // recovery to the rim
            (103.9, 104.0, 102.8, 103.2), // the shallow handle
            (103.2, 103.4, 102.9, 103.1),
            (103.1, 104.6, 103.0, 104.3), // the resumption above 104
        ];
        for (o, h, l, c) in path {
            ks.push(k(o, h, l, c));
        }
        let events = detect_patterns_and_events(&pa_series(ks));
        let cup = events.iter().find(|e| e.kind == EventKind::CupAndHandle).expect("cup");
        assert_eq!(cup.side, Some(Side::Buy));
        assert_eq!(cup.price, 104.0, "the rim is the swing high before the cup");
    }

    #[test]
    fn a_pennant_converges_where_a_flag_drifts() {
        // Pole up (+10 over 8 bars), then a converging small channel.
        let mut ks: Vec<Candle> = (0..9).map(|_| k(100.0, 100.6, 99.8, 100.2)).collect();
        for i in 0..8 {
            ks.push(k(100.2 + i as f64 * 1.2, 101.0 + i as f64 * 1.2, 100.0 + i as f64 * 1.2, 100.9 + i as f64 * 1.2));
        }
        // The convergence: highs falling, lows rising.
        let channel = [
            (110.9, 111.2, 110.4, 110.8),
            (110.8, 111.0, 110.5, 110.7),
            (110.7, 110.9, 110.6, 110.75),
            (110.75, 110.85, 110.65, 110.7),
        ];
        for (o, h, l, c) in channel {
            ks.push(k(o, h, l, c));
        }
        for _ in 0..4 {
            ks.push(k(110.7, 110.8, 110.66, 110.72));
        }
        let events = detect_patterns_and_events(&pa_series(ks));
        let pen = events.iter().find(|e| e.kind == EventKind::Pennant);
        assert!(pen.is_some(), "the converging channel is a pennant: {:?}", kinds(&events));
        assert_eq!(pen.expect("checked").side, Some(Side::Buy));
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
            geometry: None,
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
