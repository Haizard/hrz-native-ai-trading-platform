//! What the agent is doing, while it is doing it.
//!
//! A question takes about a minute. The ladder is read from the database, then
//! the model is called once per turn, and each turn may call tools that read
//! more. Without this the socket is silent for that whole minute, and a user
//! cannot tell a slow run from a hung one -- which is the difference between
//! waiting and reloading.
//!
//! ## Why this is progress and not token streaming
//!
//! The obvious design -- stream the model's tokens -- does not fit this agent.
//! The answer is a `submit_thesis` **tool call**, not prose: the answering
//! phase announces that one tool and refuses every other, and the nudge it
//! sends says in as many words "Do not answer in prose". So there is no answer
//! text to stream, and a token stream would carry the model's narration, which
//! this design deliberately does not use.
//!
//! What there is to stream is the work: which timeframe is being read, which
//! turn the loop is on, which tool is running. That is what a user waiting a
//! minute actually wants to know.

use serde::Serialize;

/// One step of a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Progress {
    /// Reading the timeframe ladder, before the model is asked anything.
    ///
    /// Reported first because it happens first, and it is not instant: it is
    /// one database read per declared timeframe.
    ReadingMarket {
        /// The symbol being read.
        symbol: String,
        /// How many timeframes the ladder holds.
        timeframes: usize,
    },
    /// A turn of the tool loop began, and the model is being called.
    Thinking {
        /// 1-based turn number.
        turn: usize,
        /// How many turns the run is allowed in total.
        total: usize,
        /// Whether this is an answer-only turn.
        answering: bool,
    },
    /// The model asked for a tool, and it is being run.
    Tool {
        /// The tool's name, as the model called it.
        name: String,
    },
    /// A tool returned.
    ToolDone {
        /// The tool's name.
        name: String,
        /// Whether it returned an error the model will see.
        ok: bool,
    },
    /// The thesis cited a level no tool reported, and the model has been asked
    /// to correct it.
    ///
    /// Worth surfacing rather than hiding: it is a turn being spent on a
    /// fixable mistake, which is the difference between a run that takes one
    /// minute and one that takes two.
    Correcting {
        /// Why it was rejected.
        reason: String,
    },
}

/// Somewhere to report [`Progress`] to.
///
/// A trait rather than a closure so the agent does not have to name a lifetime
/// for it, and so "nobody is listening" is a type rather than a closure that
/// does nothing.
pub trait ProgressSink: Send + Sync {
    /// Report one step.
    ///
    /// Called between awaits, so an implementation that hands the step to a
    /// channel or a log is fine; one that blocks on I/O is not.
    fn report(&self, progress: Progress);
}

/// A sink that discards everything.
///
/// What `Agent::ask` uses, so a caller that does not care about progress pays
/// nothing for the machinery.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _progress: Progress) {}
}

/// A sink that keeps what it is told, for tests.
///
/// Crate-visible rather than private to this module's tests, because the tests
/// that matter are in `agent.rs`: what a run *reports* is only meaningful
/// against what the run actually did, and a double only one module can reach
/// is a double that gets copied.
#[cfg(test)]
#[derive(Default)]
pub struct Collected {
    steps: std::sync::Mutex<Vec<Progress>>,
}

#[cfg(test)]
impl Collected {
    /// Everything reported, in order.
    #[must_use]
    pub fn steps(&self) -> Vec<Progress> {
        self.steps.lock().expect("lock poisoned").clone()
    }
}

#[cfg(test)]
impl ProgressSink for Collected {
    fn report(&self, progress: Progress) {
        self.steps.lock().expect("lock poisoned").push(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_carries_its_stage_so_a_client_can_branch_on_it() {
        let step = Progress::Tool {
            name: "analyze_timeframe".into(),
        };
        let rendered = serde_json::to_value(&step).expect("a step serialises");
        assert_eq!(rendered["stage"], "tool");
        assert_eq!(rendered["name"], "analyze_timeframe");
    }

    #[test]
    fn a_turn_says_whether_it_is_an_answer_turn() {
        // The panel uses this to stop saying "analysing" when the model has
        // moved on to answering -- the two read very differently to a user
        // who has been waiting a minute.
        let step = Progress::Thinking {
            turn: 4,
            total: 6,
            answering: true,
        };
        let rendered = serde_json::to_value(&step).expect("a step serialises");
        assert_eq!(rendered["stage"], "thinking");
        assert_eq!(rendered["turn"], 4);
        assert_eq!(rendered["total"], 6);
        assert_eq!(rendered["answering"], true);
    }

    #[test]
    fn the_discarding_sink_keeps_nothing() {
        // It exists to be cheap, so the test is that it is callable with every
        // variant -- a new variant must not force every caller to handle it.
        NoProgress.report(Progress::ReadingMarket {
            symbol: "BTCUSDT".into(),
            timeframes: 3,
        });
        NoProgress.report(Progress::Correcting {
            reason: "stop 64000 was never reported by a tool".into(),
        });
    }

    #[test]
    fn a_collecting_sink_receives_steps_in_order() {
        let sink = Collected::default();
        sink.report(Progress::ReadingMarket {
            symbol: "BTCUSDT".into(),
            timeframes: 2,
        });
        sink.report(Progress::Thinking {
            turn: 1,
            total: 6,
            answering: false,
        });
        let steps = sink.steps();
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], Progress::ReadingMarket { .. }));
        assert!(matches!(steps[1], Progress::Thinking { turn: 1, .. }));
    }
}
