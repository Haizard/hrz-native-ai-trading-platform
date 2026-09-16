//! # `trading-engine`
//!
//! Runs an approved `StrategyDocument` continuously against live market data --
//! first simulated (Phase 6), later with real orders (Phase 8) -- always through
//! the same `strategy-runtime`/sandbox path the backtester uses.
//!
//! ## Phases
//!
//! * Phase 6 -- paper trading: subscribe to live `MarketState`, feed closed
//!   candles into the sandboxed strategy, simulate fills, persist every
//!   decision (including "no signal") for audit.
//! * Phase 8 -- live trading, gated behind a paper track record, explicit
//!   per-venue opt-in and active risk limits.
//!
//! ## Risk is always on
//!
//! Per-trade risk cap clamped to the platform ceiling, per-account daily/weekly
//! loss limits, max concurrent positions, and a kill-switch that must work even
//! when the agent or market-data engine is degraded
//! (`docs/15-RISK-COMPLIANCE.md`).
//!
//! Credentials live here, never in the sandbox: order placement happens outside
//! the sandbox, driven only by the sandbox's `Signal` output.
//!
//! ## Status
//!
//! Phase 6: paper trading. The risk engine, the simulated executor and the
//! decision audit trail are implemented and tested. The live runner -- 48
//! hours against a real feed -- is the remaining piece, and it is a scheduling
//! problem rather than a missing component: [`PaperBot::on_candle`] is the
//! whole loop, and a runner only has to feed it.
//!
//! [`PaperBot::on_candle`]: paper::PaperBot::on_candle

#![deny(missing_docs)]

pub mod binance;
pub mod credentials;
pub mod error;
pub mod execution;
pub mod gate;
pub mod live;
pub mod live_store;
pub mod paper;
pub mod risk;
pub mod store;

pub use binance::BinanceRest;
pub use credentials::ExchangeCredentials;
pub use error::ExecutionError;
pub use execution::{
    client_order_id, ExchangeAdapter, Mismatch, MismatchKind, OrderAck, OrderGateway, OrderRequest,
    OrderSide, OrderStatus, OrderStatusReport, OrderType, Reconciliation,
};
pub use gate::{GateRequirements, GateVerdict, LiveGate, TrackRecord};
pub use live::{LiveBot, LiveConfig, LiveOutcome, LivePosition, LiveRecord, LiveTrade};
pub use live_store::{
    live_decision_payload, LiveSession, LIVE_DECISION_EVENT, LIVE_ORDER_EVENT, RECONCILE_EVENT,
};
pub use paper::{BotAlert, DecisionOutcome, DecisionRecord, PaperBot, PaperConfig};
pub use risk::{OnBreach, RiskEngine, RiskLimits, RiskVerdict, PLATFORM_MAX_RISK_PCT};
pub use store::{
    decision_payload, notification_payload, BotSession, DECISION_EVENT, NOTIFICATION_EVENT,
    RISK_EVENT, STARTED_EVENT, STOPPED_EVENT,
};
