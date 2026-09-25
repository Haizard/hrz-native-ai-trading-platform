//! The multi-timeframe ladder (`docs/09`: "1D -> 4H -> 1H -> 5M").
//!
//! ## What is deterministic here and what is not
//!
//! Reading the ladder is deterministic: fetch each timeframe, build its
//! `MarketState`, order the results coarse to fine. Interpreting it is the
//! model's job.
//!
//! That split matters. The temptation is to let the model summarise the ladder
//! from raw candle arrays, at which point "the 4h trend is bullish" becomes a
//! judgement made by reading numbers off a chart in prose. [`LadderView::digest`]
//! instead computes the comparisons in Rust and hands the model statements that
//! are already true, so the model's contribution is synthesis rather than
//! arithmetic.

use analytics_core::market_structure::Trend;
use analytics_core::types::Timeframe;
use analytics_core::{build_market_state, MarketState, MarketStateConfig};
use serde::{Deserialize, Serialize};

use crate::error::AgentError;
use crate::skills::Skill;
use crate::tools::MarketDataSource;

/// The default ladder from `docs/09`, coarse to fine.
pub const DEFAULT_LADDER: [Timeframe; 4] =
    [Timeframe::D1, Timeframe::H4, Timeframe::H1, Timeframe::M5];

/// An ordered set of timeframes, always held coarse to fine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeframeLadder {
    timeframes: Vec<Timeframe>,
}

impl Default for TimeframeLadder {
    fn default() -> Self {
        Self::new(DEFAULT_LADDER.to_vec())
    }
}

impl TimeframeLadder {
    /// Build a ladder, sorting coarse to fine and dropping duplicates.
    #[must_use]
    pub fn new(timeframes: Vec<Timeframe>) -> Self {
        let mut timeframes = timeframes;
        // `Timeframe`'s `Ord` is by duration (see the explicit impl in
        // types.rs), so ascending is *finest first*. The ladder is read
        // top-down, which means descending by duration. Reversing the
        // comparison does that without a lookup table that would silently go
        // stale when a timeframe is added.
        timeframes.sort_unstable_by(|a, b| b.cmp(a));
        timeframes.dedup();
        Self { timeframes }
    }

    /// Parse a ladder from exchange-style strings, ignoring unknown values.
    #[must_use]
    pub fn from_strs(values: &[String]) -> Self {
        Self::new(
            values
                .iter()
                .filter_map(|value| value.parse::<Timeframe>().ok())
                .collect(),
        )
    }

    /// The ladder a skill declares, falling back to the default when it
    /// declares none.
    #[must_use]
    pub fn from_skill(skill: &Skill) -> Self {
        let ladder = Self::from_strs(&skill.conditions.timeframes);
        if ladder.is_empty() {
            Self::from_strs(&skill.preferred_timeframes)
        } else {
            ladder
        }
    }

    /// The timeframes, coarse to fine.
    #[must_use]
    pub fn timeframes(&self) -> &[Timeframe] {
        &self.timeframes
    }

    /// Whether the ladder has no timeframes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timeframes.is_empty()
    }

    /// Number of timeframes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.timeframes.len()
    }
}

/// One timeframe's read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameView {
    /// The timeframe.
    pub timeframe: Timeframe,
    /// The order-flow state on it.
    pub state: MarketState,
}

/// Every timeframe's read, coarse to fine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LadderView {
    /// Symbol analysed.
    pub symbol: String,
    /// Frames, coarse to fine.
    pub frames: Vec<FrameView>,
}

/// One line of the confluence read: a factor, its direction and its points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfluenceFactor {
    /// What the factor reads, e.g. `trend alignment`.
    pub name: String,
    /// `bullish`, `bearish` or `neutral`.
    pub direction: String,
    /// Points earned, out of [`Confluence::MAX_POINTS_PER_FACTOR`]-style
    /// weights that sum to 100.
    pub points: f64,
    /// The arithmetic behind it, citable verbatim.
    pub note: String,
}

/// The ladder's confluence, computed in Rust so the model never has to.
///
/// ## Why this exists in the engine and not in the prompt
///
/// "Are the timeframes agreeing?" is arithmetic over facts the engine already
/// holds: each frame's trend, value-area position, VWAP side, CVD and delta.
/// Left to the model, it is the single most error-prone mental step in the
/// thesis -- five frames times five readings, summed under attention pressure.
/// Computed here, it is one more block of statements that are already true,
/// and the model's contribution narrows to what only it can do: judgement.
///
/// Every factor is signed evidence: it earns points for bullish **or** for
/// bearish, never both. The bias is whichever side holds more of the 100
/// available points, with a 25% margin required to call it -- a 46/38 split is
/// *mixed*, not a faint bull, because a thesis built on a coin flip is worse
/// than a stand-aside.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confluence {
    /// Points on the bullish side, 0..=100.
    pub bullish: f64,
    /// Points on the bearish side, 0..=100.
    pub bearish: f64,
    /// `bullish`, `bearish` or `mixed`, by the margin rule above.
    pub bias: String,
    /// The dominant side's share of all awarded points, percent.
    pub agreement_pct: f64,
    /// The factors, in the order a reader should weigh them.
    pub factors: Vec<ConfluenceFactor>,
}

impl Confluence {
    /// The margin (as a fraction of the losing side) required to name a side.
    const MARGIN: f64 = 0.25;

    fn neutral_factors() -> Vec<ConfluenceFactor> {
        Vec::new()
    }

    /// A confluence over no readable frames: nothing is claimed.
    fn empty() -> Self {
        Self {
            bullish: 0.0,
            bearish: 0.0,
            bias: "mixed".into(),
            agreement_pct: 0.0,
            factors: Self::neutral_factors(),
        }
    }
}

/// Majority helper shared by the count-based factors: returns the winning
/// side and how many of `total` readings it holds, or neutral on a tie.
fn lean(bulls: usize, bears: usize, total: usize) -> (&'static str, f64, f64) {
    let total_f = total as f64;
    if total == 0 || bulls == bears {
        return ("neutral", 0.0, total_f);
    }
    if bulls > bears {
        ("bullish", bulls as f64 / total_f, total_f)
    } else {
        ("bearish", bears as f64 / total_f, total_f)
    }
}

impl LadderView {
    /// The coarsest frame -- macro context.
    #[must_use]
    pub fn highest(&self) -> Option<&FrameView> {
        self.frames.first()
    }

    /// The ladder's confluence, computed from the frames it already holds.
    ///
    /// Deterministic: the same frames always produce the same score, so a
    /// thesis citing "confluence: bullish (64%)" can be checked by rerunning
    /// the read.
    #[must_use]
    pub fn confluence(&self) -> Confluence {
        if self.frames.is_empty() {
            return Confluence::empty();
        }
        let mut factors = Vec::new();
        let mut bullish = 0.0;
        let mut bearish = 0.0;

        // Trend alignment, 25 points. Only frames that *report* a trend vote;
        // a Ranging frame is an absence of structure, not a third opinion.
        let reporting: Vec<&Trend> = self
            .frames
            .iter()
            .map(|f| &f.state.trend)
            .filter(|t| **t != Trend::Ranging)
            .collect();
        let bulls = reporting.iter().filter(|t| ***t == Trend::Bullish).count();
        let bears = reporting.iter().filter(|t| ***t == Trend::Bearish).count();
        let (direction, share, total) = lean(bulls, bears, reporting.len());
        let points = 25.0 * share;
        match direction {
            "bullish" => bullish += points,
            "bearish" => bearish += points,
            _ => {}
        }
        factors.push(ConfluenceFactor {
            name: "trend alignment".into(),
            direction: direction.into(),
            points,
            note: if reporting.is_empty() {
                "no frame reports a trend".into()
            } else {
                format!("{bulls} of {total} reporting frames bullish, {bears} bearish")
            },
        });

        // Value-area position, 20 points. Acceptance above value (or below)
        // per frame; frames sitting inside value abstain.
        let above = self
            .frames
            .iter()
            .filter(|f| f.state.price > f.state.vah)
            .count();
        let below = self
            .frames
            .iter()
            .filter(|f| f.state.price < f.state.val)
            .count();
        let (direction, share, _total) = lean(above, below, self.frames.len());
        let points = 20.0 * share;
        match direction {
            "bullish" => bullish += points,
            "bearish" => bearish += points,
            _ => {}
        }
        factors.push(ConfluenceFactor {
            name: "value-area lean".into(),
            direction: direction.into(),
            points,
            note: format!(
                "{above} of {} frames accepted above value, {below} below",
                self.frames.len()
            ),
        });

        // VWAP side, 15 points, only where VWAP exists.
        let with_vwap: Vec<bool> = self
            .frames
            .iter()
            .filter_map(|f| f.state.above_vwap())
            .collect();
        let above = with_vwap.iter().filter(|a| **a).count();
        let (direction, share, total) = lean(above, with_vwap.len() - above, with_vwap.len());
        let points = 15.0 * share;
        match direction {
            "bullish" => bullish += points,
            "bearish" => bearish += points,
            _ => {}
        }
        factors.push(ConfluenceFactor {
            name: "vwap side".into(),
            direction: direction.into(),
            points,
            note: format!("{above} of {total} frames with a VWAP are above it"),
        });

        // CVD sign, 15 points: whether aggression agrees with the story.
        let pos = self.frames.iter().filter(|f| f.state.cvd > 0.0).count();
        let neg = self.frames.iter().filter(|f| f.state.cvd < 0.0).count();
        let (direction, share, _total) = lean(pos, neg, self.frames.len());
        let points = 15.0 * share;
        match direction {
            "bullish" => bullish += points,
            "bearish" => bearish += points,
            _ => {}
        }
        factors.push(ConfluenceFactor {
            name: "cvd sign".into(),
            direction: direction.into(),
            points,
            note: format!(
                "{pos} of {} frames carry positive CVD, {neg} negative",
                self.frames.len()
            ),
        });

        // Divergence, 10 points: a warning factor, weighted to be a tiebreak
        // rather than a verdict.
        let bull_div = self
            .frames
            .iter()
            .filter(|f| f.state.divergence == analytics_core::CvdDivergence::Bullish)
            .count();
        let bear_div = self
            .frames
            .iter()
            .filter(|f| f.state.divergence == analytics_core::CvdDivergence::Bearish)
            .count();
        let (direction, share, total) = lean(bull_div, bear_div, self.frames.len());
        let points = 10.0 * share;
        match direction {
            "bullish" => bullish += points,
            "bearish" => bearish += points,
            _ => {}
        }
        factors.push(ConfluenceFactor {
            name: "cvd divergence".into(),
            direction: direction.into(),
            points,
            note: format!(
                "{bull_div} bullish divergences, {bear_div} bearish across {total} frames"
            ),
        });

        // Decision-timeframe delta, 5 points: the trigger frame's own aggression.
        if let Some(lowest) = self.lowest() {
            let direction = if lowest.state.delta > 0.0 {
                "bullish"
            } else if lowest.state.delta < 0.0 {
                "bearish"
            } else {
                "neutral"
            };
            let points = if direction == "neutral" { 0.0 } else { 5.0 };
            match direction {
                "bullish" => bullish += points,
                "bearish" => bearish += points,
                _ => {}
            }
            factors.push(ConfluenceFactor {
                name: "decision-timeframe delta".into(),
                direction: direction.into(),
                points,
                note: format!("{} delta {:+.2}", lowest.timeframe, lowest.state.delta),
            });
        }

        // Liquidity proximity, 10 points: which unswept pool the price would
        // reach first -- stop runs travel toward the nearer pool.
        if let Some(lowest) = self.lowest() {
            let above = lowest
                .state
                .nearest_liquidity_above()
                .filter(|l| !l.swept)
                .map(|l| l.price);
            let below = lowest
                .state
                .nearest_liquidity_below()
                .filter(|l| !l.swept)
                .map(|l| l.price);
            let (direction, points, note) = match (above, below) {
                (Some(above), Some(below)) if above - below > f64::EPSILON => {
                    let nearer_above = above - lowest.state.price;
                    let nearer_below = lowest.state.price - below;
                    if nearer_above < nearer_below {
                        (
                            "bearish",
                            10.0,
                            format!(
                                "unswept highs at {above:.4} are nearer than lows at {below:.4}"
                            ),
                        )
                    } else if nearer_below < nearer_above {
                        (
                            "bullish",
                            10.0,
                            format!(
                                "unswept lows at {below:.4} are nearer than highs at {above:.4}"
                            ),
                        )
                    } else {
                        (
                            "neutral",
                            0.0,
                            format!("pools at {above:.4} and {below:.4} are equally near"),
                        )
                    }
                }
                (Some(above), _) => (
                    "bearish",
                    10.0,
                    format!("only unswept pool is the highs at {above:.4}"),
                ),
                (_, Some(below)) => (
                    "bullish",
                    10.0,
                    format!("only unswept pool is the lows at {below:.4}"),
                ),
                (None, None) => ("neutral", 0.0, "no unswept pools detected".into()),
            };
            match direction {
                "bullish" => bullish += points,
                "bearish" => bearish += points,
                _ => {}
            }
            factors.push(ConfluenceFactor {
                name: "liquidity proximity".into(),
                direction: direction.into(),
                points,
                note,
            });
        }

        let (bias, agreement_pct) = if bullish > bearish * (1.0 + Confluence::MARGIN) {
            ("bullish", bullish / (bullish + bearish) * 100.0)
        } else if bearish > bullish * (1.0 + Confluence::MARGIN) {
            ("bearish", bearish / (bullish + bearish) * 100.0)
        } else {
            // No side cleared the margin: mixed, and the agreement reported is
            // the larger share anyway, so a 46/38 split does not read as 50/50.
            let total = bullish + bearish;
            let share = if total > 0.0 {
                bullish.max(bearish) / total * 100.0
            } else {
                0.0
            };
            ("mixed", share)
        };
        Confluence {
            bullish,
            bearish,
            bias: bias.into(),
            agreement_pct,
            factors,
        }
    }

    /// The finest frame -- where an entry trigger would come from.
    #[must_use]
    pub fn lowest(&self) -> Option<&FrameView> {
        self.frames.last()
    }

    /// Whether every frame that reports a trend reports the same one.
    #[must_use]
    pub fn trends_aligned(&self) -> bool {
        let trends: Vec<String> = self
            .frames
            .iter()
            .map(|frame| format!("{:?}", frame.state.trend))
            .collect();
        match trends.split_first() {
            Some((first, rest)) => rest.iter().all(|trend| trend == first),
            None => false,
        }
    }

    /// A deterministic, compact summary of the ladder.
    ///
    /// Every line is computed from the states themselves. The model is given
    /// this *in addition to* the raw states, so that the facts it is most
    /// likely to get wrong if computed mentally are already correct.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut out = format!("Ladder digest for {}:\n", self.symbol);

        for frame in &self.frames {
            let state = &frame.state;
            let vwap = state
                .vwap
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "n/a".into());
            out.push_str(&format!(
                "  {:<3} price {:<12.4} trend {:<8} delta {:>10.2} cvd {:>12.2} \
                 poc {:<12.4} vwap {:<12} value_area {}\n",
                frame.timeframe.to_string(),
                state.price,
                format!("{:?}", state.trend),
                state.delta,
                state.cvd,
                state.poc,
                vwap,
                if state.in_value_area() {
                    "inside"
                } else {
                    "outside"
                },
            ));
        }

        if let Some(highest) = self.highest() {
            out.push_str(&format!(
                "\n  highest timeframe ({}): trend {:?}, price vs POC {}\n",
                highest.timeframe,
                highest.state.trend,
                match highest.state.poc_deviation() {
                    Some(deviation) if deviation > 0.0 => "above".to_string(),
                    Some(_) => "below".to_string(),
                    None => "unknown".to_string(),
                }
            ));
        }
        out.push_str(&format!(
            "  trend alignment across the ladder: {}\n",
            if self.trends_aligned() {
                "aligned"
            } else {
                "mixed"
            }
        ));

        // The confluence block is the digest's verdict line: computed in Rust
        // (see `confluence`), stated here so the model cites it rather than
        // recomputes it. Every factor's note is arithmetic already done.
        let confluence = self.confluence();
        out.push_str(&format!(
            "\n  confluence: {} ({:.0}% agreement, {:.0} bull vs {:.0} bear points)\n",
            confluence.bias, confluence.agreement_pct, confluence.bullish, confluence.bearish
        ));
        for factor in &confluence.factors {
            out.push_str(&format!(
                "    - {} [{}]: {:.0} pts -- {}\n",
                factor.name, factor.direction, factor.points, factor.note
            ));
        }
        out
    }
}

/// Read every timeframe in a ladder.
///
/// A timeframe with no data is skipped rather than failing the whole ladder:
/// the higher timeframes are still useful context, and one gap should not turn
/// "here is the 4h and 1h picture" into an error.
///
/// # Errors
/// [`AgentError::NoData`] when *no* timeframe has data -- there is genuinely
/// nothing to reason about.
pub async fn analyze_ladder(
    data: &dyn MarketDataSource,
    symbol: &str,
    ladder: &TimeframeLadder,
    config: &MarketStateConfig,
    lookback: usize,
) -> Result<LadderView, AgentError> {
    let lookback = lookback.max(2);
    let mut frames = Vec::new();

    for timeframe in ladder.timeframes() {
        let latest = match data.latest_candle_time(symbol, *timeframe).await? {
            Some(time) => time,
            None => continue,
        };
        let width = timeframe.nanos();
        let bars = i64::try_from(lookback.saturating_sub(1)).unwrap_or(i64::MAX);
        let candles = data
            .candles(symbol, *timeframe, latest - bars * width, latest + width)
            .await?;
        if candles.is_empty() {
            continue;
        }
        let trades = data
            .trades(symbol, latest - bars * width, latest + width)
            .await
            .unwrap_or_default();

        if let Some(state) = build_market_state(&candles, &trades, config) {
            frames.push(FrameView {
                timeframe: *timeframe,
                state,
            });
        }
    }

    if frames.is_empty() {
        return Err(AgentError::NoData {
            symbol: symbol.to_string(),
            timeframe: "ladder".to_string(),
        });
    }

    Ok(LadderView {
        symbol: symbol.to_string(),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use analytics_core::types::{Candle, Trade};
    use async_trait::async_trait;

    /// Serves candles on every timeframe by counting backwards from a fixed
    /// "now", with a gap on one timeframe to exercise the skip path.
    struct Ladder {
        candles_per_timeframe: usize,
        missing: Option<Timeframe>,
        now: i64,
    }

    #[async_trait]
    impl MarketDataSource for Ladder {
        async fn candles(
            &self,
            symbol: &str,
            timeframe: Timeframe,
            from_ns: i64,
            to_ns: i64,
        ) -> Result<Vec<Candle>, AgentError> {
            if self.missing == Some(timeframe) {
                return Ok(Vec::new());
            }
            let mut candles = Vec::new();
            let width = timeframe.nanos();
            let mut t = self.now - i64::try_from(self.candles_per_timeframe).unwrap() * width;
            let mut i = 0_usize;
            while t < to_ns {
                let price = 100.0 + (i % 7) as f64;
                if t >= from_ns {
                    candles.push(Candle {
                        symbol: symbol.into(),
                        timeframe,
                        open_time: t,
                        open: price,
                        high: price + 1.0,
                        low: price - 1.0,
                        close: price + 0.5,
                        volume: 10.0,
                        buy_volume: 6.0,
                        sell_volume: 4.0,
                    });
                }
                t += width;
                i += 1;
            }
            Ok(candles)
        }

        async fn trades(
            &self,
            _symbol: &str,
            _from_ns: i64,
            _to_ns: i64,
        ) -> Result<Vec<Trade>, AgentError> {
            Ok(Vec::new())
        }

        async fn latest_candle_time(
            &self,
            _symbol: &str,
            timeframe: Timeframe,
        ) -> Result<Option<i64>, AgentError> {
            Ok(Some(timeframe.bucket_of(self.now)))
        }
    }

    fn ladder_source(missing: Option<Timeframe>) -> Ladder {
        Ladder {
            candles_per_timeframe: 400,
            missing,
            now: 1_700_000_000_000_000_000,
        }
    }

    #[test]
    fn the_default_ladder_is_coarse_to_fine() {
        let ladder = TimeframeLadder::default();
        assert_eq!(
            ladder.timeframes(),
            &[Timeframe::D1, Timeframe::H4, Timeframe::H1, Timeframe::M5]
        );
    }

    #[test]
    fn a_ladder_is_sorted_and_deduplicated_whatever_order_it_arrives_in() {
        let ladder = TimeframeLadder::new(vec![
            Timeframe::M5,
            Timeframe::D1,
            Timeframe::M5,
            Timeframe::H1,
        ]);
        assert_eq!(
            ladder.timeframes(),
            &[Timeframe::D1, Timeframe::H1, Timeframe::M5]
        );
        assert_eq!(ladder.len(), 3);
    }

    #[test]
    fn unknown_timeframe_strings_are_dropped_not_errored() {
        let ladder = TimeframeLadder::from_strs(&["4h".into(), "7m".into(), "1h".into()]);
        assert_eq!(ladder.timeframes(), &[Timeframe::H4, Timeframe::H1]);
    }

    #[test]
    fn a_skill_without_a_ladder_falls_back_to_its_preferred_timeframes() {
        let skill = Skill {
            preferred_timeframes: vec!["1h".into(), "5m".into()],
            ..Skill::default()
        };
        assert_eq!(
            TimeframeLadder::from_skill(&skill).timeframes(),
            &[Timeframe::H1, Timeframe::M5]
        );
    }

    #[tokio::test]
    async fn reading_a_ladder_returns_one_frame_per_timeframe() {
        let source = ladder_source(None);
        let view = analyze_ladder(
            &source,
            "BTCUSDT",
            &TimeframeLadder::default(),
            &MarketStateConfig::default(),
            200,
        )
        .await
        .unwrap();

        assert_eq!(view.symbol, "BTCUSDT");
        assert_eq!(view.frames.len(), 4);
        let tfs: Vec<Timeframe> = view.frames.iter().map(|f| f.timeframe).collect();
        assert_eq!(
            tfs,
            vec![Timeframe::D1, Timeframe::H4, Timeframe::H1, Timeframe::M5]
        );
    }

    #[tokio::test]
    async fn one_missing_timeframe_does_not_sink_the_ladder() {
        let source = ladder_source(Some(Timeframe::H1));
        let view = analyze_ladder(
            &source,
            "BTCUSDT",
            &TimeframeLadder::default(),
            &MarketStateConfig::default(),
            200,
        )
        .await
        .unwrap();

        assert_eq!(view.frames.len(), 3, "only the three that have data");
        assert!(!view.frames.iter().any(|f| f.timeframe == Timeframe::H1));
    }

    #[tokio::test]
    async fn an_entirely_empty_source_is_an_error() {
        let source = ladder_source(Some(Timeframe::D1));
        let ladder = TimeframeLadder::new(vec![Timeframe::D1]);
        let err = analyze_ladder(
            &source,
            "BTCUSDT",
            &ladder,
            &MarketStateConfig::default(),
            200,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::NoData { .. }), "got {err}");
    }

    #[tokio::test]
    async fn the_digest_states_facts_computed_in_rust() {
        let source = ladder_source(None);
        let view = analyze_ladder(
            &source,
            "BTCUSDT",
            &TimeframeLadder::default(),
            &MarketStateConfig::default(),
            200,
        )
        .await
        .unwrap();

        let digest = view.digest();
        assert!(digest.contains("Ladder digest for BTCUSDT"));
        for timeframe in ["1d", "4h", "1h", "5m"] {
            assert!(digest.contains(timeframe), "digest missing {timeframe}");
        }
        assert!(digest.contains("trend alignment across the ladder"));
        assert!(digest.contains("highest timeframe (1d)"));
    }

    #[tokio::test]
    async fn alignment_reflects_the_underlying_states() {
        let source = ladder_source(None);
        let view = analyze_ladder(
            &source,
            "BTCUSDT",
            &TimeframeLadder::default(),
            &MarketStateConfig::default(),
            200,
        )
        .await
        .unwrap();

        let distinct = {
            let mut trends: Vec<String> = view
                .frames
                .iter()
                .map(|f| format!("{:?}", f.state.trend))
                .collect();
            trends.dedup();
            trends.len()
        };
        assert_eq!(view.trends_aligned(), distinct == 1);
        assert!(view.highest().is_some());
        assert!(view.lowest().is_some());
    }

    // -----------------------------------------------------------------------
    // The confluence read. Built from hand-made `MarketState`s rather than
    // the fixture ladder: the factors must be tested against *known* readings,
    // and the fixture's zig-zag candles would make every expected number here
    // another copy of the arithmetic under test.
    // -----------------------------------------------------------------------

    use analytics_core::cvd::CvdDivergence;

    fn frame(timeframe: Timeframe, trend: Trend) -> FrameView {
        frame_with(timeframe, trend, 0.0, 0.0)
    }

    /// A frame centred in its value area at `price`, with VWAP pinned to the
    /// price (so `above_vwap` is a decision the test makes per-frame by
    /// passing a `vwap` offset), POC at the price, and no liquidity.
    fn frame_with(timeframe: Timeframe, trend: Trend, cvd: f64, delta: f64) -> FrameView {
        frame_full(timeframe, trend, cvd, delta, 0.0, None, Vec::new())
    }

    fn frame_full(
        timeframe: Timeframe,
        trend: Trend,
        cvd: f64,
        delta: f64,
        vwap_offset: f64,
        divergence: Option<CvdDivergence>,
        liquidity: Vec<analytics_core::LiquidityLevel>,
    ) -> FrameView {
        let price = 100.0;
        FrameView {
            timeframe,
            state: MarketState {
                symbol: "BTCUSDT".into(),
                timeframe: timeframe.to_string(),
                timestamp: 0,
                price,
                delta,
                cvd,
                vwap: if vwap_offset == 0.0 {
                    None
                } else {
                    Some(price + vwap_offset)
                },
                poc: price,
                vah: price + 5.0,
                val: price - 5.0,
                volume: 1.0,
                buy_volume: 0.5,
                sell_volume: 0.5,
                divergence: divergence.unwrap_or(CvdDivergence::None),
                trend,
                imbalances: Vec::new(),
                absorption: Vec::new(),
                liquidity,
                swing_highs: Vec::new(),
                swing_lows: Vec::new(),
                breaks: Vec::new(),
                bars_since_break: None,
            },
        }
    }

    #[test]
    fn a_unanimously_bullish_ladder_scores_bullish_with_the_arithmetic_shown() {
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: vec![
                frame_with(Timeframe::D1, Trend::Bullish, 1.0, 1.0),
                frame_with(Timeframe::H4, Trend::Bullish, 1.0, 1.0),
                frame_with(Timeframe::H1, Trend::Bullish, 1.0, 1.0),
            ],
        };
        let c = view.confluence();
        assert_eq!(c.bias, "bullish");
        // Trend 25 (3/3) + CVD 15 (3/3 positive) + decision delta 5 = 45.
        // Value-area abstains (the fixture sits centred *inside* value, which
        // is a non-vote, not a bullish one), VWAP abstains (no VWAP in the
        // fixture), divergence and liquidity abstain (nothing detected) --
        // absence is not a vote, which is the point of the factor design.
        assert_eq!(c.bullish, 45.0);
        assert_eq!(c.bearish, 0.0);
        assert_eq!(c.agreement_pct, 100.0);
    }

    #[test]
    fn a_split_ladder_without_a_margin_is_mixed_not_a_faint_side() {
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: vec![
                frame(Timeframe::D1, Trend::Bullish),
                frame(Timeframe::H4, Trend::Bearish),
            ],
        };
        let c = view.confluence();
        assert_eq!(c.bias, "mixed");
        // 2 reporting frames split 1-1: the trend factor abstains entirely
        // (ties lean neutral), and the two frames' CVD/VA/delta votes cancel.
        assert_eq!(c.bullish, c.bearish);
    }

    #[test]
    fn a_ranging_frame_abstains_rather_than_voting_neutral() {
        // 2 bullish frames + 1 ranging: the trend factor is 25 * (2/2) = 25,
        // not 25 * (2/3) -- a frame with no structure said nothing, and saying
        // so twice would halve the evidence the two frames actually gave.
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: vec![
                frame(Timeframe::D1, Trend::Bullish),
                frame(Timeframe::H4, Trend::Bullish),
                frame(Timeframe::H1, Trend::Ranging),
            ],
        };
        let c = view.confluence();
        let trend = c
            .factors
            .iter()
            .find(|f| f.name == "trend alignment")
            .expect("the trend factor is always present");
        assert_eq!(trend.points, 25.0);
        assert!(trend.note.contains("2 of 2"));
    }

    #[test]
    fn divergence_and_liquidity_are_tiebreaks_with_signed_evidence() {
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: vec![frame_full(
                Timeframe::H1,
                Trend::Ranging,
                0.0,
                // The trigger frame's own aggression is bearish, which is what
                // lets the warning factors outvote the bullish pool magnet.
                -1.0,
                0.0,
                Some(CvdDivergence::Bearish),
                vec![analytics_core::LiquidityLevel {
                    price: 98.0,
                    kind: analytics_core::LiquidityKind::EqualLows,
                    touches: 2,
                    swept: false,
                    formed_at: 0,
                    last_index: 0,
                }],
            )],
        };
        let c = view.confluence();
        assert_eq!(c.bias, "bearish");
        // A ladder that reports *no* structure can still lean: divergence 10
        // + trigger delta 5 = 15 bearish, against 10 bullish from the pool
        // below. Signed factors are allowed to disagree -- that is what makes
        // them evidence rather than a verdict -- and the margin rule decides.
        assert_eq!(c.bearish, 15.0);
        assert_eq!(c.bullish, 10.0);
        let proximity = c
            .factors
            .iter()
            .find(|f| f.name == "liquidity proximity")
            .expect("the liquidity factor is present when pools exist");
        assert_eq!(proximity.direction, "bullish");
        assert!(proximity.note.contains("98.0000"), "{}", proximity.note);
    }

    #[test]
    fn an_empty_ladder_claims_nothing() {
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: Vec::new(),
        };
        let c = view.confluence();
        assert_eq!(c.bias, "mixed");
        assert_eq!(c.bullish + c.bearish, 0.0);
    }

    #[test]
    fn the_digest_states_the_confluence_verdict_rather_than_the_arithmetic() {
        let view = LadderView {
            symbol: "BTCUSDT".into(),
            frames: vec![frame(Timeframe::H1, Trend::Bullish)],
        };
        let digest = view.digest();
        assert!(digest.contains("confluence: bullish"), "{digest}");
        assert!(digest.contains("trend alignment [bullish]"), "{digest}");
    }
}
