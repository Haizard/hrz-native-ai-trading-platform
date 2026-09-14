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

impl LadderView {
    /// The coarsest frame -- macro context.
    #[must_use]
    pub fn highest(&self) -> Option<&FrameView> {
        self.frames.first()
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
}
