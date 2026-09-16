//! HTTP instrumentation and the scrape endpoint (`docs/18`).
//!
//! ## Per route, not in aggregate
//!
//! `docs/18` asks for request rate, latency and error rate *per route*. A
//! global error rate hides the one broken endpoint: 2% of everything failing
//! is invisible, 100% of `/backtests` failing is an outage, and only the second
//! is actionable.
//!
//! ## The route label is `MatchedPath`, not the URI
//!
//! `/strategies/8bd3.../backtest` is a different string for every strategy.
//! Using the URI as a label would create one time series per id, which is the
//! classic cardinality blow-up that makes a Prometheus instance fall over.
//! `MatchedPath` gives the *template* -- `/strategies/{id}/backtest` -- so the
//! label set stays small and stable.

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use observability::metrics::{Labels, Registry, HTTP_LATENCY, HTTP_REQUESTS, MD_FEED_AGE};
use observability::{AlertSink, QueueSink};
use tracing::Instrument;

use crate::now_ns;
use crate::AppState;

/// Count and time one request.
///
/// Installed as a layer rather than called per handler, because a handler that
/// has to remember to instrument itself is a handler that will not.
pub async fn track(
    State(registry): State<Arc<Registry>>,
    request: Request,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |path| path.as_str().to_string());

    let request_id = observability::request_id();
    let span = tracing::info_span!(
        "http",
        request_id = %request_id,
        route = %route,
        method = %request.method()
    );

    let started = Instant::now();
    let response = next.run(request).instrument(span).await;
    let elapsed = started.elapsed().as_secs_f64();

    // `5xx` rather than the raw code: 404 and 500 are different events for the
    // same route, but 500 and 503 are the same event for an alert.
    let status = format!("{}xx", response.status().as_u16() / 100);
    let labels = Labels::new(&[("route", &route), ("status", &status)]);

    registry.count(HTTP_REQUESTS, "HTTP requests handled", &labels);
    registry.observe(
        HTTP_LATENCY,
        "HTTP request duration in seconds",
        &Labels::new(&[("route", &route)]),
        elapsed,
    );

    response
}

/// The Prometheus scrape endpoint.
pub async fn scrape(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        state.metrics.render(),
    )
}

/// How often the alert rules are evaluated.
pub const ALERT_INTERVAL: Duration = Duration::from_secs(30);

/// Sample every feed's age into the registry, as of `now`.
///
/// The only writer of `MD_FEED_AGE`, and it is a named function rather than two
/// lines inside the alert loop so a test can drive the *same* code the alert
/// task drives. The alternative -- a test that sets the gauge by hand -- proves
/// the rule works and says nothing about whether anything ever sets the gauge,
/// which is exactly the gap that let this metric go unwritten.
///
/// `now` is a parameter rather than a call to `now_ns()` inside, so a test can
/// age a feed without sleeping for ten minutes. Production passes the clock.
///
/// Sampled here rather than published by the collector because the age is a
/// function of *now*: a value published at publish-time is already wrong by the
/// time the rule reads it, and wrong by an amount that grows with the scrape
/// interval. The number the rule sees should be the number the rule is about.
///
/// Labelled per symbol. A single global age would report whichever feed was
/// written last, so one quiet market would hide behind another that is healthy.
pub fn publish_feed_ages(supervisor: &crate::bots::BotSupervisor, registry: &Registry, now: i64) {
    for (symbol, age) in supervisor.feed_ages(now) {
        registry.set_gauge(
            MD_FEED_AGE,
            "Seconds since the most recent candle for a symbol",
            &Labels::new(&[("symbol", &symbol)]),
            age,
        );
    }
}

/// Evaluate the alert rules forever, writing what fires to the audit trail.
///
/// The audit write is what makes an alert durable: `docs/18` wants an
/// on-call engineer to work from dashboards, but the *record* of an alert
/// belongs in the same append-only trail as the trade decisions it is about,
/// so "the kill-switch tripped at 03:00" survives a metrics restart.
pub fn spawn_alert_task(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut alerter = observability::Alerter::new(observability::default_rules());
        let mut ticker = tokio::time::interval(ALERT_INTERVAL);
        // The first tick completes immediately, which would evaluate a registry
        // that has not recorded anything yet and is therefore fine but useless.
        ticker.tick().await;

        loop {
            ticker.tick().await;

            // Refresh the feed age *before* the rules read it.
            //
            // Without this the `StaleFeed` rule read a metric nothing wrote, so
            // it could never fire -- a runbook in `docs/20` for an alert that
            // was structurally incapable of being raised.
            publish_feed_ages(&state.bots, &state.metrics, now_ns());

            let alerts = alerter.evaluate(&state.metrics);
            for alert in &alerts {
                if let Some(db) = &state.db {
                    let payload = serde_json::json!({
                        "alert": alert.name,
                        "severity": alert.severity.as_str(),
                        "detail": alert.detail,
                        "at_ms": alert.at_ms,
                    });
                    if let Err(error) = db::paper::insert_audit_events(
                        db.pool(),
                        &[db::paper::AuditEvent {
                            user_id: None,
                            event_type: crate::ALERT_EVENT.into(),
                            payload,
                            ts: alert.at_ms * 1_000_000,
                        }],
                    )
                    .await
                    {
                        tracing::warn!(alert = %alert.name, error = %error, "alert not written to the audit trail");
                    }
                }
                if let Some(queue) = &state.alert_queue {
                    queue.send(alert);
                }
            }
        }
    })
}

/// Whether the process is configured to emit JSON logs.
#[must_use]
pub fn json_logs() -> bool {
    std::env::var(observability::LOG_FORMAT_ENV)
        .map(|value| value.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

/// Drain the alert queue to a webhook, forever.
///
/// The queue exists so this task's failures cannot reach the trading loop: if
/// the webhook hangs, alerts accumulate in memory and are logged, and nothing
/// else in the platform slows down or notices.
pub fn spawn_webhook_task(url: String, queue: Arc<QueueSink>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let mut ticker = tokio::time::interval(Duration::from_secs(5));

        loop {
            ticker.tick().await;
            for alert in queue.drain() {
                let body = serde_json::json!({
                    "alert": alert.name,
                    "severity": alert.severity.as_str(),
                    "detail": alert.detail,
                    "at_ms": alert.at_ms,
                    "service": observability::service(),
                });
                match client.post(&url).json(&body).send().await {
                    Ok(response) if response.status().is_success() => {}
                    Ok(response) => tracing::warn!(
                        status = %response.status(),
                        alert = %alert.name,
                        "alert webhook rejected the delivery"
                    ),
                    Err(error) => tracing::warn!(
                        error = %error,
                        alert = %alert.name,
                        "alert webhook delivery failed"
                    ),
                }
            }
        }
    })
}
