//! Synchronized, timestamp-correct multi-timeframe market observations.
//!
//! This is the shared market representation required by top-down Core Analytics:
//! every mathematical discipline receives the *same* hierarchy of completed
//! candles, but builds its own features and conclusions from it.
//!
//! Two rules are enforced here rather than left to callers:
//!
//! 1. **No look-ahead.** A candle that closes after the decision instant is a
//!    forming candle and is never placed in [`TimeframeObservation::candles`].
//!    A candle that *opens* after the decision instant is a future observation
//!    and is rejected outright.
//! 2. **Honest data quality.** Gaps, duplicates and out-of-order input are
//!    measured rather than silently smoothed over, and a frame with too few
//!    completed bars is marked as such.
//!
//! The hierarchy is data-driven: the caller supplies the timeframes it actually
//! has. Nothing here assumes a particular ladder, and no wall-clock is read,
//! so the module remains deterministic and `wasm32` compatible.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::AnalyticsError;
use crate::resample::resample_all;
use crate::types::{Candle, Timeframe};

/// Whether a timeframe has enough completed data to support analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameAvailability {
    /// At least the required number of completed candles is present.
    Available,
    /// Some completed candles exist, but fewer than the caller requires.
    Insufficient,
    /// No completed candle yet; only a forming candle exists.
    FormingOnly,
    /// No observations at all for this timeframe.
    Missing,
}

impl FrameAvailability {
    /// Whether this frame can be used as completed evidence.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Measured quality of one timeframe's observations at a decision instant.
///
/// Counts are exact observations about the supplied data, not scores about the
/// market. [`Self::score`] is a deterministic bounded summary for ranking data,
/// never a probability and never a trading signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataQuality {
    /// Number of completed candles in the frame.
    pub completed_bars: usize,
    /// Expected bucket positions with no observation between two bars.
    pub missing_buckets: usize,
    /// Extra observations sharing an already-seen open time.
    pub duplicate_buckets: usize,
    /// Observations whose open time was earlier than the previous one.
    pub out_of_order_bars: usize,
    /// Nanoseconds between the last completed candle's close and the decision.
    ///
    /// `None` when the frame has no completed candle.
    pub staleness_ns: Option<i64>,
    /// Broker/venue provenance supplied by the caller.
    pub source: String,
}

impl DataQuality {
    /// True when no gap, duplicate or ordering defect was observed.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.missing_buckets == 0 && self.duplicate_buckets == 0 && self.out_of_order_bars == 0
    }

    /// A deterministic quality score in `[0, 1]`, where `1` is defect-free.
    ///
    /// An empty frame scores zero. The score is for comparing data feeds and
    /// deciding whether to trust an analysis; it is not calibrated market
    /// evidence and must never be reported as a probability.
    #[must_use]
    pub fn score(&self) -> f64 {
        if self.completed_bars == 0 {
            return 0.0;
        }
        let penalty = self.missing_buckets as f64 * 0.25
            + self.duplicate_buckets as f64 * 0.5
            + self.out_of_order_bars as f64 * 0.1;
        (1.0 / (1.0 + penalty)).clamp(0.0, 1.0)
    }
}

/// One timeframe's observations at a single decision instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeframeObservation {
    /// Resolution of this frame.
    pub timeframe: Timeframe,
    /// Completed candles only: every candle here closed at or before the
    /// decision instant of the containing [`MultiTimeframeObservation`].
    pub candles: Vec<Candle>,
    /// Open time of the newest completed candle, if any.
    pub last_completed_candle: Option<i64>,
    /// The currently forming candle, kept separate so no caller can mistake it
    /// for a completed bar. Its close may lie after the decision instant.
    pub forming_candle: Option<Candle>,
    /// Measured gaps, duplicates and staleness.
    pub data_quality: DataQuality,
    /// Whether this frame has enough completed bars to analyse.
    pub availability: FrameAvailability,
}

impl TimeframeObservation {
    /// Build one frame from an arbitrary candle slice at a decision instant.
    ///
    /// Candles are split into completed and forming by their own timeframe's
    /// duration. A candle that opens after `decision_timestamp` is a future
    /// observation and is rejected. `min_completed` sets the threshold for
    /// [`FrameAvailability::Available`]; it is a data-sufficiency threshold,
    /// not a model parameter.
    pub fn from_candles(
        timeframe: Timeframe,
        symbol: &str,
        candles: &[Candle],
        decision_timestamp: i64,
        source: &str,
        min_completed: usize,
    ) -> Result<Self, AnalyticsError> {
        let width = timeframe.nanos();
        let mut completed = Vec::with_capacity(candles.len());
        let mut forming = None;
        let mut seen = BTreeSet::new();
        let mut duplicates = 0usize;
        let mut out_of_order = 0usize;
        let mut missing = 0usize;
        let mut previous: Option<i64> = None;

        for candle in candles {
            if candle.timeframe != timeframe {
                return Err(AnalyticsError::MismatchedFrameTimeframe {
                    expected: timeframe,
                    found: candle.timeframe,
                });
            }
            if candle.symbol != symbol {
                return Err(AnalyticsError::MismatchedFrameInstrument {
                    expected: symbol.to_string(),
                    found: candle.symbol.clone(),
                });
            }
            if candle.open_time > decision_timestamp {
                return Err(AnalyticsError::LookAhead {
                    timeframe,
                    open_time: candle.open_time,
                    decision_time: decision_timestamp,
                });
            }
            if !seen.insert(candle.open_time) {
                duplicates += 1;
            }
            if let Some(prev) = previous {
                if candle.open_time < prev {
                    out_of_order += 1;
                } else if candle.open_time > prev {
                    let width = width.max(1);
                    missing = missing.saturating_add(
                        ((candle.open_time - prev) / width).saturating_sub(1) as usize,
                    );
                }
            }
            previous = Some(candle.open_time.max(previous.unwrap_or(i64::MIN)));

            // A candle closes exactly one timeframe after it opens. Equal is
            // completed: its final close was already known at the decision.
            if candle
                .open_time
                .checked_add(width)
                .map_or(false, |close| close <= decision_timestamp)
            {
                completed.push(candle.clone());
            } else {
                forming = Some(candle.clone());
            }
        }

        let last_completed_candle = completed.iter().map(|c| c.open_time).max();
        let staleness_ns = last_completed_candle.and_then(|open| {
            open.checked_add(width)
                .and_then(|close| decision_timestamp.checked_sub(close))
                .filter(|age| *age >= 0)
        });
        let completed_bars = completed.len();
        let availability = if completed_bars >= min_completed {
            FrameAvailability::Available
        } else if completed_bars > 0 {
            FrameAvailability::Insufficient
        } else if forming.is_some() {
            FrameAvailability::FormingOnly
        } else {
            FrameAvailability::Missing
        };

        Ok(Self {
            timeframe,
            candles: completed,
            last_completed_candle,
            forming_candle: forming,
            data_quality: DataQuality {
                completed_bars,
                missing_buckets: missing,
                duplicate_buckets: duplicates,
                out_of_order_bars: out_of_order,
                staleness_ns,
                source: source.to_string(),
            },
            availability,
        })
    }

    /// Completed candles as a slice. This is the only view a discipline should
    /// analyse; the forming candle is deliberately kept out of it.
    #[must_use]
    pub fn completed(&self) -> &[Candle] {
        &self.candles
    }
}

/// A synchronized hierarchy of frames at one decision instant.
///
/// `frames` is ordered from the coarsest timeframe to the finest, matching the
/// top-down analytical direction used elsewhere in QUANTERA. Callers must not
/// construct an observation whose frames contain future candles; use
/// [`Self::from_series`] or [`Self::from_fine`] to enforce that automatically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiTimeframeObservation {
    /// Instrument identifier, e.g. `BTCUSDT`.
    pub instrument_id: String,
    /// Unix nanoseconds UTC of the decision instant.
    pub decision_timestamp: i64,
    /// Frames ordered coarsest first.
    pub frames: Vec<TimeframeObservation>,
}

impl MultiTimeframeObservation {
    /// Assemble and validate an observation from already-resampled series.
    pub fn from_series(
        instrument_id: impl Into<String>,
        decision_timestamp: i64,
        series: Vec<(Timeframe, Vec<Candle>)>,
        source: &str,
        min_completed: usize,
    ) -> Result<Self, AnalyticsError> {
        let instrument_id = instrument_id.into();
        let mut frames = Vec::with_capacity(series.len());
        for (timeframe, candles) in series {
            frames.push(TimeframeObservation::from_candles(
                timeframe,
                &instrument_id,
                &candles,
                decision_timestamp,
                source,
                min_completed,
            )?);
        }
        Self::new(instrument_id, decision_timestamp, frames)
    }

    /// Resample one fine series onto `ladder` and build a validated observation.
    ///
    /// This is the convenient path for a backtest replay or a live window that
    /// holds one base resolution. No new bucket math is introduced here:
    /// [`resample_all`] does the aggregation, including the buy/sell split.
    pub fn from_fine(
        instrument_id: impl Into<String>,
        decision_timestamp: i64,
        candles: &[Candle],
        ladder: &[Timeframe],
        source: &str,
        min_completed: usize,
    ) -> Result<Self, AnalyticsError> {
        Self::from_series(
            instrument_id,
            decision_timestamp,
            resample_all(candles, ladder),
            source,
            min_completed,
        )
    }

    /// Build an observation from frames, sorting them coarsest first and
    /// rejecting duplicate timeframes or inconsistent instruments.
    pub fn new(
        instrument_id: impl Into<String>,
        decision_timestamp: i64,
        mut frames: Vec<TimeframeObservation>,
    ) -> Result<Self, AnalyticsError> {
        let instrument_id = instrument_id.into();
        if frames.is_empty() {
            return Err(AnalyticsError::EmptyInput);
        }
        frames.sort_by(|a, b| b.timeframe.cmp(&a.timeframe));
        let mut seen = BTreeSet::new();
        for frame in &frames {
            if !seen.insert(frame.timeframe) {
                return Err(AnalyticsError::DuplicateTimeframe(frame.timeframe));
            }
            for candle in frame.candles.iter().chain(frame.forming_candle.iter()) {
                if candle.symbol != instrument_id {
                    return Err(AnalyticsError::MismatchedFrameInstrument {
                        expected: instrument_id.clone(),
                        found: candle.symbol.clone(),
                    });
                }
            }
        }
        Ok(Self {
            instrument_id,
            decision_timestamp,
            frames,
        })
    }

    /// The coarsest frame, if any.
    #[must_use]
    pub fn highest(&self) -> Option<&TimeframeObservation> {
        self.frames.first()
    }

    /// The finest frame, if any.
    #[must_use]
    pub fn lowest(&self) -> Option<&TimeframeObservation> {
        self.frames.last()
    }

    /// Find a frame by timeframe.
    #[must_use]
    pub fn frame(&self, timeframe: Timeframe) -> Option<&TimeframeObservation> {
        self.frames.iter().find(|f| f.timeframe == timeframe)
    }

    /// Frames strictly coarser than `timeframe`, in top-down order.
    pub fn higher_than(&self, timeframe: Timeframe) -> impl Iterator<Item = &TimeframeObservation> {
        self.frames
            .iter()
            .take_while(move |f| f.timeframe > timeframe)
    }

    /// Frames strictly finer than `timeframe`, in top-down order.
    pub fn lower_than(&self, timeframe: Timeframe) -> impl Iterator<Item = &TimeframeObservation> {
        self.frames
            .iter()
            .skip_while(move |f| f.timeframe >= timeframe)
    }

    /// True when every frame has at least one completed bar and no defect.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.frames
            .iter()
            .all(|frame| frame.availability.is_usable() && frame.data_quality.is_clean())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000_000_000;

    fn candle(symbol: &str, timeframe: Timeframe, open_time: i64, close: f64) -> Candle {
        Candle {
            symbol: symbol.to_string(),
            timeframe,
            open_time,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
            buy_volume: 7.0,
            sell_volume: 3.0,
        }
    }

    fn series(count: usize, timeframe: Timeframe) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                candle(
                    "BTCUSDT",
                    timeframe,
                    i as i64 * timeframe.nanos(),
                    100.0 + i as f64,
                )
            })
            .collect()
    }

    #[test]
    fn forming_candle_is_never_a_completed_bar() {
        let candles = series(3, Timeframe::M5);
        // Decision at 2.5 bars: the third candle opened but has not closed.
        let decision = 2 * Timeframe::M5.nanos() + MIN * 2;
        let frame = TimeframeObservation::from_candles(
            Timeframe::M5,
            "BTCUSDT",
            &candles,
            decision,
            "test",
            1,
        )
        .unwrap();

        assert_eq!(frame.candles.len(), 2);
        assert_eq!(
            frame.forming_candle.as_ref().unwrap().open_time,
            2 * Timeframe::M5.nanos()
        );
        assert_eq!(frame.last_completed_candle, Some(Timeframe::M5.nanos()));
        assert_eq!(frame.availability, FrameAvailability::Available);
    }

    #[test]
    fn a_future_candle_is_rejected() {
        let candles = series(3, Timeframe::M5);
        let decision = Timeframe::M5.nanos();
        let error = TimeframeObservation::from_candles(
            Timeframe::M5,
            "BTCUSDT",
            &candles,
            decision,
            "test",
            1,
        )
        .unwrap_err();
        assert_eq!(
            error,
            AnalyticsError::LookAhead {
                timeframe: Timeframe::M5,
                open_time: 2 * Timeframe::M5.nanos(),
                decision_time: decision,
            }
        );
    }

    #[test]
    fn gaps_duplicates_and_ordering_are_measured() {
        let mut candles = vec![
            candle("BTCUSDT", Timeframe::M5, 0, 1.0),
            candle("BTCUSDT", Timeframe::M5, 3 * Timeframe::M5.nanos(), 2.0),
            candle("BTCUSDT", Timeframe::M5, Timeframe::M5.nanos(), 3.0),
            candle("BTCUSDT", Timeframe::M5, 3 * Timeframe::M5.nanos(), 4.0),
        ];
        let decision = 10 * Timeframe::M5.nanos();
        let quality = TimeframeObservation::from_candles(
            Timeframe::M5,
            "BTCUSDT",
            &candles,
            decision,
            "test",
            1,
        )
        .unwrap()
        .data_quality;

        assert_eq!(quality.missing_buckets, 2); // buckets 1 and 2 absent
        assert_eq!(quality.duplicate_buckets, 1); // bucket 3 twice
        assert_eq!(quality.out_of_order_bars, 1);
        assert!(!quality.is_clean());
        assert!(quality.score() < 1.0);

        candles.clear();
    }

    #[test]
    fn frames_are_sorted_coarse_to_fine_and_unique() {
        let fine = series(5, Timeframe::M1);
        let observation = MultiTimeframeObservation::from_fine(
            "BTCUSDT",
            10 * Timeframe::M1.nanos(),
            &fine,
            &[Timeframe::M5, Timeframe::M1, Timeframe::H1],
            "test",
            1,
        )
        .unwrap();

        let order: Vec<Timeframe> = observation.frames.iter().map(|f| f.timeframe).collect();
        assert_eq!(order, vec![Timeframe::H1, Timeframe::M5, Timeframe::M1]);
        assert_eq!(observation.highest().unwrap().timeframe, Timeframe::H1);
        assert_eq!(observation.lowest().unwrap().timeframe, Timeframe::M1);

        let higher: Vec<Timeframe> = observation
            .higher_than(Timeframe::M5)
            .map(|f| f.timeframe)
            .collect();
        let lower: Vec<Timeframe> = observation
            .lower_than(Timeframe::M5)
            .map(|f| f.timeframe)
            .collect();
        assert_eq!(higher, vec![Timeframe::H1]);
        assert_eq!(lower, vec![Timeframe::M1]);
    }

    #[test]
    fn duplicate_timeframes_are_rejected() {
        let frame = TimeframeObservation::from_candles(
            Timeframe::M1,
            "BTCUSDT",
            &series(3, Timeframe::M1),
            3 * MIN,
            "test",
            1,
        )
        .unwrap();
        let error = MultiTimeframeObservation::new("BTCUSDT", 3 * MIN, vec![frame.clone(), frame])
            .unwrap_err();
        assert_eq!(error, AnalyticsError::DuplicateTimeframe(Timeframe::M1));
    }

    #[test]
    fn empty_observation_is_rejected() {
        assert_eq!(
            MultiTimeframeObservation::new("BTCUSDT", MIN, Vec::new()).unwrap_err(),
            AnalyticsError::EmptyInput
        );
    }

    #[test]
    fn resampled_frames_keep_the_buy_sell_split() {
        let fine = series(30, Timeframe::M1);
        let observation = MultiTimeframeObservation::from_fine(
            "BTCUSDT",
            30 * MIN,
            &fine,
            &[Timeframe::M1, Timeframe::M5],
            "test",
            1,
        )
        .unwrap();

        let fine_delta: f64 = observation
            .frame(Timeframe::M1)
            .unwrap()
            .completed()
            .iter()
            .map(Candle::delta)
            .sum();
        let coarse_delta: f64 = observation
            .frame(Timeframe::M5)
            .unwrap()
            .completed()
            .iter()
            .map(Candle::delta)
            .sum();
        assert_eq!(fine_delta, coarse_delta);
        assert!(observation.is_clean());
    }

    #[test]
    fn insufficient_and_missing_frames_are_reported_honestly() {
        let decision = 10 * MIN;
        let few = TimeframeObservation::from_candles(
            Timeframe::M1,
            "BTCUSDT",
            &series(2, Timeframe::M1),
            decision,
            "test",
            20,
        )
        .unwrap();
        assert_eq!(few.availability, FrameAvailability::Insufficient);

        let empty =
            TimeframeObservation::from_candles(Timeframe::M1, "BTCUSDT", &[], decision, "test", 1)
                .unwrap();
        assert_eq!(empty.availability, FrameAvailability::Missing);
        assert_eq!(empty.data_quality.score(), 0.0);

        let only_forming = TimeframeObservation::from_candles(
            Timeframe::M1,
            "BTCUSDT",
            &[candle("BTCUSDT", Timeframe::M1, decision, 1.0)],
            decision,
            "test",
            1,
        )
        .unwrap();
        assert_eq!(only_forming.availability, FrameAvailability::FormingOnly);
        assert!(only_forming.completed().is_empty());
    }

    #[test]
    fn quality_score_is_bounded() {
        let mut quality = DataQuality {
            completed_bars: 10,
            missing_buckets: 100,
            duplicate_buckets: 100,
            out_of_order_bars: 100,
            staleness_ns: None,
            source: "test".to_string(),
        };
        assert!(quality.score() >= 0.0 && quality.score() <= 1.0);
        quality.missing_buckets = 0;
        quality.duplicate_buckets = 0;
        quality.out_of_order_bars = 0;
        assert_eq!(quality.score(), 1.0);
    }
}
