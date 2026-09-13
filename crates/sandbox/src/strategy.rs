//! Drive a sandboxed interpreter from the backtester's own replay loop.
//!
//! [`SandboxedStrategy`] implements [`strategy_runtime::Strategy`], which is the
//! trait `backtester::replay` already takes. That is the whole integration: a
//! backtest runs through the sandbox by swapping the strategy, with no change to
//! the replay loop, the simulator or the report.
//!
//! It matters that this is the *only* integration point. If the sandbox had its
//! own replay path, "runs identically natively and in the sandbox" would be a
//! claim about two implementations agreeing rather than a claim about one
//! implementation being driven two ways -- and the second kind of claim is the
//! one that stays true.

use strategy_dsl::ValidatedStrategy;
use strategy_runtime::context::MarketContext;
use strategy_runtime::engine::Strategy;
use strategy_runtime::signal::Signal;

use crate::error::SandboxError;
use crate::instance::{ResourceUsage, Sandbox, Session};

/// A strategy whose every decision is made inside the sandbox.
///
/// ## Failures are recorded, not swallowed
///
/// [`Strategy::on_candle`] returns `Option<Signal>` and has no error channel, so
/// a sandboxed failure has to be stored rather than returned. It is therefore
/// **essential** to check [`SandboxedStrategy::errors`] after a run: a strategy
/// that failed on 200 of 10,000 candles produced a valid-looking report over the
/// other 9,800, and nothing in the numbers would say so.
pub struct SandboxedStrategy<'a> {
    session: Session<'a>,
    errors: Vec<SandboxError>,
    denials: Vec<String>,
    evaluations: u64,
    signals: u64,
}

impl std::fmt::Debug for SandboxedStrategy<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedStrategy")
            .field("evaluations", &self.evaluations)
            .field("signals", &self.signals)
            .field("errors", &self.errors.len())
            .finish_non_exhaustive()
    }
}

impl<'a> SandboxedStrategy<'a> {
    /// Instantiate a sandboxed interpreter for `document`.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::start`].
    pub fn new(sandbox: &'a Sandbox, document: &ValidatedStrategy) -> Result<Self, SandboxError> {
        Ok(Self {
            session: sandbox.start(document)?,
            errors: Vec::new(),
            denials: Vec::new(),
            evaluations: 0,
            signals: 0,
        })
    }

    /// Everything that went wrong. Non-empty means the run is not trustworthy.
    #[must_use]
    pub fn errors(&self) -> &[SandboxError] {
        &self.errors
    }

    /// Requests the host refused, in the guest's own words.
    #[must_use]
    pub fn denials(&self) -> &[String] {
        &self.denials
    }

    /// Writes the host refused for exceeding a ceiling.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        self.session.refusals()
    }

    /// What the sandboxed half of the run cost.
    #[must_use]
    pub const fn usage(&self) -> ResourceUsage {
        self.session.usage()
    }

    /// Candles the sandbox was asked about.
    #[must_use]
    pub const fn evaluations(&self) -> u64 {
        self.evaluations
    }

    /// Signals the sandbox emitted.
    #[must_use]
    pub const fn signals(&self) -> u64 {
        self.signals
    }

    /// Whether every evaluation completed cleanly.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.denials.is_empty()
    }
}

impl Strategy for SandboxedStrategy<'_> {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
        self.evaluations += 1;

        match self.session.evaluate(ctx) {
            Ok(mut signals) => {
                self.denials.extend(self.session.denials().iter().cloned());
                // The interpreter emits at most one signal per candle. If a
                // future one emits several, the extra is dropped rather than
                // silently reshaping the replay -- and `signals` will show the
                // discrepancy against `evaluations`.
                let signal = signals.pop();
                if signal.is_some() {
                    self.signals += 1;
                }
                signal
            }
            Err(error) => {
                self.denials.extend(self.session.denials().iter().cloned());
                self.errors.push(error);
                None
            }
        }
    }
}
