//! # `observability`
//!
//! Metrics, alerting and structured logging for every service
//! (`docs/18-OBSERVABILITY.md`), built as Phase 8's first slice.
//!
//! ## Why this is a crate and not a module in `api-gateway`
//!
//! `docs/18` specifies metrics per *service*: market data, the agent, the
//! backtester, the trading engine and the gateway each export their own. A
//! module inside the gateway could not be used by the trading engine, because
//! `docs/03` forbids that dependency direction -- so the alternative was five
//! copies, which would drift exactly the way four strategy implementations
//! would.
//!
//! ## What it deliberately does not do
//!
//! * **No Prometheus push.** There is no Prometheus server in this stack, so
//!   the registry renders the scrape format and `/metrics` serves it. Adding a
//!   push gateway would be a deployment decision, not an observability one.
//! * **No network I/O in a sink.** Rule evaluation runs on whichever task owns
//!   the loop, including the trading loop. A sink that did HTTP could let a
//!   hanging webhook delay an order, so [`QueueSink`] queues and the service's
//!   own task delivers.
//! * **No credentials in logs.** `docs/15` forbids it, and redaction is the
//!   caller's job at the point of formatting -- this crate never sees a key.
//!
//! [`QueueSink`]: alerts::QueueSink

#![deny(missing_docs)]

pub mod alerts;
pub mod logging;
pub mod metrics;

pub use alerts::{
    default_rules, Alert, AlertSink, Alerter, CollectingSink, LogSink, QueueSink, Rule, Severity,
};
pub use logging::{init_logging, request_id, service, LOG_FORMAT_ENV};
pub use metrics::{Kind, Labels, Registry, Sample};
