//! Risk limits and the kill-switch (`docs/15-RISK-COMPLIANCE.md`).
//!
//! ## Always on, and independent of everything else
//!
//! `docs/15` requires the limits to hold "even when the agent or market-data
//! engine is degraded". That is why this module is a plain state machine with
//! no I/O, no async, and no dependency on any other crate: it cannot fail
//! because a socket dropped or a database is slow. It is the last thing
//! standing between a bad strategy and the account.
//!
//! ## A clamped limit is reported, not silently applied
//!
//! A document asking to risk 20% per trade is not executed at 20% and not
//! rejected outright -- it is clamped to the platform ceiling, and
//! [`RiskLimits::clamped`] says so. Rejecting would mean a strategy that
//! cannot run at all and no way to see why; executing at the requested size
//! would defeat the point of having a ceiling.
//!
//! ## Windows are UTC, by index
//!
//! Daily and weekly windows are derived by dividing the timestamp, not by
//! tracking a "last reset" timestamp that has to be ticked. A bot that
//! receives no candles for three days therefore rolls its window correctly on
//! the next one, rather than attributing a week-old loss to today.
//!
//! [`RiskLimits::clamped`]: RiskLimits::clamped

use serde::{Deserialize, Serialize};

use crate::error::ExecutionError;

/// The platform hard ceiling on per-trade risk, in percent of equity.
///
/// `docs/15` fixes this at 5%. A document may ask for less; it may not
/// effectively ask for more.
pub const PLATFORM_MAX_RISK_PCT: f64 = 5.0;

/// Nanoseconds in a UTC day.
const NANOS_PER_DAY: i64 = 86_400 * 1_000_000_000;

/// What to do with positions that are already open when a limit is breached.
///
/// `docs/11` insists this be a documented policy rather than a silent default,
/// so it is a field on the configuration and not a constant in a match arm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnBreach {
    /// Close every open position at market on the next candle.
    #[default]
    Close,
    /// Leave positions alone and alert. Correct when the operator wants to
    /// decide, wrong as a default for an unattended bot.
    Hold,
}

/// The configured limits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Per-trade risk as a percentage of equity. Clamped to
    /// [`PLATFORM_MAX_RISK_PCT`].
    pub max_risk_pct: f64,
    /// Loss in R tolerated per UTC day before the kill-switch trips.
    pub daily_loss_limit_r: f64,
    /// Loss in R tolerated per UTC week before the kill-switch trips.
    pub weekly_loss_limit_r: f64,
    /// How many positions may be open at once.
    pub max_concurrent_positions: usize,
    /// What happens to open positions on a breach.
    pub on_breach: OnBreach,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_risk_pct: 1.0,
            daily_loss_limit_r: 3.0,
            weekly_loss_limit_r: 8.0,
            max_concurrent_positions: 1,
            on_breach: OnBreach::Close,
        }
    }
}

impl RiskLimits {
    /// Clamp to the platform ceiling, reporting whether it was necessary.
    ///
    /// Returns the effective limits and a description of any clamp applied, so
    /// the caller can put it in the audit log rather than discovering later
    /// that trades were smaller than the document asked for.
    #[must_use]
    pub fn clamped(&self) -> (Self, Option<String>) {
        if self.max_risk_pct <= PLATFORM_MAX_RISK_PCT {
            return (*self, None);
        }
        let mut clamped = *self;
        clamped.max_risk_pct = PLATFORM_MAX_RISK_PCT;
        (
            clamped,
            Some(format!(
                "risk.max_risk_pct was {}% -- clamped to the {}% platform ceiling",
                self.max_risk_pct, PLATFORM_MAX_RISK_PCT
            )),
        )
    }
}

/// Why a trade was not taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskVerdict {
    /// Go ahead, at this capped fraction of equity.
    Allow,
    /// Do not trade.
    Deny {
        /// Which limit blocked it.
        limit: String,
        /// The value that breached it, as text, for the audit log.
        value: String,
    },
}

/// Rolling loss accounting and the switch itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskEngine {
    limits: RiskLimits,
    killed: bool,
    reason: Option<String>,
    day: i64,
    week: i64,
    loss_today_r: f64,
    loss_this_week_r: f64,
}

/// The UTC day index a timestamp falls in.
fn day_index(now: i64) -> i64 {
    now.div_euclid(NANOS_PER_DAY)
}

/// The UTC week index, aligned to Monday.
///
/// The epoch day 0 was a Thursday, so shifting by three makes week 0 begin on
/// the Monday before it. A week that straddles the epoch is irrelevant here;
/// what matters is that the boundary is stable and in UTC, so two bots agree
/// on when the window rolled.
fn week_index(now: i64) -> i64 {
    (day_index(now) + 3).div_euclid(7)
}

impl RiskEngine {
    /// Build an engine from limits, clamping the per-trade cap to the ceiling.
    ///
    /// The clamp is applied once, here, so no caller can forget it.
    pub fn new(limits: RiskLimits) -> Self {
        let (limits, _) = limits.clamped();
        Self {
            limits,
            killed: false,
            reason: None,
            day: 0,
            week: 0,
            loss_today_r: 0.0,
            loss_this_week_r: 0.0,
        }
    }

    /// The limits in force, after clamping.
    #[must_use]
    pub fn limits(&self) -> &RiskLimits {
        &self.limits
    }

    /// Whether the switch is engaged.
    #[must_use]
    pub fn is_killed(&self) -> bool {
        self.killed
    }

    /// Why the switch is engaged, if it is.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Loss booked so far today, in R.
    #[must_use]
    pub fn loss_today_r(&self) -> f64 {
        self.loss_today_r
    }

    /// Loss booked so far this week, in R.
    #[must_use]
    pub fn loss_this_week_r(&self) -> f64 {
        self.loss_this_week_r
    }

    /// Trip the switch by hand.
    ///
    /// `docs/15` asks for a manual switch as well as an automatic one. It is
    /// deliberately not reversible: clearing it means reloading the bot, which
    /// is the point -- an operator should have to decide to resume.
    pub fn kill(&mut self, reason: impl Into<String>) {
        if self.killed {
            return;
        }
        self.killed = true;
        self.reason = Some(reason.into());
    }

    /// Roll the day and week windows to `now`, clearing counters that no longer
    /// apply.
    ///
    /// Called at the top of every check, so a bot that receives no candles for
    /// a week does not carry last week's loss into today.
    fn roll(&mut self, now: i64) {
        let day = day_index(now);
        let week = week_index(now);
        if day != self.day {
            self.day = day;
            self.loss_today_r = 0.0;
        }
        if week != self.week {
            self.week = week;
            self.loss_this_week_r = 0.0;
        }
    }

    /// Whether one more entry may be taken at `now`.
    ///
    /// `open_positions` is the caller's count, not this engine's: the engine
    /// deliberately does not own positions, so that the same limits can be
    /// asked by a paper bot, a live bot, or a test without each having to
    /// model the others' state.
    ///
    /// `requested_risk_pct` is the document's own figure. It is checked rather
    /// than trusted, because a document is data and data can be edited.
    #[must_use]
    pub fn check_entry(
        &mut self,
        requested_risk_pct: f64,
        open_positions: usize,
        now: i64,
    ) -> RiskVerdict {
        self.roll(now);

        if self.killed {
            return RiskVerdict::Deny {
                limit: "kill_switch".into(),
                value: self.reason.clone().unwrap_or_else(|| "manual".into()),
            };
        }

        if !requested_risk_pct.is_finite() || requested_risk_pct <= 0.0 {
            return RiskVerdict::Deny {
                limit: "max_risk_pct".into(),
                value: format!("{requested_risk_pct} is not a usable risk fraction"),
            };
        }

        // A document that asks for more than the ceiling is a document that
        // was edited after review. Refuse rather than trade it.
        if requested_risk_pct > self.limits.max_risk_pct + f64::EPSILON {
            return RiskVerdict::Deny {
                limit: "max_risk_pct".into(),
                value: format!(
                    "{requested_risk_pct}% exceeds the {}% ceiling",
                    self.limits.max_risk_pct
                ),
            };
        }

        if open_positions >= self.limits.max_concurrent_positions {
            return RiskVerdict::Deny {
                limit: "max_concurrent_positions".into(),
                value: format!("{open_positions} already open"),
            };
        }

        if self.loss_today_r >= self.limits.daily_loss_limit_r {
            self.kill(format!(
                "daily loss limit reached: {:.2}R of {:.2}R",
                self.loss_today_r, self.limits.daily_loss_limit_r
            ));
            return RiskVerdict::Deny {
                limit: "daily_loss_limit_r".into(),
                value: format!("{:.2}R today", self.loss_today_r),
            };
        }

        if self.loss_this_week_r >= self.limits.weekly_loss_limit_r {
            self.kill(format!(
                "weekly loss limit reached: {:.2}R of {:.2}R",
                self.loss_this_week_r, self.limits.weekly_loss_limit_r
            ));
            return RiskVerdict::Deny {
                limit: "weekly_loss_limit_r".into(),
                value: format!("{:.2}R this week", self.loss_this_week_r),
            };
        }

        RiskVerdict::Allow
    }

    /// Book a closed trade's result and trip the switch if a limit is now
    /// breached.
    ///
    /// Returns the error the caller should surface when the switch just
    /// tripped, so a breach cannot be recorded without also being reported.
    pub fn record_close(&mut self, r_multiple: f64, now: i64) -> Result<(), ExecutionError> {
        self.roll(now);
        if r_multiple < 0.0 {
            self.loss_today_r += -r_multiple;
            self.loss_this_week_r += -r_multiple;
        }

        if self.loss_today_r >= self.limits.daily_loss_limit_r {
            let value = self.loss_today_r;
            self.kill(format!(
                "daily loss limit breached: {value:.2}R of {:.2}R",
                self.limits.daily_loss_limit_r
            ));
            return Err(ExecutionError::RiskLimitBreached {
                limit: "daily_loss_limit_r".into(),
                value: format!("{value:.2}R"),
            });
        }

        if self.loss_this_week_r >= self.limits.weekly_loss_limit_r {
            let value = self.loss_this_week_r;
            self.kill(format!(
                "weekly loss limit breached: {value:.2}R of {:.2}R",
                self.limits.weekly_loss_limit_r
            ));
            return Err(ExecutionError::RiskLimitBreached {
                limit: "weekly_loss_limit_r".into(),
                value: format!("{value:.2}R"),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = NANOS_PER_DAY;

    fn limits() -> RiskLimits {
        RiskLimits {
            max_risk_pct: 1.0,
            daily_loss_limit_r: 3.0,
            weekly_loss_limit_r: 8.0,
            max_concurrent_positions: 1,
            on_breach: OnBreach::Close,
        }
    }

    #[test]
    fn a_twenty_percent_strategy_is_clamped_to_the_platform_ceiling() {
        let limits = RiskLimits {
            max_risk_pct: 20.0,
            ..limits()
        };
        let (clamped, note) = limits.clamped();

        assert_eq!(clamped.max_risk_pct, PLATFORM_MAX_RISK_PCT);
        let note = note.expect("a clamp must be reported");
        assert!(note.contains("20"), "{note}");
        assert!(note.contains("5"), "{note}");
    }

    #[test]
    fn a_strategy_under_the_ceiling_is_left_alone() {
        let (clamped, note) = limits().clamped();
        assert_eq!(clamped, limits());
        assert!(note.is_none());
    }

    #[test]
    fn the_engine_never_holds_an_unclamped_ceiling() {
        // The clamp is applied in the constructor precisely so that no caller
        // can forget it.
        let engine = RiskEngine::new(RiskLimits {
            max_risk_pct: 20.0,
            ..limits()
        });
        assert_eq!(engine.limits().max_risk_pct, PLATFORM_MAX_RISK_PCT);
    }

    #[test]
    fn a_document_asking_for_more_than_the_ceiling_is_refused_not_traded() {
        let mut engine = RiskEngine::new(limits());
        match engine.check_entry(20.0, 0, DAY) {
            RiskVerdict::Deny { limit, value } => {
                assert_eq!(limit, "max_risk_pct");
                assert!(value.contains("20"), "{value}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_second_position_is_refused_at_the_concurrency_limit() {
        let mut engine = RiskEngine::new(limits());
        assert_eq!(engine.check_entry(1.0, 0, DAY), RiskVerdict::Allow);
        match engine.check_entry(1.0, 1, DAY) {
            RiskVerdict::Deny { limit, .. } => assert_eq!(limit, "max_concurrent_positions"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_daily_loss_breach_trips_the_kill_switch() {
        // The done criterion from docs/11: configure a tight limit and confirm
        // the switch actually fires.
        let mut engine = RiskEngine::new(RiskLimits {
            daily_loss_limit_r: 2.0,
            ..limits()
        });

        assert!(engine.record_close(-1.0, DAY).is_ok());
        assert!(!engine.is_killed(), "one R of a two R budget is fine");

        let err = engine
            .record_close(-1.0, DAY)
            .expect_err("the second loss must breach the limit");
        assert!(matches!(err, ExecutionError::RiskLimitBreached { .. }));

        assert!(engine.is_killed());
        assert!(engine.reason().unwrap().contains("daily"));

        // And the switch stays engaged for every later entry.
        match engine.check_entry(1.0, 0, DAY + 1) {
            RiskVerdict::Deny { limit, .. } => assert_eq!(limit, "kill_switch"),
            other => panic!("expected the switch to hold, got {other:?}"),
        }
    }

    #[test]
    fn a_weekly_breach_trips_even_when_no_single_day_does() {
        let mut engine = RiskEngine::new(RiskLimits {
            daily_loss_limit_r: 2.0,
            weekly_loss_limit_r: 4.0,
            ..limits()
        });

        // Three separate days, each under the daily limit, over the weekly one.
        // The first two days are inside the weekly budget; the third is not.
        for day in 0..2 {
            assert!(engine.record_close(-1.5, DAY * day).is_ok(), "day {day}");
        }
        let err = engine
            .record_close(-1.5, DAY * 2)
            .expect_err("the week must be over budget on day 2");
        assert!(matches!(err, ExecutionError::RiskLimitBreached { .. }));
        assert!(engine.is_killed(), "the week is over budget");
        assert!(engine.reason().unwrap().contains("weekly"));
    }

    #[test]
    fn a_new_day_clears_the_daily_counter_but_not_the_weekly_one() {
        let mut engine = RiskEngine::new(limits());
        assert!(engine.record_close(-2.0, DAY).is_ok());
        assert_eq!(engine.loss_today_r(), 2.0);

        assert!(engine.record_close(-1.0, DAY * 2).is_ok());
        assert_eq!(engine.loss_today_r(), 1.0, "the day rolled");
        assert_eq!(engine.loss_this_week_r(), 3.0, "the week did not");
    }

    #[test]
    fn a_gap_in_candles_does_not_carry_an_old_loss_into_today() {
        // A bot that hears nothing for a week must not attribute last week's
        // loss to today.
        let mut engine = RiskEngine::new(limits());
        assert!(engine.record_close(-2.5, DAY).is_ok());

        let much_later = DAY * 30;
        assert_eq!(
            engine.check_entry(1.0, 0, much_later),
            RiskVerdict::Allow,
            "the window rolled while the bot was silent"
        );
        assert_eq!(engine.loss_today_r(), 0.0);
    }

    #[test]
    fn the_next_day_is_a_new_window_rather_than_a_continuation() {
        let mut engine = RiskEngine::new(RiskLimits {
            daily_loss_limit_r: 2.0,
            ..limits()
        });
        assert!(engine.record_close(-1.9, DAY).is_ok());
        assert!(!engine.is_killed());

        // Same clock, next day: the budget resets, so this is allowed.
        assert_eq!(engine.check_entry(1.0, 0, DAY * 2), RiskVerdict::Allow);
    }

    #[test]
    fn a_manual_kill_is_reported_and_holds() {
        let mut engine = RiskEngine::new(limits());
        engine.kill("operator pressed stop");

        assert!(engine.is_killed());
        assert_eq!(engine.reason(), Some("operator pressed stop"));

        engine.kill("a second reason");
        assert_eq!(
            engine.reason(),
            Some("operator pressed stop"),
            "the first reason is the one that stopped it"
        );
    }

    #[test]
    fn a_manual_kill_needs_no_market_data_to_work() {
        // docs/15: the switch must work while everything else is degraded.
        // This engine has no I/O at all, so there is nothing to degrade --
        // which is the point, and worth pinning with a test that constructs
        // and kills it without touching a clock, a socket, or a database.
        let mut engine = RiskEngine::new(limits());
        engine.kill("degraded");
        match engine.check_entry(1.0, 0, 0) {
            RiskVerdict::Deny { limit, .. } => assert_eq!(limit, "kill_switch"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_winning_trade_does_not_count_against_the_loss_budget() {
        let mut engine = RiskEngine::new(limits());
        assert!(engine.record_close(2.5, DAY).is_ok());
        assert_eq!(engine.loss_today_r(), 0.0);
        assert!(!engine.is_killed());
    }
}
