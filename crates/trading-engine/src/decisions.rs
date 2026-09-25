//! Where a bot's decisions come from.
//!
//! Two ways to drive the **same** interpreter. `strategy-runtime` is one crate;
//! it is compiled into this process for [`Decisions::Native`], and compiled to
//! wasm32 with the document passed in as *data* for [`Decisions::Sandboxed`].
//!
//! A bot cannot tell which it holds -- `on_candle` is the whole of the interface
//! -- and that is the point. A bot that branched on "am I sandboxed" would be
//! two implementations of a bot, and the two could drift; instead there is one
//! bot driven two ways, which is the same argument `crates/sandbox/src/strategy.rs`
//! makes about the backtester's replay loop.
//!
//! ## Why both variants exist, and which one production uses
//!
//! Principle #6 says AI-authored logic never executes unsandboxed, so **every
//! bot the gateway creates is sandboxed**. [`Decisions::Native`] is not a
//! supported production path.
//!
//! It exists because the sandbox's central claim -- *a document decides the same
//! thing natively and in the sandbox* -- can only be checked against something.
//! `crates/sandbox/tests/equivalence.rs` checks it by running the backtester's
//! replay loop twice with a different strategy on each side. A native path that
//! nothing could construct would make that
//! claim unfalsifiable, which is worse than not making it.
//!
//! ## What the sandbox buys a bot
//!
//! A document that loops, allocates without bound or wedges the interpreter is
//! bounded by fuel, memory and a wall-clock ceiling enforced **outside** the
//! guest, and it fails as a recorded error rather than as a trading process that
//! stops answering. The cost is one crossing of the wasm boundary per candle,
//! which is nothing beside a five-minute bar.
//!
//! ## A failure must not be silent
//!
//! [`Strategy::on_candle`] has no error channel, so a sandboxed failure is
//! *stored* rather than returned. A bot that ignored it would look like a bot
//! whose strategy simply never fires -- and the sandbox's own documentation says
//! so. [`Decisions::sandbox_error_count`] is therefore read on every candle and
//! surfaced as a [`BotAlert`](crate::paper::BotAlert) on the transition, so a
//! strategy failing 200 times is a notification rather than a quiet nothing.

use analytics_core::types::Timeframe;
use sandbox::{SandboxError, SandboxedStrategy};
use strategy_dsl::StrategyDocument;
use strategy_runtime::context::MarketContext;
use strategy_runtime::engine::Strategy;
use strategy_runtime::signal::Signal;
use strategy_runtime::{RollingLadder, StrategyEngine};

/// What a bot needs from the document, derived the same way by both of its
/// constructors and by both bots.
///
/// It comes from the *document* rather than from an engine because a sandboxed
/// bot has no engine to ask: `decision_timeframe` is the shortest declared
/// timeframe, which `strategy-dsl` already computes. Deriving it in one place is
/// what keeps a sandboxed bot from disagreeing with a native one about which bar
/// it decides on -- a disagreement that would look like a strategy bug.
pub(crate) struct Shape {
    pub(crate) ladder: RollingLadder,
    pub(crate) decision_name: String,
    pub(crate) decision_resolution: Timeframe,
}

impl Shape {
    pub(crate) fn of(document: &StrategyDocument) -> Self {
        let (decision_name, decision_resolution) = document.decision_timeframe().map_or_else(
            || ("5m".to_string(), Timeframe::M5),
            |(name, tf)| (name.to_string(), tf),
        );
        Self {
            ladder: RollingLadder::new(&document.timeframes),
            decision_name,
            decision_resolution,
        }
    }
}

/// Which of the two paths a bot is on.
///
/// Reported rather than inferred. "Is this bot's logic sandboxed?" is a
/// principle-#6 fact about a running system, and a fact nobody can read is a
/// fact nobody can check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionPath {
    /// `strategy-runtime` interpreted in this process. The reference the sandbox
    /// is measured against; not used for a bot the gateway creates.
    Native,
    /// `strategy-runtime` compiled to wasm32 and driven inside `sandbox`, under
    /// fuel, memory and wall-clock ceilings.
    Sandboxed,
}

impl DecisionPath {
    /// The name this path goes by on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sandboxed => "sandboxed",
        }
    }
}

impl std::fmt::Display for DecisionPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The thing a bot asks for a decision.
///
/// Not a trait object, deliberately: the two variants are known at the call
/// site, and an enum keeps [`Decisions::path`] answerable without a downcast.
#[derive(Debug)]
// The size gap is structural, not a bug: `Native` holds the whole engine while
// `Sandboxed` holds one boxed handle. Boxing the larger variant would put an
// indirection on the hot path for zero benefit.
#[allow(clippy::large_enum_variant)]
pub enum Decisions {
    /// The interpreter runs in this process.
    Native(StrategyEngine),
    /// The interpreter runs inside the sandbox.
    Sandboxed(Box<SandboxedStrategy>),
}

impl Decisions {
    /// Build the sandboxed variant, or return the refusal.
    ///
    /// Split from the bot constructor so the refusal can be checked *before*
    /// a bot row is created, which would leave a `running` bot with no task.
    pub fn sandboxed(
        sandbox: &sandbox::Sandbox,
        document: &strategy_dsl::ValidatedStrategy,
    ) -> Result<Self, SandboxError> {
        Ok(Self::Sandboxed(Box::new(SandboxedStrategy::new(
            sandbox, document,
        )?)))
    }

    /// Which path this is.
    #[must_use]
    pub const fn path(&self) -> DecisionPath {
        match self {
            Self::Native(_) => DecisionPath::Native,
            Self::Sandboxed(_) => DecisionPath::Sandboxed,
        }
    }

    /// How many evaluations the sandbox recorded an error for.
    ///
    /// Zero for a native bot, which has nowhere to record one -- it panics the
    /// process instead, which is the difference this type exists to remove.
    #[must_use]
    pub fn sandbox_error_count(&self) -> usize {
        match self {
            Self::Native(_) => 0,
            Self::Sandboxed(strategy) => strategy.errors().len(),
        }
    }

    /// The most recent sandbox failure, in the sandbox's own words.
    #[must_use]
    pub fn last_sandbox_error(&self) -> Option<String> {
        match self {
            Self::Native(_) => None,
            Self::Sandboxed(strategy) => strategy.errors().last().map(ToString::to_string),
        }
    }

    /// Requests the sandbox host refused, in the guest's own words.
    #[must_use]
    pub fn sandbox_denials(&self) -> &[String] {
        match self {
            Self::Native(_) => &[],
            Self::Sandboxed(strategy) => strategy.denials(),
        }
    }

    /// What the sandboxed half of the bot has cost, when there is one.
    #[must_use]
    pub fn sandbox_usage(&self) -> Option<sandbox::ResourceUsage> {
        match self {
            Self::Native(_) => None,
            Self::Sandboxed(strategy) => Some(strategy.usage()),
        }
    }

    /// Every `SandboxError` recorded so far.
    #[must_use]
    pub fn sandbox_errors(&self) -> &[SandboxError] {
        match self {
            Self::Native(_) => &[],
            Self::Sandboxed(strategy) => strategy.errors(),
        }
    }
}

impl Strategy for Decisions {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
        match self {
            Self::Native(engine) => engine.on_candle(ctx),
            Self::Sandboxed(strategy) => strategy.on_candle(ctx),
        }
    }
}
