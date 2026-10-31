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

use analytics_core::types::{Candle, Timeframe};
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
    /// The ladder for a composition: one declared timeframe (the
    /// composition's own), deciding on it. The name is fixed so the
    /// strategy's context lookup has exactly one key to miss.
    pub(crate) fn for_composition(document: &market_data::composer::Composition) -> Self {
        let tf: Timeframe = document
            .timeframe
            .parse()
            .expect("the composition validated before the bot was created");
        let mut timeframes = std::collections::BTreeMap::new();
        timeframes.insert("composition".to_string(), tf);
        Self {
            ladder: RollingLadder::new(&timeframes),
            decision_name: "composition".to_string(),
            decision_resolution: tf,
        }
    }

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
    /// A Concept-Registry composition evaluated over the event stream (P3b).
    Composer,
}

impl DecisionPath {
    /// The name this path goes by on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sandboxed => "sandboxed",
            Self::Composer => "composer",
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
    /// A Concept-Registry composition evaluated over the event stream (P3b):
    /// the bot's decisions come from the composer, not from a DSL document.
    Composer(ComposerStrategy),
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
            Self::Composer(_) => DecisionPath::Composer,
        }
    }

    /// How many evaluations the sandbox recorded an error for.
    ///
    /// Zero for a native bot, which has nowhere to record one -- it panics the
    /// process instead, which is the difference this type exists to remove.
    #[must_use]
    pub fn sandbox_error_count(&self) -> usize {
        match self {
            Self::Native(_) | Self::Composer(_) => 0,
            Self::Sandboxed(strategy) => strategy.errors().len(),
        }
    }

    /// The most recent sandbox failure, in the sandbox's own words.
    #[must_use]
    pub fn last_sandbox_error(&self) -> Option<String> {
        match self {
            Self::Native(_) | Self::Composer(_) => None,
            Self::Sandboxed(strategy) => strategy.errors().last().map(ToString::to_string),
        }
    }

    /// Requests the sandbox host refused, in the guest's own words.
    #[must_use]
    pub fn sandbox_denials(&self) -> &[String] {
        match self {
            Self::Native(_) | Self::Composer(_) => &[],
            Self::Sandboxed(strategy) => strategy.denials(),
        }
    }

    /// What the sandboxed half of the bot has cost, when there is one.
    #[must_use]
    pub fn sandbox_usage(&self) -> Option<sandbox::ResourceUsage> {
        match self {
            Self::Native(_) | Self::Composer(_) => None,
            Self::Sandboxed(strategy) => Some(strategy.usage()),
        }
    }

    /// Every `SandboxError` recorded so far.
    #[must_use]
    pub fn sandbox_errors(&self) -> &[SandboxError] {
        match self {
            Self::Native(_) | Self::Composer(_) => &[],
            Self::Sandboxed(strategy) => strategy.errors(),
        }
    }
}

/// A composition-backed strategy: the Concept Registry's declarative
/// documents driving a bot through the same `Signal` contract every other
/// path speaks.
#[derive(Debug)]
pub struct ComposerStrategy {
    /// The decision timeframe's own resolution.
    decision_tf: analytics_core::types::Timeframe,
    /// Extra timeframes already fed, by their completed-bar close time.
    extra_fed: std::collections::HashMap<analytics_core::types::Timeframe, i64>,
    composer: market_data::composer::Composer,
    symbol: String,
    /// The monotonic decision-bar counter (the composer's lookback clock).
    bar: u64,
    /// The trailing window detection reads (bounded like the runtime's).
    history: Vec<Candle>,
    plan: market_data::composer::DetectionPlan,
}

impl ComposerStrategy {
    /// Bind a validated composition to a market.
    #[must_use]
    pub fn new(document: market_data::composer::Composition, symbol: &str) -> Self {
        Self {
            plan: market_data::composer::DetectionPlan::for_catalog(&document.parameters),
            decision_tf: document.timeframe.parse().unwrap_or(analytics_core::types::Timeframe::M5),
            composer: market_data::composer::Composer::new(document),
            symbol: symbol.to_string(),
            bar: 0,
            history: Vec::new(),
            extra_fed: std::collections::HashMap::new(),
        }
    }

    /// The composition this strategy runs.
    #[must_use]
    pub fn document(&self) -> &market_data::composer::Composition {
        self.composer.document()
    }
}

impl ComposerStrategy {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
        use strategy_dsl::Direction;
        use strategy_runtime::EnterSignal;

        let view = ctx.timeframes.get(&ctx.decision_timeframe)?;
        self.history.push(view.candle.clone());
        // The detection window: the engine's structure lookback needs ~2x
        // plus margin -- and an MTF document needs it MULTIPLIED by the
        // largest resample factor its conditions name (a 4h condition on a
        // 1m document wants 240x240 base bars to have any upstream at all,
        // bounded at a sane ceiling).
        let factor = market_data::composer::involved_timeframes(self.composer.document())
            .iter()
            .filter_map(|name| name.parse::<analytics_core::types::Timeframe>().ok())
            .map(|extra| (extra.nanos() / self.decision_tf.nanos().max(1)).max(1) as usize)
            .max()
            .unwrap_or(1);
        let depth = (240 * factor).min(50_000);
        if self.history.len() > depth {
            self.history.drain(..self.history.len() - depth);
        }
        let events: Vec<_> = self
            .plan
            .detect(&self.history, &self.symbol)
            .into_iter()
            .filter(|e| e.event_time_ns == view.candle.open_time)
            .collect();
        // The extra timeframes' events, fed with their tags: resample the
        // history upstream, detect on the newest completed extra bar, and
        // feed it when the decision bar reaches the extra bar's close.
        for name in market_data::composer::involved_timeframes(self.composer.document()) {
            if let Ok(extra) = name.parse::<analytics_core::types::Timeframe>() {
                if extra.nanos() <= self.decision_tf.nanos() {
                    continue;
                }
                let resampled = analytics_core::resample::resample(&self.history, extra);
                if resampled.len() < 2 {
                    continue;
                }
                // The newest COMPLETED extra bar (the one before the forming one).
                let completed_idx = resampled.len() - 2;
                let completed_time = resampled[completed_idx].open_time + extra.nanos();
                if completed_time != self.extra_fed.get(&extra).copied().unwrap_or(0) {
                    let extra_events: Vec<_> =
                        self.plan.detect(&resampled[..=completed_idx], &self.symbol);
                    let tf_name = extra.to_string();
                    let _ = self.composer.on_bar(self.bar as usize, &extra_events, Some(&tf_name), None);
                    self.extra_fed.insert(extra, completed_time);
                }
            }
        }
        self.composer.set_last_close(view.candle.close);
        let fired = self.composer.on_bar(self.bar as usize, &events, None, None);
        self.bar += 1;
        let fired = fired?;

        // Protection is mandatory in this world (EnterSignal carries a stop,
        // not an Option): an entry whose stop cannot be resolved is not an
        // entry -- unprotected is not a signal, it is a refusal.
        let stop_price = match &self.composer.document().risk.stop {
            market_data::composer::StopKind::Structure => fired.stop_level?,
            market_data::composer::StopKind::AtrMultiple { multiple } => {
                let atr = analytics_core::indicators::atr(&self.history, 14)
                    .last()
                    .copied()
                    .flatten()?;
                if fired.long {
                    view.candle.close - multiple * atr
                } else {
                    view.candle.close + multiple * atr
                }
            }
        };
        let max_risk_pct = match &self.composer.document().risk.sizing {
            market_data::composer::SizeKind::FixedFractional { percent } => *percent,
            // A fixed-stake composition reads as a 1% risk budget in the
            // DSL's equity-fraction world; the venue-level stake comes from
            // the order's quantity for contract adapters.
            market_data::composer::SizeKind::Fixed { .. } => 1.0,
            // Volatility-adjusted risks its fraction; Kelly bets its cap.
            market_data::composer::SizeKind::VolatilityAdjusted { risk_fraction } => risk_fraction * 100.0,
            market_data::composer::SizeKind::Kelly { cap } => cap * 100.0,
        };
        Some(Signal::Enter(EnterSignal {
            direction: if fired.long { Direction::Long } else { Direction::Short },
            reference_price: view.candle.close,
            stop_price,
            take_profit_price: None,
            max_risk_pct,
            reasons: fired.evidence,
        }))
    }
}

impl Strategy for Decisions {
    fn on_candle(&mut self, ctx: &MarketContext) -> Option<Signal> {
        match self {
            Self::Native(engine) => engine.on_candle(ctx),
            Self::Sandboxed(strategy) => strategy.on_candle(ctx),
            Self::Composer(strategy) => strategy.on_candle(ctx),
        }
    }
}
