//! The gate a strategy must pass before it may trade real money
//! (`docs/15`).
//!
//! ## Three conditions, and none of them is negotiable
//!
//! `docs/15` says a strategy may not go live until it has a paper track
//! record, the user has opted in **per venue**, and risk limits are configured
//! and active. All three are checked here, together, so a new requirement
//! cannot be added to one caller and forgotten by another.
//!
//! ## Why the verdict lists every reason at once
//!
//! A gate that returns the first failure makes the operator fix them one at a
//! time, which turns a two-minute correction into four deploys. Every reason is
//! collected.
//!
//! ## Opt-in is per venue, and revocable
//!
//! Opting in to Binance is not opting in to every venue that will ever exist,
//! and revoking has to be possible without a redeploy -- an operator who has
//! lost confidence in a venue at 03:00 should not need a release to stop it.

use std::collections::HashSet;

use crate::risk::RiskLimits;

/// What a strategy has done in simulation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackRecord {
    /// How many trades it closed in paper mode.
    pub closed_trades: usize,
    /// How long it has been running, in hours.
    pub hours: f64,
    /// Cumulative result in R.
    pub cumulative_r: f64,
}

/// The thresholds a track record is measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateRequirements {
    /// Minimum closed paper trades.
    pub min_paper_trades: usize,
    /// Minimum hours of paper trading.
    pub min_paper_hours: f64,
    /// How bad the paper result may be before going live is refused.
    ///
    /// A strategy that lost 20R in simulation is not "unproven", it is
    /// *disproven*, and the gate should say so rather than let it buy its way
    /// past a trade count.
    pub max_paper_loss_r: f64,
}

impl Default for GateRequirements {
    fn default() -> Self {
        Self {
            min_paper_trades: 20,
            min_paper_hours: 48.0,
            max_paper_loss_r: 10.0,
        }
    }
}

/// The answer to "may this strategy trade here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// Yes.
    Allowed,
    /// No, for these reasons.
    Refused {
        /// Every reason, not just the first.
        reasons: Vec<String>,
    },
}

impl GateVerdict {
    /// Whether it may trade.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// The gate: requirements plus the set of venues the user has opted in to.
#[derive(Debug, Clone, Default)]
pub struct LiveGate {
    requirements: GateRequirements,
    venues: HashSet<String>,
}

impl LiveGate {
    /// A gate with no venue opted in.
    #[must_use]
    pub fn new(requirements: GateRequirements) -> Self {
        Self {
            requirements,
            venues: HashSet::new(),
        }
    }

    /// The requirements in force.
    #[must_use]
    pub const fn requirements(&self) -> GateRequirements {
        self.requirements
    }

    /// Record an explicit opt-in for a venue.
    pub fn opt_in(&mut self, venue: &str) {
        self.venues.insert(venue.to_ascii_lowercase());
    }

    /// Withdraw it. Takes effect on the next check, which is the next time a
    /// bot would place an order.
    pub fn revoke(&mut self, venue: &str) {
        self.venues.remove(&venue.to_ascii_lowercase());
    }

    /// Whether a venue is opted in.
    #[must_use]
    pub fn is_opted_in(&self, venue: &str) -> bool {
        self.venues.contains(&venue.to_ascii_lowercase())
    }

    /// Every opted-in venue, sorted so logs and responses are stable.
    #[must_use]
    pub fn venues(&self) -> Vec<String> {
        let mut venues: Vec<String> = self.venues.iter().cloned().collect();
        venues.sort();
        venues
    }

    /// Check every condition at once.
    #[must_use]
    pub fn check(&self, venue: &str, record: TrackRecord, limits: &RiskLimits) -> GateVerdict {
        let mut reasons = Vec::new();

        if !self.is_opted_in(venue) {
            reasons.push(format!(
                "the account has not opted in to live trading on {venue}"
            ));
        }

        if record.closed_trades < self.requirements.min_paper_trades {
            reasons.push(format!(
                "{} of {} required paper trades",
                record.closed_trades, self.requirements.min_paper_trades
            ));
        }

        if record.hours < self.requirements.min_paper_hours {
            reasons.push(format!(
                "{:.1}h of {:.1}h required paper trading",
                record.hours, self.requirements.min_paper_hours
            ));
        }

        if record.cumulative_r < -self.requirements.max_paper_loss_r {
            reasons.push(format!(
                "paper result {:.2}R is worse than the {:.2}R floor",
                record.cumulative_r, self.requirements.max_paper_loss_r
            ));
        }

        if !limits.max_risk_pct.is_finite() || limits.max_risk_pct <= 0.0 {
            reasons.push("no usable per-trade risk limit is configured".to_string());
        }

        if reasons.is_empty() {
            GateVerdict::Allowed
        } else {
            GateVerdict::Refused { reasons }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: f64 = 24.0;

    fn record() -> TrackRecord {
        TrackRecord {
            closed_trades: 30,
            hours: 72.0,
            cumulative_r: 4.0,
        }
    }

    #[test]
    fn everything_satisfied_is_allowed() {
        let mut gate = LiveGate::new(GateRequirements::default());
        gate.opt_in("binance");
        assert!(
            gate.check("binance", record(), &RiskLimits::default())
                .is_allowed()
        );
    }

    #[test]
    fn no_venue_opt_in_is_refused_even_with_a_perfect_record() {
        let gate = LiveGate::new(GateRequirements::default());
        let verdict = gate.check("binance", record(), &RiskLimits::default());
        match verdict {
            GateVerdict::Refused { reasons } => {
                assert!(reasons[0].contains("opted in"), "{reasons:?}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn opting_in_is_per_venue() {
        let mut gate = LiveGate::new(GateRequirements::default());
        gate.opt_in("binance");
        assert!(gate.is_opted_in("binance"));
        assert!(
            gate.is_opted_in("BINANCE"),
            "venue names are case-insensitive"
        );
        assert!(!gate.is_opted_in("kraken"), "one venue is not all venues");
    }

    #[test]
    fn revoking_takes_effect_immediately() {
        let mut gate = LiveGate::new(GateRequirements::default());
        gate.opt_in("binance");
        gate.revoke("binance");
        assert!(!gate.is_opted_in("binance"));
        assert!(gate.venues().is_empty());
    }

    #[test]
    fn every_reason_is_reported_not_just_the_first() {
        // An operator should not need four deploys to satisfy four conditions.
        let gate = LiveGate::new(GateRequirements::default());
        let verdict = gate.check(
            "binance",
            TrackRecord::default(),
            &RiskLimits {
                max_risk_pct: 0.0,
                ..RiskLimits::default()
            },
        );
        match verdict {
            GateVerdict::Refused { reasons } => {
                assert!(
                    reasons.len() >= 4,
                    "opt-in, trades, hours and limits all failed: {reasons:?}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_disproven_strategy_is_refused_however_long_it_ran() {
        let mut gate = LiveGate::new(GateRequirements::default());
        gate.opt_in("binance");
        let verdict = gate.check(
            "binance",
            TrackRecord {
                closed_trades: 500,
                hours: 30.0 * DAY,
                cumulative_r: -40.0,
            },
            &RiskLimits::default(),
        );
        match verdict {
            GateVerdict::Refused { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("worse")), "{reasons:?}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_strategy_under_the_trade_count_is_refused() {
        let mut gate = LiveGate::new(GateRequirements::default());
        gate.opt_in("binance");
        let verdict = gate.check(
            "binance",
            TrackRecord {
                closed_trades: 19,
                ..record()
            },
            &RiskLimits::default(),
        );
        match verdict {
            GateVerdict::Refused { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("19 of 20")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_default_requirements_are_the_docs_fifteen_minimums() {
        let requirements = GateRequirements::default();
        assert_eq!(requirements.min_paper_trades, 20);
        assert_eq!(requirements.min_paper_hours, 48.0);
    }
}
