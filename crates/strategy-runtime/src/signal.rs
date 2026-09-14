//! What a strategy decides on a candle (`docs/06-STRATEGY-DSL.md`).
//!
//! `on_candle` returns `Option<Signal>`, and a [`Signal`] is either an entry or
//! an exit. They are separate types rather than one struct with a `kind` field
//! and a handful of `Option`s, because an exit has no stop, no target and no
//! risk percentage -- modelling that as "optional fields you must remember not
//! to read" is how a simulator ends up sizing a position off a `None`.
//!
//! ## Stops are resolved *before* the entry, not after
//!
//! [`EnterSignal`] carries a **resolved stop price**, not the [`StopSpec`] rule
//! it came from. That is deliberate and it matters:
//!
//! * The rule is resolved against the decision candle's close -- the last price
//!   the strategy could actually see. The stop you decide on is the stop you
//!   get; it does not silently re-derive itself from a fill price that the
//!   strategy never observed.
//! * The resolved price is what the simulator watches, what the trade log
//!   records, and what the R multiple is measured against. One number, decided
//!   once, inspectable after the fact.
//!
//! [`StopSpec`]: strategy_dsl::StopSpec

use serde::{Deserialize, Serialize};
use strategy_dsl::Direction;

/// A strategy's decision for one candle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Signal {
    /// Open a position.
    Enter(EnterSignal),
    /// Close the open position because a condition fired.
    Exit(ExitSignal),
}

impl Signal {
    /// The action, without borrowing the payload.
    #[must_use]
    pub const fn action(&self) -> SignalAction {
        match self {
            Self::Enter(_) => SignalAction::Enter,
            Self::Exit(_) => SignalAction::Exit,
        }
    }

    /// The entry payload, if this is an entry.
    #[must_use]
    pub const fn as_enter(&self) -> Option<&EnterSignal> {
        match self {
            Self::Enter(signal) => Some(signal),
            Self::Exit(_) => None,
        }
    }

    /// The exit payload, if this is an exit.
    #[must_use]
    pub const fn as_exit(&self) -> Option<&ExitSignal> {
        match self {
            Self::Exit(signal) => Some(signal),
            Self::Enter(_) => None,
        }
    }
}

/// Which of the two a [`Signal`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalAction {
    /// Open a position.
    Enter,
    /// Close the open position.
    Exit,
}

/// An instruction to open a position, with its protection already worked out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnterSignal {
    /// Which way to trade.
    pub direction: Direction,
    /// The decision candle's close.
    ///
    /// The price the stop and target were resolved against, and the reference
    /// the simulator measures the eventual fill's slippage from. Named
    /// "reference" rather than "entry" because the fill happens on the *next*
    /// candle's open and will differ.
    pub reference_price: f64,
    /// The resolved stop price.
    pub stop_price: f64,
    /// The resolved take-profit price, when the document declares one.
    pub take_profit_price: Option<f64>,
    /// Percentage of equity to risk, from the document's risk block.
    pub max_risk_pct: f64,
    /// Labels of the conditions that fired, in document order.
    ///
    /// This is the explainability contract (principle #8): a trade log entry
    /// says *which* rules fired, not merely that "the strategy" fired.
    pub reasons: Vec<String>,
}

impl EnterSignal {
    /// Distance from the reference price to the stop, always positive.
    ///
    /// This is the R unit: `1R` is the risk the trade accepted at the moment it
    /// was decided.
    #[must_use]
    pub fn risk_per_unit(&self) -> f64 {
        (self.reference_price - self.stop_price).abs()
    }

    /// Whether the stop sits on the correct side of the reference price.
    ///
    /// A long whose stop is above its entry is not a trade with a bad risk
    /// profile, it is a malformed trade. The engine refuses to emit one; the
    /// simulator re-checks before opening.
    #[must_use]
    pub fn stop_is_valid(&self) -> bool {
        match self.direction {
            Direction::Long => self.stop_price < self.reference_price,
            Direction::Short => self.stop_price > self.reference_price,
        }
    }

    /// Whether the target (if any) sits on the correct side.
    #[must_use]
    pub fn target_is_valid(&self) -> bool {
        match (self.direction, self.take_profit_price) {
            (_, None) => true,
            (Direction::Long, Some(t)) => t > self.reference_price,
            (Direction::Short, Some(t)) => t < self.reference_price,
        }
    }
}

/// An instruction to close the open position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitSignal {
    /// What caused the exit.
    pub trigger: ExitTrigger,
    /// Labels of the conditions that fired, in document order. Empty for
    /// [`ExitTrigger::Stop`] and [`ExitTrigger::Target`], which are price
    /// events rather than condition events.
    pub reasons: Vec<String>,
}

impl ExitSignal {
    /// An exit caused by a condition block.
    #[must_use]
    pub fn from_conditions(trigger: ExitTrigger, reasons: Vec<String>) -> Self {
        Self { trigger, reasons }
    }
}

/// Why a position was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitTrigger {
    /// Price touched the stop.
    Stop,
    /// Price touched the take-profit level.
    Target,
    /// An `invalidation` condition fired -- the setup's premise is gone.
    Invalidation,
    /// An `exit` condition fired.
    ExitCondition,
    /// The data ran out with the position still open. Closed at the last close
    /// so the trade is accounted for rather than dropped, which would flatter
    /// the statistics.
    EndOfData,
    /// The risk engine's kill-switch closed it (`docs/15`). Distinct from
    /// every other exit because it was not the strategy's decision and not a
    /// price level: it has to be visible as its own thing in the audit trail,
    /// or a liquidation at a bad moment looks like a normal exit.
    KillSwitch,
}

impl ExitTrigger {
    /// Canonical name, for the trade log.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Target => "target",
            Self::Invalidation => "invalidation",
            Self::ExitCondition => "exit_condition",
            Self::EndOfData => "end_of_data",
            Self::KillSwitch => "kill_switch",
        }
    }

    /// Whether this exit was decided by a price level rather than a condition.
    #[must_use]
    pub const fn is_price_event(self) -> bool {
        matches!(self, Self::Stop | Self::Target)
    }
}

impl std::fmt::Display for ExitTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_signal(stop: f64, target: Option<f64>) -> EnterSignal {
        EnterSignal {
            direction: Direction::Long,
            reference_price: 100.0,
            stop_price: stop,
            take_profit_price: target,
            max_risk_pct: 1.0,
            reasons: vec!["liquidity.swept == \"sell_side\"".into()],
        }
    }

    #[test]
    fn risk_per_unit_is_positive_for_both_directions() {
        assert!((long_signal(95.0, None).risk_per_unit() - 5.0).abs() < 1e-9);

        let mut short = long_signal(105.0, None);
        short.direction = Direction::Short;
        assert!((short.risk_per_unit() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_stop_on_the_wrong_side_is_invalid() {
        // Long with the stop above the entry: not a bad risk profile, a bug.
        assert!(!long_signal(105.0, None).stop_is_valid());
        assert!(long_signal(95.0, None).stop_is_valid());

        let mut short = long_signal(95.0, None);
        short.direction = Direction::Short;
        assert!(!short.stop_is_valid());
    }

    #[test]
    fn a_target_on_the_wrong_side_is_invalid() {
        assert!(long_signal(95.0, Some(110.0)).target_is_valid());
        assert!(!long_signal(95.0, Some(90.0)).target_is_valid());
        // No target at all is fine -- the trade exits on stop or condition.
        assert!(long_signal(95.0, None).target_is_valid());

        let mut short = long_signal(105.0, Some(90.0));
        short.direction = Direction::Short;
        assert!(short.target_is_valid());
        short.take_profit_price = Some(110.0);
        assert!(!short.target_is_valid());
    }

    #[test]
    fn signal_accessors_agree_with_the_variant() {
        let enter = Signal::Enter(long_signal(95.0, None));
        assert_eq!(enter.action(), SignalAction::Enter);
        assert!(enter.as_enter().is_some());
        assert!(enter.as_exit().is_none());

        let exit = Signal::Exit(ExitSignal::from_conditions(
            ExitTrigger::Invalidation,
            vec!["close_below(stop_price)".into()],
        ));
        assert_eq!(exit.action(), SignalAction::Exit);
        assert!(exit.as_exit().is_some());
        assert!(exit.as_enter().is_none());
    }

    #[test]
    fn price_event_triggers_are_distinguishable_from_condition_triggers() {
        assert!(ExitTrigger::Stop.is_price_event());
        assert!(ExitTrigger::Target.is_price_event());
        assert!(!ExitTrigger::Invalidation.is_price_event());
        assert!(!ExitTrigger::ExitCondition.is_price_event());
        assert!(!ExitTrigger::EndOfData.is_price_event());
    }

    #[test]
    fn signals_serialize_with_their_action_tagged() {
        let signal = Signal::Enter(long_signal(95.0, Some(110.0)));
        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains("\"action\":\"enter\""), "{json}");
        assert!(json.contains("\"stop_price\":95.0"), "{json}");
    }
}
