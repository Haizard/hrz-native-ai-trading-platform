//! Trading **sessions** and killzones -- time-of-day windows, not candles.
//!
//! ## Why this module exists
//!
//! The condition language is a closed vocabulary of scalars: every field reads
//! one candle or one aggregate. A "session" is neither -- it is a property of
//! the *clock* a candle opened under, and no window of candles can recover it.
//! Asked for "only trade the London killzone", a model would reach for the
//! nearest scalar and produce a proxy. This module is the primitive the
//! vocabulary was missing, in the same spirit as `concepts.rs`: the measurement
//! lives here once, and every future client prompt can reference it.
//!
//! ## What a session is here
//!
//! A named window of minutes-past-UTC-midnight, repeating every UTC day.
//! `Asia`, `London` and `NewYork` are the defaults futures traders actually
//! gate entries on (the "killzones"), and a `Custom` window covers anything
//! else -- a prop firm's payout cut-off, a news embargo, a local open. Windows
//! may cross midnight; a window that does is matched on both sides of the
//! boundary rather than silently splitting in two.
//!
//! Crypto trades 24/7, so *outside every window* is not an error -- it is
//! `None`, and a condition comparing `session == "london"` is simply false
//! there. That is the same absent-means-false rule the rest of the vocabulary
//! follows, which is what keeps a session gate composable rather than special.
//!
//! ## What accumulates inside one
//!
//! A [`SessionEngine`] carries the same aggregates `state.rs` carries across
//! the whole window -- VWAP, delta, the open -- but resets at every session
//! boundary. The point is comparability: today's London VWAP next to
//! yesterday's, instead of one eternal accumulator that never means the same
//! thing twice. The shape mirrors [`crate::cvd::Cvd`], down to the stateful
//! `update` returning the running value.

use serde::{Deserialize, Serialize};

use crate::types::{Candle, NS_PER_SEC};

/// Nanoseconds in one day -- the period every session window repeats over.
pub const NS_PER_DAY: i64 = 86_400 * NS_PER_SEC;

/// Nanoseconds in one minute.
const NS_PER_MIN: i64 = 60 * NS_PER_SEC;

/// A named time-of-day window, repeating every UTC day.
///
/// `start` and `end` are minutes past UTC midnight; `end` is exclusive and may
/// be smaller than `start`, which is how a window that crosses midnight is
/// written (`22:00 -> 02:00`). `end == start` would be a zero-length window,
/// which [`SessionWindow::validate`] refuses: a window that contains no time
/// is not a window, it is a bug waiting to be traded on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWindow {
    /// Which session this window opens.
    pub kind: SessionKind,
    /// Minutes past UTC midnight the window opens at, inclusive.
    pub start: i64,
    /// Minutes past UTC midnight the window closes at, exclusive. May be less
    /// than `start` -- that is a window crossing midnight.
    pub end: i64,
}

impl SessionWindow {
    /// The default **London killzone**: 07:00-10:00 UTC.
    #[must_use]
    pub const fn london() -> Self {
        Self {
            kind: SessionKind::London,
            start: 7 * 60,
            end: 10 * 60,
        }
    }

    /// The default **New York killzone**: 12:00-15:00 UTC.
    #[must_use]
    pub const fn new_york() -> Self {
        Self {
            kind: SessionKind::NewYork,
            start: 12 * 60,
            end: 15 * 60,
        }
    }

    /// The default **Asia session**: 23:00-02:00 UTC, crossing midnight.
    ///
    /// Tokyo's morning is the deepest liquidity Asia offers, and it straddles
    /// the UTC day boundary -- which is exactly the case a start/end pair that
    /// cannot wrap would get wrong.
    #[must_use]
    pub const fn asia() -> Self {
        Self {
            kind: SessionKind::Asia,
            start: 23 * 60,
            end: 2 * 60,
        }
    }

    /// The three windows a session gate usually means.
    #[must_use]
    pub const fn defaults() -> [Self; 3] {
        [Self::asia(), Self::london(), Self::new_york()]
    }

    /// Whether the window is well-formed: it covers some minutes of the day.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.start != self.end
            && self.start >= 0
            && self.start < MINUTES_PER_DAY
            && self.end >= 0
            && self.end < MINUTES_PER_DAY
    }

    /// Whether `minute` (minutes past UTC midnight) falls inside the window.
    ///
    /// A window that wraps midnight matches on both sides: 23:00 -> 02:00
    /// contains 23:30 *and* 01:30. Written as two range checks rather than one
    /// modular one, because the modular form (`(minute - start).rem_euclid(day)
    /// < length`) is correct but reads as clever, and the whole value of this
    /// type is that a reader can see it is right.
    #[must_use]
    pub const fn contains(self, minute: i64) -> bool {
        if self.start < self.end {
            minute >= self.start && minute < self.end
        } else {
            // Wraps midnight: the window is the union of [start, day) and
            // [0, end).
            minute >= self.start || minute < self.end
        }
    }
}

/// Minutes in a UTC day, the domain the windows live in.
pub const MINUTES_PER_DAY: i64 = NS_PER_DAY / NS_PER_MIN;

/// The session a candle belongs to.
///
/// A closed set rather than a string because these are the names conditions
/// are written against (`session == "london"`), and an open set would let a
/// typo through validation and fail silently at evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// The Asia session.
    Asia,
    /// The London session.
    London,
    /// The New York session.
    NewYork,
    /// A client-defined window with no preset name.
    Custom,
}

impl SessionKind {
    /// Every variant, for exhaustive tests.
    pub const ALL: [Self; 4] = [Self::Asia, Self::London, Self::NewYork, Self::Custom];

    /// Canonical name, as a condition writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Asia => "asia",
            Self::London => "london",
            Self::NewYork => "new_york",
            Self::Custom => "custom",
        }
    }
}

/// Which session `ts` falls in, earliest window wins.
///
/// Overlapping windows are a configuration mistake this function does not
/// hide: the first window in the list to contain the timestamp wins, and the
/// order the caller wrote is the tie-break a reader can predict. `None` when
/// no window matches -- crypto trades 24/7, so off-session is a fact, not an
/// error.
#[must_use]
pub fn session_of(ts: i64, windows: &[SessionWindow]) -> Option<SessionKind> {
    let minute = minute_of_day(ts);
    windows
        .iter()
        .find(|window| window.is_valid() && window.contains(minute))
        .map(|window| window.kind)
}

/// Minutes past UTC midnight a timestamp falls in.
#[must_use]
pub const fn minute_of_day(ts: i64) -> i64 {
    ts.rem_euclid(NS_PER_DAY) / NS_PER_MIN
}

/// The per-session aggregates one series accumulates.
///
/// Stateful, like [`crate::cvd::Cvd`]: feed candles in order and read the
/// running values. Everything resets when the candle's session differs from
/// the previous one -- including stepping from `Some(asia)` to `Some(london)`
/// -- so the values always describe *this* session so far.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEngine {
    windows: Vec<SessionWindow>,
    current: Option<SessionKind>,
    cumulative_pv: f64,
    cumulative_volume: f64,
    open: Option<f64>,
    delta: f64,
}

impl SessionEngine {
    /// An engine matching `windows`, earliest-wins.
    #[must_use]
    pub fn new(windows: Vec<SessionWindow>) -> Self {
        Self {
            windows,
            current: None,
            cumulative_pv: 0.0,
            cumulative_volume: 0.0,
            open: None,
            delta: 0.0,
        }
    }

    /// Feed a candle; returns the session it belongs to, if any.
    pub fn update(&mut self, candle: &Candle) -> Option<SessionKind> {
        let session = session_of(candle.open_time, &self.windows);

        match session {
            // Same session as before: keep accumulating.
            Some(kind) if self.current == Some(kind) => {}
            // A new session -- or off-session after a session: everything
            // restarts. The first candle of a session sets its open, which is
            // the level intraday levels are measured from.
            _ => {
                self.current = session;
                self.cumulative_pv = 0.0;
                self.cumulative_volume = 0.0;
                self.open = None;
                self.delta = 0.0;
            }
        }

        if let Some(_kind) = session {
            let typical = (candle.high + candle.low + candle.close) / 3.0;
            self.cumulative_pv += typical * candle.volume;
            self.cumulative_volume += candle.volume;
            if self.open.is_none() {
                self.open = Some(candle.open);
            }
            self.delta += candle.delta();
        }

        session
    }

    /// The session the newest candle belongs to, if any.
    #[must_use]
    pub const fn session(&self) -> Option<SessionKind> {
        self.current
    }

    /// The session's VWAP so far; `None` off-session or with no volume yet.
    #[must_use]
    pub const fn vwap(&self) -> Option<f64> {
        match self.current {
            Some(_) if self.cumulative_volume > 0.0 => {
                Some(self.cumulative_pv / self.cumulative_volume)
            }
            _ => None,
        }
    }

    /// The session's first open; `None` off-session.
    #[must_use]
    pub const fn open(&self) -> Option<f64> {
        self.open
    }

    /// The session's cumulative delta.
    #[must_use]
    pub const fn delta(&self) -> f64 {
        self.delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeframe;

    fn candle(minute_of_day: i64, delta: f64) -> Candle {
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M1,
            open_time: minute_of_day * NS_PER_MIN,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0 + delta,
            volume: 10.0,
            buy_volume: 5.0 + delta / 2.0,
            sell_volume: 5.0 - delta / 2.0,
        }
    }

    #[test]
    fn a_window_contains_its_own_minutes_and_not_the_boundary() {
        let london = SessionWindow::london();
        assert!(london.contains(7 * 60), "the open minute is inside");
        assert!(london.contains(9 * 60 + 59));
        assert!(!london.contains(10 * 60), "the end is exclusive");
        assert!(!london.contains(6 * 60 + 59));
    }

    #[test]
    fn a_window_that_wraps_midnight_matches_both_sides() {
        let asia = SessionWindow::asia(); // 23:00 -> 02:00
        assert!(asia.contains(23 * 60), "late evening is inside");
        assert!(asia.contains(1 * 60 + 30), "just after midnight is inside");
        assert!(!asia.contains(3 * 60));
        assert!(!asia.contains(12 * 60));
    }

    #[test]
    fn a_zero_length_window_is_refused() {
        let degenerate = SessionWindow {
            kind: SessionKind::Custom,
            start: 600,
            end: 600,
        };
        assert!(!degenerate.is_valid());
        // And it matches nothing, so a bad config cannot hijack evaluation.
        assert_eq!(
            session_of(600 * NS_PER_MIN, &[degenerate]),
            None,
            "an invalid window matches nothing"
        );
    }

    #[test]
    fn the_default_killzones_are_the_ones_traders_mean() {
        let [asia, london, new_york] = SessionWindow::defaults();
        assert_eq!(asia.kind.name(), "asia");
        assert_eq!(london.kind.name(), "london");
        assert_eq!(new_york.kind.name(), "new_york");
        // The Asia window is the one that wraps midnight, so it is the one
        // whose validity would catch a broken `contains`.
        assert!(asia.is_valid() && london.is_valid() && new_york.is_valid());
    }

    #[test]
    fn off_session_is_none_not_an_error() {
        let windows = SessionWindow::defaults();
        // 15:00 -- after New York's close, inside nothing.
        assert_eq!(session_of(15 * 60 * NS_PER_MIN, &windows), None);
        // 06:59 -- one minute before London.
        assert_eq!(session_of(6 * 60 * NS_PER_MIN + 59 * NS_PER_SEC, &windows), None);
    }

    #[test]
    fn overlapping_windows_resolve_in_the_order_they_were_written() {
        let windows = vec![
            SessionWindow::london(),
            SessionWindow {
                kind: SessionKind::Custom,
                start: 8 * 60,
                end: 11 * 60,
            },
        ];
        // 08:30 is inside both; the first listed wins, deterministically.
        assert_eq!(session_of(8 * 60 * NS_PER_MIN + 30 * NS_PER_MIN, &windows), Some(SessionKind::London));
    }

    #[test]
    fn the_engine_accumulates_within_a_session_and_resets_across_one() {
        let mut engine = SessionEngine::new(SessionWindow::defaults().to_vec());

        // 07:05 London: +2 delta, VWAP seeded.
        assert_eq!(engine.update(&candle(7 * 60 + 5, 2.0)), Some(SessionKind::London));
        // 07:06: -1 delta. Cumulative delta is +1.
        assert_eq!(engine.update(&candle(7 * 60 + 6, -1.0)), Some(SessionKind::London));
        assert_eq!(engine.session(), Some(SessionKind::London));
        assert_eq!(engine.delta(), 1.0);
        assert_eq!(engine.open(), Some(100.0), "the session open is the first candle's");
        // VWAP is the volume-weighted mean of the *typical* prices
        // ((high+low+close)/3), and the two candles traded equal volume:
        // (302/3 + 299/3) / 2.
        let vwap = engine.vwap().expect("volume has traded");
        let expected = (302.0 / 3.0 + 299.0 / 3.0) / 2.0;
        assert!((vwap - expected).abs() < 1e-9, "{vwap} vs {expected}");

        // 12:00 sharp -- New York opens, everything restarts.
        assert_eq!(engine.update(&candle(12 * 60, 4.0)), Some(SessionKind::NewYork));
        assert_eq!(engine.delta(), 4.0, "the accumulator reset at the boundary");
        assert_eq!(engine.open(), Some(100.0));
        // One candle's VWAP is that candle's typical price.
        let vwap = engine.vwap().expect("volume has traded");
        assert!((vwap - (101.0 + 99.0 + 104.0) / 3.0).abs() < 1e-9, "{vwap}");
    }

    #[test]
    fn stepping_off_session_clears_the_aggregates() {
        let mut engine = SessionEngine::new(SessionWindow::defaults().to_vec());
        engine.update(&candle(8 * 60, 3.0));
        // 16:00 is off-session.
        assert_eq!(engine.update(&candle(16 * 60, 0.0)), None);
        assert_eq!(engine.session(), None);
        assert_eq!(engine.vwap(), None, "off-session there is no session VWAP");
        assert_eq!(engine.open(), None);
        assert_eq!(engine.delta(), 0.0);
    }

    #[test]
    fn a_candle_one_minute_before_a_session_is_not_its_first_candle() {
        // The boundary is the clock, not the neighbourhood: 06:59 must not
        // seed London's open, or the "session open" level lies by a minute.
        let mut engine = SessionEngine::new(vec![SessionWindow::london()]);
        assert_eq!(engine.update(&candle(6 * 60 + 59, 0.0)), None);
        assert_eq!(engine.update(&candle(7 * 60, 0.0)), Some(SessionKind::London));
        assert_eq!(engine.open(), Some(100.0));
    }

    #[test]
    fn minute_of_day_floors_across_days() {
        assert_eq!(minute_of_day(0), 0);
        // 07:30:00.5 -- half a second past the half hour floors to 450.
        let seven_thirty = (7 * 60 + 30) * NS_PER_MIN + 30 * NS_PER_SEC;
        assert_eq!(minute_of_day(seven_thirty), 450);
        // Same clock time the next day reads the same minute -- the property
        // the daily window match depends on.
        assert_eq!(minute_of_day(seven_thirty + NS_PER_DAY), 450);
    }
}
