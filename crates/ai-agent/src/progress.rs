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
    /// A chart timeframe the agent is examining now. The shell can show the
    /// matching captured image in its chart overlay while this read runs.
    ReviewFrame {
        /// Instrument under review.
        symbol: String,
        /// Timeframe being read, e.g. `4h`.
        timeframe: String,
        /// One-based position in this review sequence.
        index: usize,
        /// Number of frames in this review sequence.
        total: usize,
        /// Which pass this frame belongs to. `false` is the plain look at the
        /// chart; `true` is the look at the chart *the agent has marked*.
        ///
        /// Always written, and it is the only way this can be read: `Progress`
        /// is serialise-only, so the Rust side never meets a line without the
        /// key and cannot offer one a compatibility for. The decoding happens
        /// in the shell's JS, and it is the shell that decides what a missing
        /// key means -- there, the plain pass, which is what every run before
        /// this field was.
        marked: bool,
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
    /// The model asked for the screen to change (`docs/47`): open a chart,
    /// switch the one being watched.
    ///
    /// Rides the progress channel rather than a frame of its own because that
    /// is the channel already streamed live over the socket -- a command that
    /// waited for the run to end would open the chart after the answer about
    /// it, which is backwards. The client executes it; the server only ever
    /// *relays* it, which is the confirmed-not-seen posture: the model learns
    /// the command was issued, and confirms what the screen did from a later
    /// screenshot, never from the relay.
    UiCommand {
        /// `open_chart` or `set_chart`.
        action: String,
        /// The symbol the command names, when it names one.
        symbol: Option<String>,
        /// The timeframe the command names, when it names one.
        timeframe: Option<String>,
        /// The upload rendezvous id, when the command asks for a capture:
        /// the shell uploads the image under it, the run picks it up.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_id: Option<String>,
        /// The command's remaining fields verbatim (description, criteria,
        /// ...): the typed fields above serve the chart commands; pipeline
        /// commands carry their own payload, and a typed struct per command
        /// would make the relay know more than it needs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<serde_json::Value>,
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
        NoProgress.report(Progress::ReviewFrame {
            symbol: "BTCUSDT".into(),
            timeframe: "4h".into(),
            index: 1,
            total: 3,
            marked: false,
        });
    }

    /// The shell branches on `stage` and reads the counter to caption the card.
    /// A rename on either side is invisible to the compiler and shows up as a
    /// chart that never gets a review picture, so the wire shape is pinned.
    #[test]
    fn a_review_frame_is_the_shape_the_shell_reads() {
        let value = serde_json::to_value(Progress::ReviewFrame {
            symbol: "BTCUSDT".into(),
            timeframe: "4h".into(),
            index: 2,
            total: 5,
            marked: false,
        })
        .expect("progress serializes");
        assert_eq!(value["stage"], "review_frame");
        assert_eq!(value["symbol"], "BTCUSDT");
        assert_eq!(value["timeframe"], "4h");
        assert_eq!(value["index"], 2);
        assert_eq!(value["total"], 5);
        assert_eq!(value["marked"], serde_json::json!(false), "the shell reads `marked`");

        // The second pass has to be tellable apart from the first, on the wire,
        // or the shell captions a plain chart with "MARKED".
        let marked = serde_json::to_value(Progress::ReviewFrame {
            symbol: "BTCUSDT".into(),
            timeframe: "4h".into(),
            index: 1,
            total: 6,
            marked: true,
        })
        .expect("progress serializes");
        assert_eq!(marked["stage"], "review_frame");
        assert_eq!(marked["marked"], serde_json::json!(true), "pass two is named");
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
