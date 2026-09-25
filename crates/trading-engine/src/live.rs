//! The live-trading bot (`docs/11`, `docs/15`).
//!
//! ## The same pipeline, one different hop
//!
//! This bot asks the strategy exactly when the paper bot does, over the same
//! ladder and the same engine, and it checks the same risk limits. The single
//! difference is what a signal *becomes*: paper hands it to a simulator, this
//! hands it to an [`OrderGateway`].
//!
//! ## Protective orders, and what happens when they are not there
//!
//! An entry is placed together with a stop and, when the document declares
//! one, a take-profit. That pair is the whole risk control at the venue, so
//! the bot treats their absence as an emergency rather than a bookkeeping
//! difference: if reconciliation shows a position with no protective order on
//! the exchange, it **closes at market** and says so. A reconciliation
//! mismatch is a report; an unprotected position is a loss waiting to happen,
//! and the two are not the same severity.
//!
//! ## Nothing here decides anything the risk engine has not allowed
//!
//! Sizing, concurrency and the loss budget all go through [`RiskEngine`]
//! first. This module never reads the document's own risk numbers except to
//! hand them to the engine to be checked.

use analytics_core::types::{Candle, Timeframe};
use observability::metrics::{
    Labels, Registry, KILL_SWITCH, OPEN_POSITIONS, ORDERS_EXECUTED, RECONCILE_MISMATCHES,
    RISK_BREACHES, SIGNALS_GENERATED,
};
use sandbox::SandboxError;
use serde::Serialize;
use strategy_dsl::ValidatedStrategy;
use strategy_runtime::{
    EnterSignal, ExitSignal, ExitTrigger, PositionView, RollingConfig, RollingLadder, Signal,
    Strategy, StrategyEngine,
};

use crate::decisions::{DecisionPath, Decisions, Shape};
use crate::error::ExecutionError;
use crate::execution::{
    client_order_id, ExchangeAdapter, OrderGateway, OrderRequest, OrderSide, OrderStatus, OrderType,
};
use crate::risk::{OnBreach, RiskEngine, RiskLimits, RiskVerdict};

/// What the live bot needs that the document does not say.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveConfig {
    /// Symbol traded.
    pub symbol: String,
    /// An identifier used in order ids, so two bots on one account cannot
    /// collide.
    pub bot_id: String,
    /// Risk limits, clamped on construction.
    pub limits: RiskLimits,
    /// Retained history and per-timeframe tuning.
    pub rolling: RollingConfig,
    /// Account equity used for sizing.
    pub equity: f64,
    /// Below this, an order is not worth sending -- dust, and an exchange
    /// minimum the platform does not know about.
    pub min_quantity: f64,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            symbol: "BTCUSDT".into(),
            bot_id: "bot".into(),
            limits: RiskLimits::default(),
            rolling: RollingConfig::default(),
            equity: 10_000.0,
            min_quantity: 0.0001,
        }
    }
}

/// An open position at the venue.
#[derive(Debug, Clone, PartialEq)]
pub struct LivePosition {
    /// Direction.
    pub side: OrderSide,
    /// Base quantity.
    pub quantity: f64,
    /// Fill price of the entry.
    pub entry_price: f64,
    /// Stop price, as sent.
    pub stop_price: f64,
    /// Take-profit price, as sent, if any.
    pub target_price: Option<f64>,
    /// Client id of the protective stop.
    pub stop_order_id: String,
    /// Client id of the take-profit, if any.
    pub target_order_id: Option<String>,
    /// When it was opened, unix nanos.
    pub opened_at: i64,
}

impl LivePosition {
    /// Risk in price terms: how far the stop is from the entry.
    #[must_use]
    pub fn risk_distance(&self) -> f64 {
        (self.entry_price - self.stop_price).abs()
    }
}

/// A completed live trade.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveTrade {
    /// Direction.
    pub side: OrderSide,
    /// Entry fill.
    pub entry_price: f64,
    /// Exit fill.
    pub exit_price: f64,
    /// Quantity.
    pub quantity: f64,
    /// The stop the position carried, as sent.
    ///
    /// Kept on the trade rather than only on the position because the position
    /// is dropped when the trade closes, and `trades_executed` is what the
    /// P&L surface reads. A row whose `stop_price` is `NULL` cannot be drawn
    /// against the price axis.
    pub stop_price: f64,
    /// The take-profit, if the document declared one.
    pub target_price: Option<f64>,
    /// Result in R.
    pub r_multiple: f64,
    /// Opened at, unix nanos.
    pub opened_at: i64,
    /// Closed at, unix nanos.
    pub closed_at: i64,
    /// What closed it.
    pub trigger: String,
}

/// What one decision candle produced.
///
/// Serialized, because it is what `/ws/bots/{id}` streams to a watcher. The
/// outcome is tagged by `kind` so a client can switch on it without parsing
/// prose -- the same rule the agent's progress stream follows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveRecord {
    /// The decision candle's close time, unix nanos.
    pub at: i64,
    /// Symbol traded.
    pub symbol: String,
    /// Price the strategy was shown.
    pub price: f64,
    /// Whether a position was held when it decided.
    pub in_position: bool,
    /// What came of it.
    pub outcome: LiveOutcome,
}

/// The outcome of one decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveOutcome {
    /// The ladder was not warm enough to ask.
    NoContext,
    /// Asked, nothing to do.
    NoSignal,
    /// An entry was placed and filled.
    Entered {
        /// Conditions that fired.
        reasons: Vec<String>,
        /// Quantity.
        quantity: f64,
        /// Fill price.
        price: f64,
    },
    /// The risk engine refused the entry.
    EntryDenied {
        /// Conditions that fired.
        reasons: Vec<String>,
        /// Which limit.
        limit: String,
        /// The value.
        value: String,
    },
    /// The order was refused by the venue or was too small to send.
    EntryRefused {
        /// Why.
        reason: String,
    },
    /// A protective order filled and the position is closed.
    Closed {
        /// Stop, target, condition, or the switch.
        trigger: String,
        /// Result in R.
        r_multiple: f64,
    },
    /// A position was closed by a signal or by the switch.
    Exited {
        /// Why.
        trigger: String,
    },
    /// A protective order vanished while a position was open. Closed at market.
    ProtectionLost {
        /// What was missing.
        detail: String,
    },
    /// The kill-switch is engaged; the strategy was not asked.
    Halted {
        /// Why.
        reason: String,
    },
}

/// A strategy running against live candles with real orders.
pub struct LiveBot<A: ExchangeAdapter> {
    symbol: String,
    bot_id: String,
    decision_name: String,
    decision_resolution: Timeframe,
    ladder: RollingLadder,
    /// The thing that decides, which may be native or sandboxed -- the same
    /// seam the paper bot uses, so the two cannot drift about *where* a decision
    /// comes from any more than they can about what it says.
    strategy: Decisions,
    risk: RiskEngine,
    rolling: RollingConfig,
    equity: f64,
    min_quantity: f64,
    gateway: OrderGateway<A>,
    position: Option<LivePosition>,
    records: Vec<LiveRecord>,
    trades: Vec<LiveTrade>,
    halted_announced: bool,
    /// Whether the sandbox had already failed when the bot last looked, so the
    /// first failure is logged once rather than once per candle.
    noted_sandbox_failure: bool,
}

impl<A: ExchangeAdapter> LiveBot<A> {
    /// Start a live bot for an already-validated strategy, decided in this
    /// process.
    ///
    /// The reference the sandbox is measured against. **Not the path the gateway
    /// creates a bot on** -- see [`LiveBot::with_strategy`] and principle #6.
    ///
    /// The caller is responsible for having passed [`crate::gate::LiveGate`]:
    /// this constructor does not check the venue opt-in, because a bot built in
    /// a test must be buildable without one.
    #[must_use]
    pub fn new(engine: StrategyEngine, adapter: A, config: LiveConfig) -> Self {
        let shape = Shape::of(engine.document());
        Self::from_shape(Decisions::Native(engine), adapter, shape, config)
    }

    /// Start a live bot around a strategy that has already been prepared.
    ///
    /// The same split as [`PaperBot::with_strategy`](crate::paper::PaperBot::with_strategy),
    /// and it matters more here: preparing a sandboxed strategy can fail, and for
    /// a live bot the row carries the id every client order id is built from --
    /// so the failure has to be able to land before the row exists.
    #[must_use]
    pub fn with_strategy(
        strategy: Decisions,
        document: &ValidatedStrategy,
        adapter: A,
        config: LiveConfig,
    ) -> Self {
        let shape = Shape::of(document.document());
        Self::from_shape(strategy, adapter, shape, config)
    }

    /// Which path this bot decides on: native or sandboxed.
    #[must_use]
    pub const fn decision_path(&self) -> DecisionPath {
        self.strategy.path()
    }

    /// Everything the sandbox recorded a failure for. Empty for a native bot.
    #[must_use]
    pub fn sandbox_errors(&self) -> &[SandboxError] {
        self.strategy.sandbox_errors()
    }

    fn from_shape(strategy: Decisions, adapter: A, shape: Shape, config: LiveConfig) -> Self {
        Self {
            symbol: config.symbol,
            bot_id: config.bot_id,
            decision_name: shape.decision_name,
            decision_resolution: shape.decision_resolution,
            ladder: shape.ladder,
            strategy,
            risk: RiskEngine::new(config.limits),
            rolling: config.rolling,
            equity: config.equity,
            min_quantity: config.min_quantity,
            gateway: OrderGateway::new(adapter),
            position: None,
            records: Vec::new(),
            trades: Vec::new(),
            halted_announced: false,
            noted_sandbox_failure: false,
        }
    }

    /// The symbol traded.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// The current position, if any.
    #[must_use]
    pub fn position(&self) -> Option<&LivePosition> {
        self.position.as_ref()
    }

    /// Completed trades.
    #[must_use]
    pub fn trades(&self) -> &[LiveTrade] {
        &self.trades
    }

    /// Summed R across completed trades.
    #[must_use]
    pub fn cumulative_r(&self) -> f64 {
        self.trades.iter().map(|trade| trade.r_multiple).sum()
    }

    /// Every order this bot believes exists at the venue, as (request, ack).
    ///
    /// Exposed so the persistence layer can write `live_orders` from the same
    /// book the bot reasons with. Reading the venue instead would record what
    /// the exchange says, which is exactly the half that reconciliation exists
    /// to compare against.
    #[must_use]
    pub fn orders(&self) -> Vec<(&OrderRequest, &crate::execution::OrderAck)> {
        self.gateway.entries()
    }

    /// The venue this bot trades, as the adapter names itself.
    #[must_use]
    pub fn venue(&self) -> &str {
        self.gateway.adapter().venue()
    }

    /// Whether the kill-switch is engaged.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.risk.is_killed()
    }

    /// Why the kill-switch is engaged.
    #[must_use]
    pub fn halt_reason(&self) -> Option<&str> {
        self.risk.reason()
    }

    /// The risk engine, mutable, so an operator can trip the switch by hand.
    pub fn risk_mut(&mut self) -> &mut RiskEngine {
        &mut self.risk
    }

    /// Trip the kill switch and act on it immediately.
    ///
    /// The switch is normally read inside [`decide`](Self::decide), which runs
    /// on a decision bar -- five minutes away for a five-minute strategy. A
    /// button labelled "stop" that takes five minutes is not a stop, so this
    /// closes the position now rather than at the next bar.
    ///
    /// The switch is tripped **first**, before the close is attempted, so a
    /// failed close leaves a bot that will not open anything else rather than
    /// one that carries on trading.
    ///
    /// # Errors
    /// Returns the execution error when the emergency close itself failed. The
    /// switch stays engaged either way, and the runbook's answer is to close
    /// the position at the venue by hand and reconcile.
    pub async fn kill(&mut self, reason: &str, now: i64) -> Result<(), ExecutionError> {
        self.risk.kill(reason);
        Registry::global().count(KILL_SWITCH, "kill-switch activations", &Labels::none());

        if self.position.is_none() {
            return Ok(());
        }
        match self.risk.limits().on_breach {
            OnBreach::Close => {
                self.close_at_market(now, ExitTrigger::KillSwitch, "kill-switch")
                    .await
            }
            OnBreach::Hold => {
                // docs/11: what happens to an open position is a configured
                // policy. `Hold` means the operator decides, and that has to
                // include deciding *not* to liquidate -- otherwise the setting
                // is decoration.
                tracing::warn!(
                    symbol = %self.symbol,
                    "the kill-switch is engaged with a position open and the policy is `Hold`; \
                     the position is not protected by anything the platform will do"
                );
                Ok(())
            }
        }
    }

    /// Take the records produced since the last call.
    pub fn take_records(&mut self) -> Vec<LiveRecord> {
        std::mem::take(&mut self.records)
    }

    /// Compare our book with the exchange's.
    ///
    /// # Errors
    /// Propagates the adapter's error.
    pub async fn reconcile(&self) -> Result<crate::execution::Reconciliation, ExecutionError> {
        self.gateway.reconcile().await
    }

    /// The adapter, for a caller that wants to query the venue directly.
    ///
    /// Public rather than test-only: reconciliation is something an *operator*
    /// does, and the runbook's answer to an unknown order fate is to ask the
    /// exchange by hand.
    pub fn adapter(&self) -> &A {
        self.gateway.adapter()
    }

    /// The order gateway, for tests to assert on what was sent.
    #[cfg(test)]
    pub fn gateway_for_test(&self) -> &OrderGateway<A> {
        &self.gateway
    }

    /// The adapter, for tests to stage a scenario at the venue.
    #[cfg(test)]
    pub fn adapter_for_test(&self) -> &A {
        self.gateway.adapter()
    }

    /// Feed one closed candle, placing orders if the strategy says to.
    ///
    /// Returns a record when this candle advanced the decision clock.
    ///
    /// # Errors
    /// Returns an execution error only when the bot could not safely continue:
    /// a transport failure that left an order's fate unknown stops the bot
    /// rather than trading blind.
    pub async fn on_candle(
        &mut self,
        candle: &Candle,
    ) -> Result<Option<LiveRecord>, ExecutionError> {
        let mut advanced = false;
        for (name, frame) in self.ladder.iter_mut() {
            if frame.timeframe() != candle.timeframe {
                continue;
            }
            if frame.push(candle.clone(), &self.rolling) && *name == self.decision_name {
                advanced = true;
            }
        }
        if !advanced {
            return Ok(None);
        }

        let now = candle.open_time + self.decision_resolution.nanos();
        let record = self.decide(now, candle).await?;
        self.records.push(record.clone());
        Ok(Some(record))
    }

    /// One decision.
    async fn decide(&mut self, now: i64, candle: &Candle) -> Result<LiveRecord, ExecutionError> {
        let price = candle.close;

        if let Some(reason) = self.halt_reason().map(str::to_string) {
            // docs/11: what happens to an open position is a configured policy,
            // not a silent default.
            if self.position.is_some() && self.risk.limits().on_breach == OnBreach::Close {
                self.close_at_market(now, ExitTrigger::KillSwitch, "kill-switch")
                    .await?;
            }
            if !self.halted_announced {
                self.halted_announced = true;
                Registry::global().count(KILL_SWITCH, "kill-switch activations", &Labels::none());
            }
            return Ok(self.record(
                now,
                price,
                LiveOutcome::Halted {
                    reason: reason.clone(),
                },
            ));
        }

        // 1. Did a protective order fill? That closes the position.
        if let Some(outcome) = self.check_protection(now, candle).await? {
            return Ok(self.record(now, price, outcome));
        }

        // 2. Ask the strategy, on every decision bar, exactly as paper does.
        let context = self.ladder.context(
            &self.symbol,
            now,
            &self.decision_name,
            self.position_view(price),
            self.equity,
        );

        let outcome = match context.and_then(|context| self.strategy.on_candle(&context)) {
            Some(Signal::Enter(enter)) => self.enter(enter, now, candle).await?,
            Some(Signal::Exit(exit)) => self.exit(exit, now, candle).await?,
            None => LiveOutcome::NoSignal,
        };

        // A sandboxed failure has no channel to be returned on -- `on_candle`
        // yields a signal, not a `Result` -- so it is noticed here, and logged
        // on the transition only.
        //
        // Note what this deliberately does **not** do: halt the bot. Halting a
        // live bot that holds an open position is a decision about someone's
        // money, and closing row 1 is not the place to make it. `docs/19` row 26
        // records that the response to a sandbox failure is still undecided --
        // an unknown is written down rather than settled by omission.
        self.note_sandbox_failure();

        Ok(self.record(now, price, outcome))
    }

    /// Log the first sandbox failure, once.
    fn note_sandbox_failure(&mut self) {
        if self.noted_sandbox_failure || self.strategy.sandbox_error_count() == 0 {
            return;
        }
        self.noted_sandbox_failure = true;
        tracing::error!(
            bot = %self.bot_id,
            symbol = %self.symbol,
            detail = self.strategy.last_sandbox_error().unwrap_or_default(),
            "the bot's strategy failed inside the sandbox; those decisions are missing"
        );
    }

    /// Whether a protective order filled, or has gone missing.
    async fn check_protection(
        &mut self,
        now: i64,
        candle: &Candle,
    ) -> Result<Option<LiveOutcome>, ExecutionError> {
        let Some(position) = self.position.clone() else {
            return Ok(None);
        };

        let reports = self.gateway.adapter().reconcile().await?;
        let stop = reports
            .iter()
            .find(|report| report.client_order_id == position.stop_order_id);
        let target = position
            .target_order_id
            .as_ref()
            .and_then(|id| reports.iter().find(|report| report.client_order_id == *id));

        // A stop that filled closes the position; the target, if any, is now
        // an orphan and must be cancelled or it will trade against us.
        if let Some(filled) = stop.filter(|report| report.status == OrderStatus::Filled) {
            self.close(now, filled.avg_price.unwrap_or(position.stop_price), "stop")
                .await?;
            return Ok(Some(LiveOutcome::Closed {
                trigger: "stop".into(),
                r_multiple: self.trades.last().map_or(0.0, |t| t.r_multiple),
            }));
        }
        if let Some(filled) = target.filter(|report| report.status == OrderStatus::Filled) {
            self.close(
                now,
                filled
                    .avg_price
                    .unwrap_or(position.target_price.unwrap_or(candle.close)),
                "target",
            )
            .await?;
            return Ok(Some(LiveOutcome::Closed {
                trigger: "target".into(),
                r_multiple: self.trades.last().map_or(0.0, |t| t.r_multiple),
            }));
        }

        // The emergency: we hold a position and the exchange has no stop for
        // it. This is not a mismatch to report later -- it is an unprotected
        // position right now.
        if stop.is_none() && self.position.is_some() {
            let detail = format!(
                "the protective stop {} is not on the exchange while a position is open",
                position.stop_order_id
            );
            Registry::global().count(
                RECONCILE_MISMATCHES,
                "reconciliation mismatches",
                &Labels::none(),
            );
            self.close_at_market(now, ExitTrigger::KillSwitch, "protection lost")
                .await?;
            return Ok(Some(LiveOutcome::ProtectionLost { detail }));
        }

        Ok(None)
    }

    /// Place an entry and its protective orders.
    async fn enter(
        &mut self,
        signal: EnterSignal,
        now: i64,
        candle: &Candle,
    ) -> Result<LiveOutcome, ExecutionError> {
        let reasons = signal.reasons.clone();
        Registry::global().count(SIGNALS_GENERATED, "signals produced", &Labels::none());

        if let RiskVerdict::Deny { limit, value } = self.risk.check_entry(
            signal.max_risk_pct,
            usize::from(self.position.is_some()),
            now,
        ) {
            if limit != "max_concurrent_positions" {
                Registry::global().count(RISK_BREACHES, "risk breaches", &Labels::none());
            }
            return Ok(LiveOutcome::EntryDenied {
                reasons,
                limit,
                value,
            });
        }

        let distance = (signal.reference_price - signal.stop_price).abs();
        if distance <= 0.0 {
            return Ok(LiveOutcome::EntryRefused {
                reason: "the stop is at the entry price".into(),
            });
        }

        // Fixed-fractional sizing, the same rule the simulator uses: risk a
        // percentage of equity over the distance to the stop.
        let risk_cash = self.equity * (signal.max_risk_pct / 100.0);
        let quantity = risk_cash / distance;
        if !quantity.is_finite() || quantity < self.min_quantity {
            return Ok(LiveOutcome::EntryRefused {
                reason: format!("{quantity} is below the {:.0} minimum", self.min_quantity),
            });
        }

        let side = OrderSide::from(signal.direction);
        let entry_id = client_order_id(&self.bot_id, &self.symbol, now, "entry");
        let ack = self
            .gateway
            .place(OrderRequest {
                client_order_id: entry_id.clone(),
                symbol: self.symbol.clone(),
                side,
                quantity,
                order_type: OrderType::Market,
            })
            .await?;

        let entry_price = ack.avg_price.unwrap_or(candle.close);
        Registry::global().count(ORDERS_EXECUTED, "orders acknowledged", &Labels::none());

        let stop_id = client_order_id(&self.bot_id, &self.symbol, now, "stop");
        self.gateway
            .place(OrderRequest {
                client_order_id: stop_id.clone(),
                symbol: self.symbol.clone(),
                side: side.opposite(),
                quantity,
                order_type: OrderType::StopMarket {
                    stop_price: signal.stop_price,
                },
            })
            .await?;

        let target_id = signal
            .take_profit_price
            .map(|_price| client_order_id(&self.bot_id, &self.symbol, now, "target"));
        if let (Some(price), Some(id)) = (signal.take_profit_price, target_id.clone()) {
            self.gateway
                .place(OrderRequest {
                    client_order_id: id,
                    symbol: self.symbol.clone(),
                    side: side.opposite(),
                    quantity,
                    order_type: OrderType::TakeProfitLimit {
                        stop_price: price,
                        price,
                    },
                })
                .await?;
        }

        self.position = Some(LivePosition {
            side,
            quantity,
            entry_price,
            stop_price: signal.stop_price,
            target_price: signal.take_profit_price,
            stop_order_id: stop_id,
            target_order_id: target_id,
            opened_at: now,
        });
        Registry::global().set_gauge(OPEN_POSITIONS, "open positions", &Labels::none(), 1.0);

        Ok(LiveOutcome::Entered {
            reasons,
            quantity,
            price: entry_price,
        })
    }

    /// Close because the strategy said to.
    async fn exit(
        &mut self,
        signal: ExitSignal,
        now: i64,
        _candle: &Candle,
    ) -> Result<LiveOutcome, ExecutionError> {
        let trigger = signal.trigger.name().to_string();
        self.close_at_market(now, signal.trigger, &trigger.clone())
            .await?;
        Ok(LiveOutcome::Exited { trigger })
    }

    /// Close at market and forget the position, cancelling protection first.
    async fn close_at_market(
        &mut self,
        now: i64,
        trigger: ExitTrigger,
        label: &str,
    ) -> Result<(), ExecutionError> {
        let Some(position) = self.position.clone() else {
            return Ok(());
        };

        // Cancel the protective orders first: leaving a stop behind after the
        // position it protects is gone is how an account ends up short.
        if let Err(error) = self.gateway.cancel(&position.stop_order_id).await {
            tracing::warn!(error = %error, "the protective stop could not be cancelled");
        }
        if let Some(id) = position.target_order_id.clone() {
            if let Err(error) = self.gateway.cancel(&id).await {
                tracing::warn!(error = %error, "the take-profit could not be cancelled");
            }
        }

        let ack = self
            .gateway
            .place(OrderRequest {
                client_order_id: client_order_id(&self.bot_id, &self.symbol, now, "close"),
                symbol: self.symbol.clone(),
                side: position.side.opposite(),
                quantity: position.quantity,
                order_type: OrderType::Market,
            })
            .await?;

        self.close(now, ack.avg_price.unwrap_or(position.entry_price), label)
            .await?;
        let _ = trigger;
        Ok(())
    }

    /// Book a closed position and hand the result to the risk engine.
    async fn close(
        &mut self,
        now: i64,
        exit_price: f64,
        trigger: &str,
    ) -> Result<(), ExecutionError> {
        let Some(position) = self.position.take() else {
            return Ok(());
        };
        let distance = position.risk_distance();
        let pnl = match position.side {
            OrderSide::Buy => exit_price - position.entry_price,
            OrderSide::Sell => position.entry_price - exit_price,
        };
        let r_multiple = if distance > 0.0 { pnl / distance } else { 0.0 };

        self.trades.push(LiveTrade {
            side: position.side,
            entry_price: position.entry_price,
            exit_price,
            quantity: position.quantity,
            stop_price: position.stop_price,
            target_price: position.target_price,
            r_multiple,
            opened_at: position.opened_at,
            closed_at: now,
            trigger: trigger.to_string(),
        });

        Registry::global().set_gauge(OPEN_POSITIONS, "open positions", &Labels::none(), 0.0);

        // A loss can trip the switch here; the error says so rather than
        // letting the bot continue into the next entry.
        if let Err(error) = self.risk.record_close(r_multiple, now) {
            Registry::global().count(RISK_BREACHES, "risk breaches", &Labels::none());
            return Err(error);
        }
        Ok(())
    }

    /// The position as the strategy likes to see it.
    ///
    /// The same shape the simulator hands the strategy -- a live bot and a
    /// paper bot must not differ in what the strategy can *see*, only in what
    /// a signal becomes. `unrealized_r` is measured against the current close,
    /// exactly as `Simulator::position_view` does.
    fn position_view(&self, price: f64) -> Option<PositionView> {
        let position = self.position.as_ref()?;
        let direction = match position.side {
            OrderSide::Buy => strategy_dsl::Direction::Long,
            OrderSide::Sell => strategy_dsl::Direction::Short,
        };
        let distance = position.risk_distance();
        let move_in_our_favour = match position.side {
            OrderSide::Buy => price - position.entry_price,
            OrderSide::Sell => position.entry_price - price,
        };
        Some(PositionView {
            direction,
            entry_price: position.entry_price,
            entry_time: position.opened_at,
            stop_price: position.stop_price,
            take_profit_price: position.target_price,
            size: position.quantity,
            bars_in_trade: 0,
            unrealized_r: if distance > 0.0 {
                move_in_our_favour / distance
            } else {
                0.0
            },
        })
    }

    fn record(&self, at: i64, price: f64, outcome: LiveOutcome) -> LiveRecord {
        LiveRecord {
            at,
            symbol: self.symbol.clone(),
            price,
            in_position: self.position.is_some(),
            outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{OrderAck, OrderStatusReport};
    use async_trait::async_trait;
    use std::sync::Mutex;

    const M5: i64 = 5 * 60 * 1_000_000_000;

    const DOCUMENT: &str = r#"
name: "Live harness"
version: "1"
kind: strategy
market: BTCUSDT
timeframes:
  trend: 1h
  entry: 5m
entry:
  direction: long
  all_of:
    - timeframe: trend
      condition: market_structure.trend == "bullish"
    - timeframe: entry
      condition: close > liquidity.swept_level
invalidation:
  - timeframe: entry
    condition: close_below(stop_price)
risk:
  max_risk_pct: 1.0
  stop: "below_sweep_low"
  take_profit:
    type: "risk_multiple"
    value: 2.0
"#;

    /// A venue that fills market orders at the current price and holds the
    /// rest.
    ///
    /// The price is mutable because a market order fills *near the market*, and
    /// a fixed fill price makes every R multiple meaningless: the first version
    /// of these tests filled a long entry at a hardcoded 100 while the stop sat
    /// wherever the fixture put it, and the stop test then reported +1R for a
    /// loss.
    struct Venue {
        price: Mutex<f64>,
        placed: Mutex<Vec<OrderRequest>>,
        open: Mutex<Vec<OrderStatusReport>>,
    }

    impl Venue {
        fn new(price: f64) -> Self {
            Self {
                price: Mutex::new(price),
                placed: Mutex::new(Vec::new()),
                open: Mutex::new(Vec::new()),
            }
        }

        fn set_price(&self, price: f64) {
            if let Ok(mut guard) = self.price.lock() {
                *guard = price;
            }
        }

        fn preload(&self, report: OrderStatusReport) {
            self.open.lock().unwrap().push(report);
        }
    }

    #[async_trait]
    impl ExchangeAdapter for Venue {
        fn venue(&self) -> &str {
            "mock"
        }

        async fn place_order(&self, order: OrderRequest) -> Result<OrderAck, ExecutionError> {
            let market = matches!(order.order_type, OrderType::Market);
            let price = *self.price.lock().unwrap();
            self.placed.lock().unwrap().push(order.clone());
            if market {
                return Ok(OrderAck {
                    client_order_id: order.client_order_id,
                    exchange_order_id: "E".into(),
                    status: OrderStatus::Filled,
                    filled_qty: order.quantity,
                    avg_price: Some(price),
                });
            }
            self.open.lock().unwrap().push(OrderStatusReport {
                client_order_id: order.client_order_id.clone(),
                exchange_order_id: "E".into(),
                status: OrderStatus::New,
                filled_qty: 0.0,
                avg_price: None,
            });
            Ok(OrderAck {
                client_order_id: order.client_order_id,
                exchange_order_id: "E".into(),
                status: OrderStatus::New,
                filled_qty: 0.0,
                avg_price: None,
            })
        }

        async fn cancel_order(&self, client_order_id: &str) -> Result<(), ExecutionError> {
            self.open
                .lock()
                .unwrap()
                .retain(|report| report.client_order_id != client_order_id);
            Ok(())
        }

        async fn reconcile(&self) -> Result<Vec<OrderStatusReport>, ExecutionError> {
            Ok(self.open.lock().unwrap().clone())
        }
    }

    /// The staircase the backtester's golden fixture uses, copied here on
    /// purpose.
    ///
    /// The first version of these tests drove a monotonic ramp, and every one
    /// of them failed for the same reason: a ramp has no sweeps and no
    /// reclaims, so `liquidity.swept_level` never resolves and the strategy
    /// never has anything to say. The tests were not wrong about the code, they
    /// were wrong about the market.
    const CYCLE: usize = 120;
    const DRIFT: f64 = 24.0;
    const KEYFRAMES: [(usize, f64); 6] = [
        (0, 0.0),
        (40, 20.0),
        (55, 8.0),
        (95, 34.0),
        (105, 4.0),
        (119, 24.0),
    ];

    fn offset(bar: usize) -> f64 {
        let position = bar % CYCLE;
        for window in KEYFRAMES.windows(2) {
            let ((from_bar, from), (to_bar, to)) = (window[0], window[1]);
            if position >= from_bar && position <= to_bar {
                let span = (to_bar - from_bar) as f64;
                return from + (to - from) * ((position - from_bar) as f64 / span);
            }
        }
        unreachable!()
    }

    fn price_at(bar: usize) -> f64 {
        100.0 + DRIFT * (bar / CYCLE) as f64 + offset(bar)
    }

    fn candle(bar: usize) -> Candle {
        let open = price_at(bar);
        let close = price_at(bar + 1);
        let wiggle = if bar % 2 == 0 { 0.3 } else { 0.6 };
        Candle {
            symbol: "BTCUSDT".into(),
            timeframe: Timeframe::M5,
            open_time: bar as i64 * M5,
            open,
            high: open.max(close) + wiggle,
            low: open.min(close) - wiggle,
            close,
            volume: 1_000.0,
            buy_volume: 700.0,
            sell_volume: 300.0,
        }
    }

    fn bot(venue: Venue) -> LiveBot<Venue> {
        bot_with_limits(venue, RiskLimits::default())
    }

    fn bot_with_limits(venue: Venue, limits: RiskLimits) -> LiveBot<Venue> {
        let validated = strategy_dsl::parse_and_validate(DOCUMENT).unwrap();
        let engine =
            StrategyEngine::new(&validated, strategy_runtime::RuntimeConfig::default()).unwrap();
        LiveBot::new(
            engine,
            venue,
            LiveConfig {
                symbol: "BTCUSDT".into(),
                bot_id: "testbot".into(),
                equity: 10_000.0,
                min_quantity: 0.0001,
                limits,
                ..LiveConfig::default()
            },
        )
    }

    /// Feed 5m candles plus the 1h candle whenever one closes, until the bot
    /// has had `bars` decision bars.
    async fn run(bot: &mut LiveBot<Venue>, bars: usize) -> Vec<LiveRecord> {
        let mut out = Vec::new();
        let candles: Vec<Candle> = (0..bars).map(candle).collect();
        for (bar, candle) in candles.iter().enumerate() {
            if bar >= 11 && (bar - 11) % 12 == 0 {
                let hourly = analytics_core::resample(&candles[..=bar], Timeframe::H1);
                if let Some(last) = hourly.last() {
                    if let Some(record) = bot.on_candle(last).await.expect("1h must not fail") {
                        out.push(record);
                    }
                }
            }
            // A market order fills near the market, so the venue's price tracks
            // the candle. Without this every R multiple in these tests is
            // arithmetic on unrelated numbers.
            bot.adapter_for_test().set_price(candle.close);
            if let Some(record) = bot.on_candle(candle).await.expect("5m must not fail") {
                out.push(record);
            }
        }
        out
    }

    #[tokio::test]
    async fn an_entry_places_the_order_and_its_protection() {
        let venue = Venue::new(100.0);
        let mut bot = bot(venue);
        let records = run(&mut bot, 400).await;

        let entered = records
            .iter()
            .any(|record| matches!(record.outcome, LiveOutcome::Entered { .. }));
        assert!(entered, "the fixture must produce a setup");

        let placed = bot.gateway_for_test().placed();
        assert!(
            placed.keys().any(|id| id.contains("_stop")),
            "a protective stop must be placed with the entry: {placed:?}"
        );
        assert!(bot.position().is_some(), "the position is open");
    }

    #[tokio::test]
    async fn the_concurrency_limit_is_enforced_before_an_order_is_placed() {
        // Configured to allow no position at all, so the check is exercised by
        // the *first* setup. Waiting for a second one made this test depend on
        // the fixture producing two setups in a row, which it does not.
        let venue = Venue::new(100.0);
        let mut bot = bot_with_limits(
            venue,
            RiskLimits {
                max_concurrent_positions: 0,
                ..RiskLimits::default()
            },
        );
        let records = run(&mut bot, 400).await;

        let denied = records.iter().any(|record| {
            matches!(
                &record.outcome,
                LiveOutcome::EntryDenied { limit, .. } if limit == "max_concurrent_positions"
            )
        });
        assert!(
            denied,
            "the risk engine must be consulted before an order is placed"
        );
        assert!(bot.position().is_none());
        assert!(
            bot.gateway_for_test().is_empty(),
            "a denied entry must not reach the venue"
        );
    }

    #[tokio::test]
    async fn a_filled_stop_closes_the_position_and_cancels_the_target() {
        let venue = Venue::new(100.0);
        let mut bot = bot_with_position(venue).await;

        // The venue now reports the stop filled at the stop price.
        let stop_id = bot.position().unwrap().stop_order_id.clone();
        let target_id = bot.position().unwrap().target_order_id.clone().unwrap();
        let quantity = bot.position().unwrap().quantity;
        let entry = bot.position().unwrap().entry_price;
        let stop = bot.position().unwrap().stop_price;
        bot.adapter_for_test().open.lock().unwrap().clear();
        bot.adapter_for_test().preload(OrderStatusReport {
            client_order_id: stop_id.clone(),
            exchange_order_id: "E".into(),
            status: OrderStatus::Filled,
            filled_qty: quantity,
            avg_price: Some(stop),
        });

        let record = bot
            .on_candle(&candle(999))
            .await
            .expect("decision")
            .expect("record");
        match &record.outcome {
            LiveOutcome::Closed {
                trigger,
                r_multiple,
            } => {
                assert_eq!(trigger, "stop");
                assert!(
                    (*r_multiple + 1.0).abs() < 1e-9,
                    "a stop at the stop price is -1R, got {r_multiple}"
                );
            }
            other => panic!("expected a close, got {other:?}"),
        }
        assert!(bot.position().is_none());
        assert_eq!(bot.trades().len(), 1);
        assert_eq!(bot.trades()[0].entry_price, entry);
        assert!(!bot
            .adapter_for_test()
            .open
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.client_order_id == target_id));
    }

    #[tokio::test]
    async fn a_missing_protective_stop_closes_the_position_immediately() {
        // The emergency path: an unprotected position is not a mismatch to
        // report later, it is a loss waiting to happen.
        let venue = Venue::new(100.0);
        let mut bot = bot_with_position(venue).await;
        bot.adapter_for_test().open.lock().unwrap().clear();

        let record = bot
            .on_candle(&candle(999))
            .await
            .expect("decision")
            .expect("record");
        match &record.outcome {
            LiveOutcome::ProtectionLost { detail } => {
                assert!(detail.contains("stop"), "{detail}");
            }
            other => panic!("expected protection lost, got {other:?}"),
        }
        assert!(
            bot.position().is_none(),
            "the position was closed at market"
        );
    }

    #[tokio::test]
    async fn a_halted_bot_closes_the_position_and_stops_asking() {
        let venue = Venue::new(100.0);
        let mut bot = bot_with_position(venue).await;
        bot.risk_mut().kill("operator pressed stop");

        let record = bot
            .on_candle(&candle(999))
            .await
            .expect("decision")
            .expect("record");
        assert!(
            matches!(record.outcome, LiveOutcome::Halted { .. }),
            "{:?}",
            record.outcome
        );
        assert!(bot.position().is_none(), "Close policy liquidates");
        assert!(bot.is_halted());
    }

    #[tokio::test]
    async fn a_bot_configured_to_hold_leaves_the_position_alone() {
        let venue = Venue::new(100.0);
        let mut bot = bot_with_position(venue).await;
        *bot.risk_mut() = RiskEngine::new(RiskLimits {
            on_breach: OnBreach::Hold,
            ..RiskLimits::default()
        });
        bot.risk_mut().kill("operator");

        let _ = bot.on_candle(&candle(999)).await.expect("decision");
        assert!(bot.position().is_some(), "Hold means the operator decides");
    }

    /// Build a bot that already holds a position, by running it over the
    /// fixture until it enters.
    async fn bot_with_position(venue: Venue) -> LiveBot<Venue> {
        let mut bot = bot(venue);
        for bar in 0..600 {
            if bar >= 11 && (bar - 11) % 12 == 0 {
                let candles: Vec<Candle> = (0..=bar).map(candle).collect();
                let hourly = analytics_core::resample(&candles, Timeframe::H1);
                if let Some(last) = hourly.last() {
                    let _ = bot.on_candle(last).await.expect("1h");
                }
            }
            let bar_candle = candle(bar);
            bot.adapter_for_test().set_price(bar_candle.close);
            let _ = bot.on_candle(&bar_candle).await.expect("5m");
            if bot.position().is_some() {
                break;
            }
        }
        assert!(
            bot.position().is_some(),
            "the fixture must open a position for this test to mean anything"
        );
        bot
    }
}
