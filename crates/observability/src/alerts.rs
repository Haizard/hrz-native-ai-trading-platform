//! Alert rules evaluated against the registry (`docs/18`).
//!
//! ## Fire on transition, not on every tick
//!
//! An alert that re-fires every 30 seconds is noise, and noise is why alerting
//! gets muted. Each rule fires once when it enters breach and once more when it
//! clears, and stays quiet in between -- so a page means "this changed".
//!
//! ## The rules are an enum, not closures
//!
//! A rule has to be nameable to be deduplicated, loggable and tested. A boxed
//! closure can be executed but not inspected, and the first thing an on-call
//! engineer needs is the rule's name.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::metrics::{
    Registry, HTTP_REQUESTS, KILL_SWITCH, MD_BOOK_AGE, MD_FEED_AGE, RECONCILE_MISMATCHES,
    RISK_BREACHES, WS_OPENS,
};

/// How urgent an alert is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something cleared. Recorded, not paged.
    Info,
    /// Degraded, not stopped.
    Warning,
    /// Real money, or the system is not doing its job. Page.
    Critical,
}

impl Severity {
    /// The wire word, for logs and payloads that are read by scripts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// One alert: what fired, how bad, and what it saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// The rule's stable name, e.g. `stale_market_data`.
    pub name: String,
    /// Urgency.
    pub severity: Severity,
    /// What the metrics said, as text an engineer can read without a dashboard.
    pub detail: String,
    /// When it fired, unix milliseconds.
    pub at_ms: i64,
}

/// Where alerts go.
pub trait AlertSink: Send + Sync {
    /// Deliver one alert. Must not block: this runs on whatever task evaluated
    /// the rules, and a slow sink must not delay a trading decision.
    fn send(&self, alert: &Alert);
}

/// Write every alert to the structured log.
///
/// Always installed, because an alert nobody receives is not an alert, and the
/// log is the one sink that cannot fail to be configured.
pub struct LogSink;

impl AlertSink for LogSink {
    fn send(&self, alert: &Alert) {
        tracing::error!(
            alert = %alert.name,
            severity = alert.severity.as_str(),
            detail = %alert.detail,
            "alert"
        );
    }
}

/// Keep alerts in memory, for tests and for the gateway to serve.
#[derive(Debug, Default)]
pub struct CollectingSink {
    alerts: Mutex<Vec<Alert>>,
}

impl CollectingSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything received so far.
    #[must_use]
    pub fn alerts(&self) -> Vec<Alert> {
        self.alerts.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |guard| guard.clone(),
        )
    }
}

impl AlertSink for CollectingSink {
    fn send(&self, alert: &Alert) {
        if let Ok(mut guard) = self.alerts.lock() {
            guard.push(alert.clone());
        }
    }
}

/// A queue a delivery task drains.
///
/// The split matters: rule evaluation is synchronous and must not do network
/// I/O, so a webhook becomes "put it in the queue" here and "POST it" in the
/// gateway's task. A sink that did its own HTTP would let a hanging webhook
/// slow the trading loop.
#[derive(Debug, Default)]
pub struct QueueSink {
    queue: Mutex<VecDeque<Alert>>,
}

impl QueueSink {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Take everything queued, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<Alert> {
        self.queue.lock().map_or_else(
            |poisoned| poisoned.into_inner().drain(..).collect(),
            |mut guard| guard.drain(..).collect(),
        )
    }
}

impl AlertSink for QueueSink {
    fn send(&self, alert: &Alert) {
        if let Ok(mut guard) = self.queue.lock() {
            guard.push_back(alert.clone());
        }
    }
}

/// A rule the alerter evaluates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// Market data is older than this many seconds.
    StaleFeed {
        /// Threshold.
        max_age_secs: f64,
    },
    /// No order book has arrived for a symbol in this many seconds.
    ///
    /// `StaleFeed` cannot see this. Candles come from a different aggregation
    /// than the book, so a run can publish a perfect candle series while the
    /// book has never bridged onto its snapshot -- which is precisely what
    /// happened in `docs/19` row 24: healthy candles, empty DOM, nothing
    /// anywhere saying so.
    StaleBook {
        /// Threshold.
        max_age_secs: f64,
    },
    /// 5xx responses as a fraction of requests to one route, once the route has
    /// served enough requests for the ratio to mean anything.
    ErrorRate {
        /// Minimum requests before the rule is allowed to fire.
        min_requests: u64,
        /// Maximum tolerated fraction of 5xx.
        max_ratio: f64,
    },
    /// WebSocket connections are being opened faster than this, and it is not
    /// stopping.
    ///
    /// A client in a reconnect loop and fifty clients arriving normally look
    /// identical to the gauge -- both leave `websocket_connections` where it is
    /// -- so this reads the *counter* instead, over time. The rate is what
    /// separates the two: normal traffic is bursty and then quiet, a loop is
    /// the same rate tick after tick.
    SocketChurn {
        /// Openings per minute above which something is reconnecting in a loop.
        max_opens_per_min: f64,
        /// Consecutive evaluations that must breach before it is news.
        ///
        /// One tick of a lot of opens is a dashboard loading, or every client
        /// reconnecting after a restart -- both expected. Two ticks is a minute
        /// of it, which nothing normal does.
        sustained_rounds: u32,
    },
    /// The kill-switch tripped.
    KillSwitch,
    /// A risk limit was breached.
    RiskBreach,
    /// Our view of orders disagrees with the exchange's.
    ReconcileMismatch,
}

impl Rule {
    /// The stable name used for dedupe, logs and dashboards.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::StaleFeed { .. } => "stale_market_data",
            Self::StaleBook { .. } => "stale_order_book",
            Self::ErrorRate { .. } => "http_error_rate",
            Self::SocketChurn { .. } => "socket_churn",
            Self::KillSwitch => "kill_switch_engaged",
            Self::RiskBreach => "risk_limit_breached",
            Self::ReconcileMismatch => "reconcile_mismatch",
        }
    }

    /// Whether this rule is about *events* rather than *levels*.
    ///
    /// A level rule (feed age, error rate) has a state that persists, so it is
    /// announced once on entering breach and once on leaving it. An event rule
    /// has no level to leave: a kill-switch activation is news every time it
    /// happens, and "none since the last tick" is the healthy case rather than
    /// a resolution. Treating the two alike would mean either a re-page every
    /// 30 seconds for a still-stale feed, or silence for the second bot that
    /// breached a limit this hour -- and both are wrong in the same way an
    /// on-call engineer would notice.
    #[must_use]
    pub const fn is_event(&self) -> bool {
        matches!(
            self,
            Self::KillSwitch | Self::RiskBreach | Self::ReconcileMismatch
        )
    }

    /// How bad a firing of this rule is.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        match self {
            Self::StaleFeed { .. } | Self::StaleBook { .. } | Self::ErrorRate { .. }
            | Self::SocketChurn { .. } => Severity::Warning,
            Self::KillSwitch | Self::RiskBreach | Self::ReconcileMismatch => Severity::Critical,
        }
    }
}

/// The rules `docs/18` asks for, with the thresholds it implies.
///
/// ## The rule that is deliberately absent
///
/// `docs/18` also names backtest-to-live divergence. There is no rule for it
/// here, because nothing in the platform measures it yet -- `docs/19` carries
/// it as debt, and the shape it needs is a job that compares a bot's realised R
/// against its backtest over the same window, not a threshold over a metric
/// that no one writes.
///
/// The first version of this list *did* include `Rule::Divergence`, over a
/// `DIVERGENCE_R` gauge with no writer. It could never fire, and that is worse
/// than not having it: an operator reading the alert list, or a runbook in
/// `docs/20`, would conclude that divergence was monitored. A missing rule is a
/// visible gap; an inert one is a false assurance.
#[must_use]
pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule::StaleFeed {
            max_age_secs: 120.0,
        },
        // Tighter than the feed's 120s, and deliberately so: a book is
        // republished every `orderbook_publish_ms` while it is healthy, so
        // 60 seconds of silence is not "a quiet market" -- it is a stream that
        // stopped, or one that never bridged at all. The 2s resync means a
        // book that *can* recover does so well inside this window, so the rule
        // fires on the ones that cannot.
        Rule::StaleBook {
            max_age_secs: 60.0,
        },
        Rule::ErrorRate {
            min_requests: 50,
            max_ratio: 0.05,
        },
        // 20/min sustained over two checks, and the two numbers are not
        // independent: rules are evaluated every 30s, so this is "more than ten
        // new sockets per check, for a full minute".
        //
        // Ten was chosen against the traffic this gateway actually sees. A
        // dashboard opening is six to eight sockets at once -- one round, well
        // under, and it does not repeat. A restart reopens every client's
        // sockets in a single round, which is exactly what `sustained_rounds`
        // exists to absorb. A client retrying every five seconds is twelve a
        // minute, round after round, and that is the thing being looked for.
        // Raise `max_opens_per_min` as the number of concurrent dashboards
        // grows; the rule is a field, not a constant, for that reason.
        Rule::SocketChurn {
            max_opens_per_min: 20.0,
            sustained_rounds: 2,
        },
        Rule::KillSwitch,
        Rule::RiskBreach,
        Rule::ReconcileMismatch,
    ]
}

/// Evaluates rules against a registry and dedupes what it sends.
pub struct Alerter {
    rules: Vec<Rule>,
    sinks: Vec<Arc<dyn AlertSink>>,
    /// Rules currently in breach, so a rule is announced once per transition.
    active: HashSet<String>,
    /// Symbols whose market feed was deliberately closed and must not alert.
    ///
    /// Closing a feed does not stop the age gauges that already exist from
    /// reading as if the symbol's data had *died* -- the gauge freezes at an
    /// ever-growing age and the rule pages. A reclaimed feed is an operator
    /// decision, not an outage, so the reaper records the symbol here and the
    /// stale rules refuse to report it until a feed is opened for it again.
    /// Cleared by the same writer that opens the feed, so the mask cannot go
    /// stale in the other direction either.
    excluded_symbols: HashSet<String>,
    /// Last seen value per counter rule, so "it went up" is detectable without
    /// the caller having to remember anything.
    last: HashMap<String, u64>,
    /// When each of those values was seen, in milliseconds.
    ///
    /// A value alone cannot produce a *rate*: five new sockets is alarming if
    /// it happened in a second and unremarkable if it took an hour. Only the
    /// rules that need a rate read this, and the rest leave it empty rather
    /// than every rule carrying a clock it does not use.
    last_at: HashMap<String, i64>,
    /// Consecutive evaluations a rule has breached, for rules that must be
    /// sustained before they are news.
    streak: HashMap<String, u32>,
}

impl std::fmt::Debug for Alerter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Alerter")
            .field("rules", &self.rules)
            .field("sinks", &self.sinks.len())
            .field("active", &self.active)
            .field("last", &self.last)
            .field("last_at", &self.last_at)
            .field("streak", &self.streak)
            .finish()
    }
}

impl Alerter {
    /// Build an alerter with a log sink always attached.
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self::with_sinks(rules, vec![Arc::new(LogSink)])
    }

    /// Build an alerter with explicit sinks.
    #[must_use]
    pub fn with_sinks(rules: Vec<Rule>, sinks: Vec<Arc<dyn AlertSink>>) -> Self {
        Self {
            rules,
            sinks,
            active: HashSet::new(),
            excluded_symbols: HashSet::new(),
            last: HashMap::new(),
            last_at: HashMap::new(),
            streak: HashMap::new(),
        }
    }

    /// The rules in force.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Evaluate every rule once and deliver whatever changed.
    ///
    /// Returns the alerts raised, so a caller can persist them to the audit
    /// trail without having to install a sink that knows about the database.
    #[must_use]
    pub fn evaluate(&mut self, registry: &Registry) -> Vec<Alert> {
        self.evaluate_at(registry, now_ms())
    }

    /// Evaluate every rule as of an explicit clock reading.
    ///
    /// The rules that read a *rate* cannot be tested against the wall clock: two
    /// evaluations in a test land microseconds apart, which is not an interval
    /// anyone can assert on. This exists so a test can pass the time itself,
    /// rather than sleeping for a minute to make the clock say what it wants.
    #[must_use]
    pub fn evaluate_at(&mut self, registry: &Registry, now: i64) -> Vec<Alert> {
        let samples = registry.snapshot();
        let raised_at = now;
        let mut raised = Vec::new();

        for rule in self.rules.clone() {
            let excluded = |symbol: &str| self.excluded_symbols.contains(symbol);
            let breach = Self::breach(
                &rule,
                &samples,
                &mut self.last,
                &mut self.last_at,
                &mut self.streak,
                raised_at,
                &excluded,
            );
            let name = rule.name().to_string();

            if rule.is_event() {
                // Every activation is its own alert; there is no "cleared".
                if let Some(detail) = breach {
                    raised.push(Alert {
                        name,
                        severity: rule.severity(),
                        detail,
                        at_ms: now_ms(),
                    });
                }
                continue;
            }

            let is_active = self.active.contains(&name);
            match (breach, is_active) {
                (Some(detail), false) => {
                    self.active.insert(name.clone());
                    raised.push(Alert {
                        name,
                        severity: rule.severity(),
                        detail,
                        at_ms: now_ms(),
                    });
                }
                (None, true) => {
                    self.active.remove(&name);
                    raised.push(Alert {
                        name,
                        severity: Severity::Info,
                        detail: "resolved".into(),
                        at_ms: now_ms(),
                    });
                }
                // Firing while firing, and quiet while quiet, are both silence.
                (Some(_), true) | (None, false) => {}
            }
        }

        for alert in &raised {
            for sink in &self.sinks {
                sink.send(alert);
            }
        }
        raised
    }

    /// Whether this rule is currently in breach.
    #[must_use]
    pub fn is_active(&self, rule: &str) -> bool {
        self.active.contains(rule)
    }

    /// Stop the market-data rules from reporting `symbol`.
    ///
    /// Called when a symbol's feed is closed on purpose (idle reclaim, ceiling
    /// eviction). If a symbol later gets a feed again, [`Self::watch_symbol`]
    /// clears the exclusion -- the mask follows the feed, not the config.
    pub fn exclude_symbol(&mut self, symbol: &str) {
        self.excluded_symbols.insert(symbol.to_uppercase());
        // A symbol that just lost its feed cannot still be "in breach": if the
        // rule was active, the next evaluation's transition logic needs to see
        // it leave cleanly rather than firing a resolution for a symbol that is
        // now simply unwatched.
        for rule in &self.rules {
            if !rule.is_event() {
                self.active.remove(rule.name());
            }
        }
    }

    /// Clear a symbol's exclusion, when a feed for it opens again.
    pub fn watch_symbol(&mut self, symbol: &str) {
        self.excluded_symbols.remove(&symbol.to_uppercase());
    }

    /// Whether `symbol` is currently excluded from the market-data rules.
    #[must_use]
    pub fn is_excluded(&self, symbol: &str) -> bool {
        self.excluded_symbols.contains(&symbol.to_uppercase())
    }

    /// Evaluate one rule against the current samples.
    fn breach(
        rule: &Rule,
        samples: &[crate::metrics::Sample],
        last: &mut HashMap<String, u64>,
        last_at: &mut HashMap<String, i64>,
        streak: &mut HashMap<String, u32>,
        now: i64,
        excluded: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        match *rule {
            Rule::StaleFeed { max_age_secs } => {
                let mut worst: Option<(f64, String)> = None;
                for sample in samples.iter().filter(|s| s.name == MD_FEED_AGE) {
                    // A symbol whose feed was closed on purpose has an age
                    // gauge that grows forever; that is the mask's job to name,
                    // not the rule's to page.
                    if excluded(&symbol_of(sample)) {
                        continue;
                    }
                    if sample.value > max_age_secs
                        && worst
                            .as_ref()
                            .is_none_or(|(value, _)| sample.value > *value)
                    {
                        worst = Some((sample.value, sample.labels.render()));
                    }
                }
                worst.map(|(value, labels)| {
                    format!("newest candle is {value:.0}s old (limit {max_age_secs:.0}s) {labels}")
                })
            }
            Rule::StaleBook { max_age_secs } => {
                let mut worst: Option<(f64, String)> = None;
                for sample in samples.iter().filter(|s| s.name == MD_BOOK_AGE) {
                    if excluded(&symbol_of(sample)) {
                        continue;
                    }
                    if sample.value > max_age_secs
                        && worst
                            .as_ref()
                            .is_none_or(|(value, _)| sample.value > *value)
                    {
                        worst = Some((sample.value, sample.labels.render()));
                    }
                }
                worst.map(|(value, labels)| {
                    format!(
                        "no order book for {value:.0}s (limit {max_age_secs:.0}s) {labels}: \
                         the depth stream has stopped, or never bridged onto its snapshot"
                    )
                })
            }
            Rule::ErrorRate {
                min_requests,
                max_ratio,
            } => {
                // Grouped by route: a single bad route is a bug, not an outage,
                // and the distinction is invisible if the ratio is global.
                let mut totals: HashMap<String, (u64, u64)> = HashMap::new();
                for sample in samples.iter().filter(|s| s.name == HTTP_REQUESTS) {
                    let route = label_of(sample, "route");
                    let status = label_of(sample, "status");
                    let entry = totals.entry(route).or_insert((0, 0));
                    entry.0 += sample.value as u64;
                    if status.starts_with('5') {
                        entry.1 += sample.value as u64;
                    }
                }
                let mut breaches: Vec<(String, f64, u64)> = totals
                    .into_iter()
                    .filter(|(_, (total, _))| *total >= min_requests)
                    .map(|(route, (total, errors))| (route, errors as f64 / total as f64, total))
                    .filter(|(_, ratio, _)| *ratio > max_ratio)
                    .collect();
                breaches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                breaches.into_iter().next().map(|(route, ratio, total)| {
                    format!("{route}: {:.1}% of {total} requests are 5xx", ratio * 100.0)
                })
            }
            Rule::SocketChurn {
                max_opens_per_min,
                sustained_rounds,
            } => {
                let rate = churn_rate(rule, samples, last, last_at, now)?;
                let key = rule.name().to_string();
                if rate <= max_opens_per_min {
                    // Not sustained: the count resets, so a burst has to start
                    // again from the first round before it can be news.
                    streak.insert(key, 0);
                    return None;
                }
                let rounds = streak.entry(key).or_insert(0);
                *rounds += 1;
                let rounds = *rounds;
                (rounds >= sustained_rounds).then(move || {
                    format!(
                        "{rate:.0} sockets/min are opening (limit {max_opens_per_min:.0}), \
                         sustained across {rounds} checks: something is reconnecting in a loop"
                    )
                })
            }
            Rule::KillSwitch => counter_rose(rule, KILL_SWITCH, samples, last).map(|delta| {
                format!("the kill-switch engaged ({delta} activation(s)); no new entries")
            }),
            Rule::RiskBreach => counter_rose(rule, RISK_BREACHES, samples, last)
                .map(|delta| format!("{delta} risk-limit breach(es) recorded")),
            Rule::ReconcileMismatch => counter_rose(rule, RECONCILE_MISMATCHES, samples, last)
                .map(|delta| format!("{delta} order(s) disagree with the exchange")),
        }
    }
}

/// How much a counter moved since the last evaluation.
fn counter_rose(
    rule: &Rule,
    name: &str,
    samples: &[crate::metrics::Sample],
    last: &mut HashMap<String, u64>,
) -> Option<u64> {
    let total: u64 = samples
        .iter()
        .filter(|s| s.name == name)
        .map(|s| s.value as u64)
        .sum();
    let key = rule.name().to_string();
    let previous = last.insert(key, total).unwrap_or(0);
    (total > previous).then_some(total - previous)
}

/// WebSocket openings per minute since the previous evaluation.
///
/// `None` on the first evaluation, deliberately: there is no previous reading to
/// difference against, and treating "everything opened since boot" as one
/// minute of traffic would make the gateway alert on its own startup.
fn churn_rate(
    rule: &Rule,
    samples: &[crate::metrics::Sample],
    last: &mut HashMap<String, u64>,
    last_at: &mut HashMap<String, i64>,
    now_ms: i64,
) -> Option<f64> {
    let key = rule.name().to_string();
    // All channels together: the loop being looked for reopens the same sockets
    // over and over, and a per-channel rate would only ever see a fraction of
    // it. The detail a human wants (which channel) is in the log line.
    let total: u64 = samples
        .iter()
        .filter(|s| s.name == WS_OPENS)
        .map(|s| s.value as u64)
        .sum();

    let previous = last.insert(key.clone(), total);
    let previous_at = last_at.insert(key, now_ms);
    let (Some(previous), Some(previous_at)) = (previous, previous_at) else {
        return None;
    };

    let elapsed_ms = now_ms.saturating_sub(previous_at);
    // Below a second, the rate is an artefact of the clock rather than a
    // measurement: two evaluations in the same millisecond would otherwise
    // report infinity. A counter that did not move is no rate at all.
    if elapsed_ms < 1_000 || total <= previous {
        return None;
    }

    Some((total - previous) as f64 * 60_000.0 / elapsed_ms as f64)
}

/// A sample's `symbol` label, if any (empty when the metric is not per-symbol).
fn symbol_of(sample: &crate::metrics::Sample) -> String {
    label_of(sample, "symbol")
}

/// Read one label out of a sample, defaulting to an empty string.
fn label_of(sample: &crate::metrics::Sample, key: &str) -> String {
    sample
        .labels
        .render()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let name = parts.next()?.trim();
            let value = parts.next()?.trim().trim_matches('"');
            (name == key).then(|| value.to_string())
        })
        .unwrap_or_default()
}

/// Now, in unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Labels;

    fn alerter(rules: Vec<Rule>) -> (Alerter, Arc<CollectingSink>) {
        let sink = Arc::new(CollectingSink::new());
        let alerter = Alerter::with_sinks(rules, vec![Arc::new(LogSink), sink.clone()]);
        (alerter, sink)
    }

    #[test]
    fn a_stale_feed_fires_once_and_not_again_on_the_next_tick() {
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![Rule::StaleFeed { max_age_secs: 60.0 }]);
        registry.set_gauge(MD_FEED_AGE, "age", &Labels::none(), 500.0);

        let first = alerter.evaluate(&registry);
        assert_eq!(first.len(), 1, "the first breach must be announced");
        assert_eq!(first[0].severity, Severity::Warning);

        let second = alerter.evaluate(&registry);
        assert!(
            second.is_empty(),
            "an unchanged breach must stay quiet, or alerting becomes noise"
        );
        assert_eq!(sink.alerts().len(), 1);
    }

    #[test]
    fn a_cleared_rule_says_so() {
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![Rule::StaleFeed { max_age_secs: 60.0 }]);
        registry.set_gauge(MD_FEED_AGE, "age", &Labels::none(), 500.0);
        let _ = alerter.evaluate(&registry);

        registry.set_gauge(MD_FEED_AGE, "age", &Labels::none(), 1.0);
        let cleared = alerter.evaluate(&registry);

        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].severity, Severity::Info);
        assert_eq!(cleared[0].detail, "resolved");
        assert!(!alerter.is_active("stale_market_data"));
        assert_eq!(sink.alerts().len(), 2);
    }

    #[test]
    fn an_excluded_symbol_never_fires_the_stale_rules() {
        // The 0GTRY incident: the reaper closed an idle feed, the symbol's age
        // gauge froze and grew, and a minute later the rule paged on a symbol
        // nobody was watching. The mask is what makes "closed on purpose"
        // different from "dead".
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![
            Rule::StaleFeed { max_age_secs: 60.0 },
            Rule::StaleBook { max_age_secs: 60.0 },
        ]);
        registry.set_gauge(
            MD_FEED_AGE,
            "age",
            &Labels::new(&[("symbol", "0GTRY")]),
            500.0,
        );
        registry.set_gauge(
            MD_BOOK_AGE,
            "age",
            &Labels::new(&[("symbol", "0GTRY")]),
            500.0,
        );

        alerter.exclude_symbol("0GTRY");
        assert!(alerter.is_excluded("0GTRY"));
        assert!(
            alerter.evaluate(&registry).is_empty(),
            "a symbol whose feed was closed on purpose must not page"
        );
        assert_eq!(sink.alerts().len(), 0);

        // The same gauge value pages the moment a feed opens again.
        alerter.watch_symbol("0GTRY");
        assert!(!alerter.is_excluded("0GTRY"));
        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 2, "both stale rules must see the breach again");
    }

    #[test]
    fn an_excluded_symbol_does_not_hide_a_genuinely_stale_one() {
        // The mask skips a symbol; it must not silence the rule. A second,
        // unwatched symbol that is genuinely stale still has to page.
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::StaleFeed { max_age_secs: 60.0 }]);
        registry.set_gauge(
            MD_FEED_AGE,
            "age",
            &Labels::new(&[("symbol", "RECLAIMED")]),
            900.0,
        );
        registry.set_gauge(
            MD_FEED_AGE,
            "age",
            &Labels::new(&[("symbol", "BTCUSDT")]),
            200.0,
        );

        alerter.exclude_symbol("RECLAIMED");
        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 1);
        assert!(
            raised[0].detail.contains("BTCUSDT"),
            "the surviving breach must name the live symbol, not the excluded one: {}",
            raised[0].detail
        );
    }

    /// A gateway that has just started has opened every socket it will open in
    /// its first minute, and that is not churn.
    #[test]
    fn churn_says_nothing_on_the_first_evaluation() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::SocketChurn {
            max_opens_per_min: 20.0,
            sustained_rounds: 2,
        }]);
        registry.inc_counter(WS_OPENS, "opens", &Labels::new(&[("channel", "market")]), 500);

        assert!(
            alerter.evaluate_at(&registry, 1_000).is_empty(),
            "there is no previous reading to difference against, so 500 opens \
             must not be read as 500 opens per minute"
        );
    }

    /// A restart reopens every client's sockets at once. That is one round of a
    /// lot, and it is expected -- which is what `sustained_rounds` is for.
    #[test]
    fn churn_ignores_a_single_round_of_opens() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::SocketChurn {
            max_opens_per_min: 20.0,
            sustained_rounds: 2,
        }]);
        let labels = Labels::new(&[("channel", "market")]);

        registry.inc_counter(WS_OPENS, "opens", &labels, 10);
        assert!(alerter.evaluate_at(&registry, 1_000).is_empty());

        // 30s later, 20 more opens -- 40/min, well over the limit, for one
        // round only. Every client reconnecting after a deploy looks like this.
        registry.inc_counter(WS_OPENS, "opens", &labels, 20);
        assert!(
            alerter.evaluate_at(&registry, 31_000).is_empty(),
            "one round above the limit is a page load or a restart, not a loop"
        );
    }

    #[test]
    fn churn_fires_when_the_rate_holds() {
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![Rule::SocketChurn {
            max_opens_per_min: 20.0,
            sustained_rounds: 2,
        }]);
        let labels = Labels::new(&[("channel", "market")]);

        registry.inc_counter(WS_OPENS, "opens", &labels, 5);
        assert!(alerter.evaluate_at(&registry, 1_000).is_empty());

        // 15 opens per 30s = 30/min, twice running.
        registry.inc_counter(WS_OPENS, "opens", &labels, 15);
        assert!(alerter.evaluate_at(&registry, 31_000).is_empty());

        registry.inc_counter(WS_OPENS, "opens", &labels, 15);
        let raised = alerter.evaluate_at(&registry, 61_000);

        assert_eq!(raised.len(), 1, "a loop must be announced");
        assert_eq!(raised[0].name, "socket_churn");
        assert_eq!(raised[0].severity, Severity::Warning);
        assert!(
            raised[0].detail.contains("30 sockets/min"),
            "the detail must name the measured rate: {}",
            raised[0].detail
        );

        // And it stays quiet while it stays broken.
        registry.inc_counter(WS_OPENS, "opens", &labels, 15);
        assert!(
            alerter.evaluate_at(&registry, 91_000).is_empty(),
            "a rule that re-fires every tick gets muted"
        );
        assert_eq!(sink.alerts().len(), 1);
    }

    /// The traffic actually observed on this platform -- roughly one socket a
    /// minute -- is not an incident, and must never become one.
    #[test]
    fn churn_ignores_traffic_at_the_rate_the_platform_actually_sees() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::SocketChurn {
            max_opens_per_min: 20.0,
            sustained_rounds: 2,
        }]);
        let labels = Labels::new(&[("channel", "market")]);

        let mut at = 1_000;
        registry.inc_counter(WS_OPENS, "opens", &labels, 7);
        assert!(alerter.evaluate_at(&registry, at).is_empty());

        // Seven sockets over seven minutes, as one open per check.
        for _ in 0..7 {
            at += 30_000;
            registry.inc_counter(WS_OPENS, "opens", &labels, 1);
            assert!(
                alerter.evaluate_at(&registry, at).is_empty(),
                "one socket per check is a dashboard being used, not a loop"
            );
        }
    }

    /// `docs/19` row 24: candles were healthy for a whole run while the book
    /// had never synced, and no rule could see it. This is the guard that
    /// would have fired.
    #[test]
    fn a_book_that_never_arrived_is_its_own_alert_and_not_the_feeds() {
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![Rule::StaleBook { max_age_secs: 60.0 }]);

        // The candle feed is fine, which is exactly the case that hid the
        // defect: `StaleFeed` is reading a healthy number.
        registry.set_gauge(MD_FEED_AGE, "age", &Labels::none(), 2.0);
        registry.set_gauge(
            MD_BOOK_AGE,
            "book age",
            &Labels::new(&[("symbol", "BTCUSDT")]),
            900.0,
        );

        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 1, "a dead book must be news");
        assert_eq!(raised[0].name, "stale_order_book");
        assert_eq!(raised[0].severity, Severity::Warning);
        assert!(
            raised[0].detail.contains("BTCUSDT") && raised[0].detail.contains("900"),
            "the detail must name the symbol and the age: {0}",
            raised[0].detail
        );
        assert_eq!(sink.alerts().len(), 1);
    }

    /// A fresh process has not had time for a book to arrive. Publishing an age
    /// for a symbol nobody has waited on would page on every restart.
    #[test]
    fn a_book_within_the_threshold_is_never_announced() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::StaleBook { max_age_secs: 60.0 }]);
        registry.set_gauge(
            MD_BOOK_AGE,
            "book age",
            &Labels::new(&[("symbol", "BTCUSDT")]),
            10.0,
        );
        assert!(alerter.evaluate(&registry).is_empty());
    }

    #[test]
    fn a_feed_within_the_threshold_is_never_announced() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::StaleFeed { max_age_secs: 60.0 }]);
        registry.set_gauge(MD_FEED_AGE, "age", &Labels::none(), 10.0);
        assert!(alerter.evaluate(&registry).is_empty());
    }

    #[test]
    fn the_error_rate_rule_needs_enough_requests_before_it_may_speak() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::ErrorRate {
            min_requests: 100,
            max_ratio: 0.05,
        }]);
        let ok = Labels::new(&[("route", "/candles"), ("status", "2xx")]);
        let bad = Labels::new(&[("route", "/candles"), ("status", "5xx")]);
        registry.inc_counter(HTTP_REQUESTS, "reqs", &ok, 40);
        registry.inc_counter(HTTP_REQUESTS, "reqs", &bad, 10);

        assert!(
            alerter.evaluate(&registry).is_empty(),
            "50 requests is below the minimum, so 20% is not yet evidence"
        );

        registry.inc_counter(HTTP_REQUESTS, "reqs", &ok, 60);
        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 1);
        assert!(
            raised[0].detail.contains("/candles"),
            "{}",
            raised[0].detail
        );
    }

    #[test]
    fn a_healthy_route_does_not_fire_the_error_rate_rule() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::ErrorRate {
            min_requests: 10,
            max_ratio: 0.05,
        }]);
        registry.inc_counter(
            HTTP_REQUESTS,
            "reqs",
            &Labels::new(&[("route", "/candles"), ("status", "2xx")]),
            100,
        );
        registry.inc_counter(
            HTTP_REQUESTS,
            "reqs",
            &Labels::new(&[("route", "/candles"), ("status", "5xx")]),
            2,
        );
        assert!(alerter.evaluate(&registry).is_empty());
    }

    #[test]
    fn a_kill_switch_activation_is_critical_and_fires_on_the_rise() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::KillSwitch]);
        registry.count(KILL_SWITCH, "ks", &Labels::none());

        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].severity, Severity::Critical);
        assert!(raised[0].detail.contains("kill-switch"));

        // Silence while nothing new happens -- and notably *not* a "resolved"
        // alert, because an event rule has no level to come back from.
        assert!(alerter.evaluate(&registry).is_empty());

        // A second activation -- another bot, or a restart -- is its own page.
        registry.count(KILL_SWITCH, "ks", &Labels::none());
        let second = alerter.evaluate(&registry);
        assert_eq!(second.len(), 1, "every activation is news");
        assert_eq!(second[0].severity, Severity::Critical);
        assert!(second[0].detail.contains('1'), "{}", second[0].detail);
    }

    #[test]
    fn an_event_rule_never_reports_itself_resolved() {
        // The distinction this pins: a kill-switch that stops firing is not a
        // kill-switch that cleared. Reporting "resolved" would tell an operator
        // trading resumed when it simply did not trip again.
        let registry = Registry::new();
        let (mut alerter, sink) = alerter(vec![Rule::RiskBreach]);
        registry.count(RISK_BREACHES, "b", &Labels::none());
        let _ = alerter.evaluate(&registry);
        let _ = alerter.evaluate(&registry);

        assert!(sink.alerts().iter().all(|a| a.detail != "resolved"));
    }

    #[test]
    fn a_risk_breach_and_a_reconcile_mismatch_are_reported() {
        let registry = Registry::new();
        let (mut alerter, _) = alerter(vec![Rule::RiskBreach, Rule::ReconcileMismatch]);
        registry.count(RISK_BREACHES, "b", &Labels::none());
        registry.count(RECONCILE_MISMATCHES, "m", &Labels::none());

        let raised = alerter.evaluate(&registry);
        assert_eq!(raised.len(), 2);
        assert!(raised.iter().all(|a| a.severity == Severity::Critical));
    }

    #[test]
    fn the_default_rules_are_the_seven_that_can_actually_fire() {
        let names: Vec<&str> = default_rules().iter().map(|rule| rule.name()).collect();
        assert_eq!(
            names,
            vec![
                "stale_market_data",
                "stale_order_book",
                "http_error_rate",
                "socket_churn",
                "kill_switch_engaged",
                "risk_limit_breached",
                "reconcile_mismatch",
            ]
        );
    }

    #[test]
    fn there_is_no_rule_for_something_nothing_measures() {
        // `docs/18` names backtest-to-live divergence and `docs/19` carries it as
        // debt. The rule was removed rather than left in place over an unwritten
        // metric, because a rule that cannot fire reads as coverage: an operator
        // checking the alert list would conclude divergence was watched. This
        // test fails if somebody re-adds the name without adding the writer.
        let names: Vec<&str> = default_rules().iter().map(|rule| rule.name()).collect();
        assert!(
            !names.contains(&"backtest_live_divergence"),
            "divergence has no writer; see docs/19 row 10: {names:?}"
        );
    }

    #[test]
    fn a_queued_alert_can_be_drained_by_a_delivery_task() {
        let queue = Arc::new(QueueSink::new());
        let mut alerter = Alerter::with_sinks(vec![Rule::KillSwitch], vec![queue.clone()]);
        let registry = Registry::new();
        registry.count(KILL_SWITCH, "ks", &Labels::none());

        // Discarded deliberately: this test is about the queue, not about what
        // the rule returned, and the returned list is asserted on elsewhere.
        let _ = alerter.evaluate(&registry);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        assert!(queue.drain().is_empty(), "a drain empties the queue");
    }
}
