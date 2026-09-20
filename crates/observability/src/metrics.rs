//! Metrics registry and Prometheus exposition (`docs/18`).
//!
//! ## Why not a metrics crate
//!
//! The usual answer is `prometheus` + `lazy_static`, and the reason this is not
//! that is the same reason the sandbox has no YAML parser: what a service
//! links is part of its attack surface and its build. We need counters,
//! gauges, latency histograms, and one text endpoint -- and the part that is
//! genuinely easy to get wrong (atomics on `f64`) is the part that hides
//! behind a `Mutex` here, which at our rates is free.
//!
//! ## Names are constants, not string literals at call sites
//!
//! `docs/18` lists the metrics per service. Spelling them as constants in one
//! place means a dashboard query and the code cannot disagree: a typo in a
//! metric name is not a compile error anywhere else, it is a blank panel --
//! the exact failure mode that Phase 7 hit with the chart's JSON keys.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

// --- market data (docs/18) ---
/// Whether the exchange socket is connected: 1 or 0.
pub const MD_CONNECTED: &str = "market_data_connected";
/// Messages received from the exchange, total.
pub const MD_MESSAGES: &str = "market_data_messages_total";
/// Sequence or trade-id gaps detected, total.
pub const MD_GAPS: &str = "market_data_gaps_total";
/// Websocket reconnects, total.
pub const MD_RECONNECTS: &str = "market_data_reconnects_total";
/// Frames on a topic we recognise that did not decode, total.
///
/// The collector treats these as **non-fatal** -- one bad frame must not end the
/// pump, for the same reason an unrecognised frame must not -- so a counter is
/// the only thing that can make a venue slowly drifting away from our model of
/// it visible. A log line is not an alert.
pub const MD_DECODE_ERRORS: &str = "market_data_decode_errors_total";
/// Seconds from candle close to persisted-and-published.
pub const MD_CLOSE_LATENCY: &str = "market_data_candle_close_latency_seconds";
/// Seconds since the newest candle was persisted. The stale-feed alert reads
/// this, which is why it is a gauge rather than a counter.
pub const MD_FEED_AGE: &str = "market_data_feed_age_seconds";

// --- ai agent ---
/// Agent requests, total.
pub const AGENT_REQUESTS: &str = "agent_requests_total";
/// Agent request duration.
pub const AGENT_LATENCY: &str = "agent_request_duration_seconds";
/// Tool calls made by the agent, total.
pub const AGENT_TOOL_CALLS: &str = "agent_tool_calls_total";
/// LLM provider errors, total.
pub const AGENT_PROVIDER_ERRORS: &str = "agent_provider_errors_total";
/// Theses produced, by outcome.
pub const AGENT_THESES: &str = "agent_theses_total";

// --- backtester ---
/// Backtests queued.
pub const BACKTEST_QUEUED: &str = "backtests_queued_total";
/// Backtests completed.
pub const BACKTEST_COMPLETED: &str = "backtests_completed_total";
/// Backtest duration.
pub const BACKTEST_DURATION: &str = "backtest_duration_seconds";

// --- trading engine ---
/// Currently open positions.
pub const OPEN_POSITIONS: &str = "trading_open_positions";
/// Signals the strategy produced, total.
pub const SIGNALS_GENERATED: &str = "trading_signals_generated_total";
/// Orders the exchange acknowledged, total.
pub const ORDERS_EXECUTED: &str = "trading_orders_executed_total";
/// Risk-limit breaches, total.
pub const RISK_BREACHES: &str = "trading_risk_breaches_total";
/// Kill-switch activations, total.
pub const KILL_SWITCH: &str = "trading_kill_switch_total";
/// Reconciliation mismatches between our book and the exchange's, total.
pub const RECONCILE_MISMATCHES: &str = "trading_reconcile_mismatches_total";

// --- api gateway ---
/// HTTP requests, total, by route and status class.
pub const HTTP_REQUESTS: &str = "http_requests_total";
/// HTTP request duration, by route.
pub const HTTP_LATENCY: &str = "http_request_duration_seconds";
/// How many bars a symbol's chart can be served from RAM, per resolution.
///
/// The replacement for the store-age pair this module used to carry. Market
/// data is **not persisted** -- the database is a free tier with 6 GB total for
/// every symbol of every market -- so "how stale is the newest stored candle"
/// stopped being a question anybody can ask. What matters now is how much of a
/// chart's window RAM can answer without a REST call to the venue, because that
/// is the difference between an instant chart and one that waits on Binance.
///
/// Labelled per symbol and resolution. A single global count would hide the
/// symbol that just restarted behind the one that has been up for days, and the
/// whole point of the number is per-series coverage.
pub const MD_HISTORY_BARS: &str = "market_data_history_bars";
/// Seconds since the newest order book for a symbol arrived.
///
/// The one number that can see `docs/19` row 24: a depth stream that has never
/// bridged onto its REST snapshot publishes **nothing**, so from outside it is
/// indistinguishable from a book nobody asked for -- empty DOM, 404 on
/// `/orderbook`. Age separates them, and it is measured from when the symbol
/// was first waited on if no book has ever arrived, so "never worked" has a
/// growing number rather than no number at all.
///
/// Labelled per symbol, and only for symbols something has actually waited on.
pub const MD_BOOK_AGE: &str = "market_data_book_age_seconds";
/// Live WebSocket connections.
pub const WS_CONNECTIONS: &str = "websocket_connections";
/// WebSocket connections accepted, total.
///
/// The gauge answers "how many are open right now"; this answers "how many have
/// ever been opened". Churn is only visible in the second: a gauge that sits at
/// 2 while this climbs by 40 an hour is a client reconnecting, and a gauge at 40
/// while this sits at 40 is a leak. Those are two different incidents with the
/// same gauge reading, and the gauge alone cannot separate them.
pub const WS_OPENS: &str = "websocket_connections_opened_total";
/// Messages dropped because a client's socket was slow, total.
pub const WS_DROPS: &str = "websocket_dropped_messages_total";

/// The kind of a metric, for the `# TYPE` line in the exposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Monotonically increasing.
    Counter,
    /// A value that goes up and down.
    Gauge,
    /// Observations bucketed by upper bound.
    Histogram,
}

impl Kind {
    /// The Prometheus type name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

/// A metric's label set.
///
/// Sorted and de-duplicated so `a=1,b=2` and `b=2,a=1` are the same series --
/// two orderings that silently become two time series is a classic way to make
/// a rate() query return half the truth.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Labels(Vec<(String, String)>);

impl Labels {
    /// Build a label set from pairs.
    #[must_use]
    pub fn new(pairs: &[(&str, &str)]) -> Self {
        let mut labels: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        labels.sort();
        labels.dedup();
        Self(labels)
    }

    /// No labels.
    #[must_use]
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// Whether this set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Render as `{key="value",...}`, or the empty string when unlabelled.
    #[must_use]
    pub fn render(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let joined = self
            .0
            .iter()
            .map(|(key, value)| format!("{key}=\"{value}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{joined}}}")
    }
}

/// One exported value. Histograms are excluded: their samples are rendered
/// directly rather than flattened, because a histogram's value is its buckets.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Metric name.
    pub name: String,
    /// The help text registered with it.
    pub help: String,
    /// Its kind.
    pub kind: Kind,
    /// Its labels.
    pub labels: Labels,
    /// Its value.
    pub value: f64,
}

/// A histogram: fixed buckets, plus sum and count.
#[derive(Debug, Clone)]
struct Histogram {
    buckets: Vec<(f64, u64)>,
    sum: f64,
    count: u64,
}

impl Histogram {
    /// Latency-appropriate bounds, in seconds: fine where human-perceived
    /// latency lives, coarse in the tail.
    fn new() -> Self {
        const BOUNDS: [f64; 12] = [
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ];
        Self {
            buckets: BOUNDS.iter().map(|upper| (*upper, 0u64)).collect(),
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, value: f64) {
        if !value.is_finite() {
            // A NaN here would poison sum forever, and the only way to notice
            // is a dashboard that reads "NaN" weeks later.
            return;
        }
        self.sum += value;
        self.count += 1;
        for (upper, count) in &mut self.buckets {
            if value <= *upper {
                *count += 1;
            }
        }
    }
}

/// Help text and kind, recorded the first time a name is used.
#[derive(Debug, Clone)]
struct Def {
    help: String,
    kind: Kind,
}

/// A registry of counters, gauges and histograms.
///
/// All three live under one `Mutex`. That is a deliberate simplification: this
/// platform does a few thousand increments per second at most, and a lock is
/// vastly easier to reason about than three sets of atomics with `f64` bit
/// juggling. If a profile ever says otherwise, the fix is local to this file.
#[derive(Debug)]
pub struct Registry {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    defs: HashMap<String, Def>,
    counters: HashMap<(String, Labels), u64>,
    gauges: HashMap<(String, Labels), f64>,
    histograms: HashMap<(String, Labels), Histogram>,
}

static GLOBAL: OnceLock<std::sync::Arc<Registry>> = OnceLock::new();

/// The one global registry, created on first use.
fn global_handle_inner() -> &'static std::sync::Arc<Registry> {
    GLOBAL.get_or_init(|| std::sync::Arc::new(Registry::new()))
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The process-wide registry.
    ///
    /// For code that has no natural place to keep an `Arc` -- a bot's risk
    /// check, for instance. Anything with a state struct should hold one
    /// explicitly so tests get isolation.
    #[must_use]
    pub fn global() -> &'static Self {
        global_handle_inner().as_ref()
    }

    /// The process-wide registry, as an owned handle.
    ///
    /// ## Why this exists, and the bug it closes
    ///
    /// `global()` returns a reference, which is what a bot's risk check wants.
    /// But the service that *serves* the scrape holds an `Arc<Registry>`, and
    /// the obvious way to build that is `Arc::new(Registry::new())` -- which is
    /// a **different registry**. That is exactly what `main` did, and the
    /// consequence was not cosmetic: the trading engine wrote
    /// `trading_kill_switch_total`, `trading_risk_breaches_total` and
    /// `trading_reconcile_mismatches_total` to the global while `/metrics`
    /// served a registry that had never seen them, and the alert task evaluated
    /// that same empty one. Three runbooks in `docs/20` described alerts that
    /// were structurally incapable of firing.
    ///
    /// A binary that serves metrics must inject *this*, not a new registry.
    /// Tests inject their own, which is the other half of why this is a handle
    /// and not a forced global.
    #[must_use]
    pub fn global_handle() -> std::sync::Arc<Self> {
        std::sync::Arc::clone(global_handle_inner())
    }

    /// Record the help and kind of a metric, once.
    ///
    /// The first writer wins and a disagreement is ignored rather than
    /// panicking: instrumentation must never be the reason a service dies.
    fn define(&self, name: &str, help: &str, kind: Kind) {
        let mut inner = self.lock();
        inner.defs.entry(name.to_string()).or_insert_with(|| Def {
            help: help.to_string(),
            kind,
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a panic happened while a metric was being
        // recorded. Losing the counters is better than losing the service, so
        // the guard is recovered rather than propagated.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Add `delta` to a counter.
    pub fn inc_counter(&self, name: &str, help: &str, labels: &Labels, delta: u64) {
        self.define(name, help, Kind::Counter);
        let mut inner = self.lock();
        *inner
            .counters
            .entry((name.to_string(), labels.clone()))
            .or_insert(0) += delta;
    }

    /// Add one to a counter. The common case.
    pub fn count(&self, name: &str, help: &str, labels: &Labels) {
        self.inc_counter(name, help, labels, 1);
    }

    /// A counter's current value.
    #[must_use]
    pub fn counter(&self, name: &str, labels: &Labels) -> u64 {
        self.lock()
            .counters
            .get(&(name.to_string(), labels.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// Set a gauge.
    pub fn set_gauge(&self, name: &str, help: &str, labels: &Labels, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.define(name, help, Kind::Gauge);
        self.lock()
            .gauges
            .insert((name.to_string(), labels.clone()), value);
    }

    /// Add `delta` to a gauge, for things like "open connections".
    ///
    /// Guarded for the same reason [`set_gauge`](Self::set_gauge) is, and it
    /// was not: a NaN or infinite delta would be added to the stored value and
    /// every later read of that gauge would return NaN, permanently, for the
    /// life of the process. The refusal has to happen on the way in, because
    /// once NaN is stored there is no arithmetic that recovers from it.
    pub fn add_gauge(&self, name: &str, help: &str, labels: &Labels, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.define(name, help, Kind::Gauge);
        let mut inner = self.lock();
        let entry = inner
            .gauges
            .entry((name.to_string(), labels.clone()))
            .or_insert(0.0);
        let next = *entry + delta;
        if next.is_finite() {
            *entry = next;
        }
    }

    /// A gauge's current value.
    #[must_use]
    pub fn gauge(&self, name: &str, labels: &Labels) -> Option<f64> {
        self.lock()
            .gauges
            .get(&(name.to_string(), labels.clone()))
            .copied()
    }

    /// Record one observation in a histogram.
    pub fn observe(&self, name: &str, help: &str, labels: &Labels, value: f64) {
        self.define(name, help, Kind::Histogram);
        self.lock()
            .histograms
            .entry((name.to_string(), labels.clone()))
            .or_insert_with(Histogram::new)
            .observe(value);
    }

    /// Every counter and gauge, sorted by name then labels.
    ///
    /// Histograms are not included: an alert rule asks about a count or a
    /// level, and a histogram's meaning is its distribution. Rendering is
    /// still complete -- see [`Registry::render`].
    #[must_use]
    pub fn snapshot(&self) -> Vec<Sample> {
        let inner = self.lock();
        let mut out: Vec<Sample> = Vec::new();

        for ((name, labels), value) in &inner.counters {
            out.push(Sample {
                name: name.clone(),
                help: Self::help(&inner, name),
                kind: Kind::Counter,
                labels: labels.clone(),
                value: *value as f64,
            });
        }
        for ((name, labels), value) in &inner.gauges {
            out.push(Sample {
                name: name.clone(),
                help: Self::help(&inner, name),
                kind: Kind::Gauge,
                labels: labels.clone(),
                value: *value,
            });
        }

        out.sort_by(|a, b| (&a.name, &a.labels).cmp(&(&b.name, &b.labels)));
        out
    }

    fn help(inner: &Inner, name: &str) -> String {
        inner
            .defs
            .get(name)
            .map_or_else(String::new, |def| def.help.clone())
    }

    /// The exposition format Prometheus scrapes.
    ///
    /// Groups by name so each `# TYPE` appears once, which is what the format
    /// requires and what a naive per-sample dump gets wrong.
    #[must_use]
    pub fn render(&self) -> String {
        let inner = self.lock();
        let mut out = String::new();

        let mut names: Vec<&String> = inner.defs.keys().collect();
        names.sort();

        for name in names {
            let Some(def) = inner.defs.get(name) else {
                continue;
            };
            let _ = writeln!(out, "# HELP {name} {}", def.help);
            let _ = writeln!(out, "# TYPE {name} {}", def.kind.as_str());

            match def.kind {
                Kind::Counter => {
                    let mut series: Vec<(&Labels, u64)> = inner
                        .counters
                        .iter()
                        .filter(|(key, _)| key.0 == *name)
                        .map(|(key, value)| (&key.1, *value))
                        .collect();
                    series.sort_by(|a, b| a.0.cmp(b.0));
                    for (labels, value) in series {
                        let _ = writeln!(out, "{name}{} {value}", labels.render());
                    }
                }
                Kind::Gauge => {
                    let mut series: Vec<(&Labels, f64)> = inner
                        .gauges
                        .iter()
                        .filter(|(key, _)| key.0 == *name)
                        .map(|(key, value)| (&key.1, *value))
                        .collect();
                    series.sort_by(|a, b| a.0.cmp(b.0));
                    for (labels, value) in series {
                        let _ = writeln!(out, "{name}{} {value}", labels.render());
                    }
                }
                Kind::Histogram => {
                    let mut series: Vec<(&Labels, &Histogram)> = inner
                        .histograms
                        .iter()
                        .filter(|(key, _)| key.0 == *name)
                        .map(|(key, value)| (&key.1, value))
                        .collect();
                    series.sort_by(|a, b| a.0.cmp(b.0));
                    for (labels, histogram) in series {
                        for (upper, count) in &histogram.buckets {
                            let rendered = labels.render();
                            let le = if labels.is_empty() {
                                format!("{{le=\"{upper}\"}}")
                            } else {
                                format!("{},le=\"{upper}\"", rendered.trim_end_matches('}'))
                            };
                            let _ = writeln!(out, "{name}_bucket{le} {count}");
                        }
                        let rendered = labels.render();
                        let _ = writeln!(out, "{name}_sum{rendered} {}", histogram.sum);
                        let _ = writeln!(out, "{name}_count{rendered} {}", histogram.count);
                    }
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counter_is_monotonic_and_readable() {
        let registry = Registry::new();
        let labels = Labels::new(&[("route", "/candles")]);
        registry.count(HTTP_REQUESTS, "requests", &labels);
        registry.count(HTTP_REQUESTS, "requests", &labels);
        registry.inc_counter(HTTP_REQUESTS, "requests", &labels, 3);

        assert_eq!(registry.counter(HTTP_REQUESTS, &labels), 5);
    }

    #[test]
    fn label_order_does_not_create_a_second_series() {
        // Two spellings of the same series must not become two time series.
        let registry = Registry::new();
        registry.set_gauge(
            WS_CONNECTIONS,
            "ws",
            &Labels::new(&[("a", "1"), ("b", "2")]),
            1.0,
        );
        registry.set_gauge(
            WS_CONNECTIONS,
            "ws",
            &Labels::new(&[("b", "2"), ("a", "1")]),
            7.0,
        );

        assert_eq!(
            registry.gauge(WS_CONNECTIONS, &Labels::new(&[("a", "1"), ("b", "2")])),
            Some(7.0)
        );
    }

    #[test]
    fn a_gauge_goes_up_and_down() {
        let registry = Registry::new();
        let labels = Labels::none();
        registry.set_gauge(OPEN_POSITIONS, "open", &labels, 2.0);
        registry.add_gauge(OPEN_POSITIONS, "open", &labels, -1.0);
        assert_eq!(registry.gauge(OPEN_POSITIONS, &labels), Some(1.0));
    }

    #[test]
    fn a_non_finite_value_is_refused_rather_than_poisoning_the_registry() {
        // A NaN would render as "NaN" and break every query touching it.
        let registry = Registry::new();
        registry.set_gauge(OPEN_POSITIONS, "open", &Labels::none(), f64::NAN);
        assert_eq!(registry.gauge(OPEN_POSITIONS, &Labels::none()), None);

        // Infinity is the other way a division by a zero risk distance reaches
        // this call, and it renders as "+Inf" -- just as unqueryable.
        registry.set_gauge(OPEN_POSITIONS, "open", &Labels::none(), f64::INFINITY);
        assert_eq!(registry.gauge(OPEN_POSITIONS, &Labels::none()), None);
    }

    #[test]
    fn a_bad_delta_cannot_poison_a_gauge_for_the_life_of_the_process() {
        // `add_gauge` is how connection counts move. A single NaN delta used to
        // be added to the stored value, after which every read returned NaN and
        // no later correct delta could repair it.
        let registry = Registry::new();
        registry.add_gauge(WS_CONNECTIONS, "open", &Labels::none(), 3.0);

        registry.add_gauge(WS_CONNECTIONS, "open", &Labels::none(), f64::NAN);
        assert_eq!(
            registry.gauge(WS_CONNECTIONS, &Labels::none()),
            Some(3.0),
            "a NaN delta must be dropped, not absorbed"
        );

        registry.add_gauge(WS_CONNECTIONS, "open", &Labels::none(), -1.0);
        assert_eq!(registry.gauge(WS_CONNECTIONS, &Labels::none()), Some(2.0));
    }

    #[test]
    fn an_unknown_metric_reads_as_zero_not_as_an_error() {
        let registry = Registry::new();
        assert_eq!(registry.counter("nope", &Labels::none()), 0);
        assert_eq!(registry.gauge("nope", &Labels::none()), None);
    }

    #[test]
    fn the_exposition_names_each_type_once_and_quotes_labels() {
        let registry = Registry::new();
        registry.count(
            HTTP_REQUESTS,
            "requests",
            &Labels::new(&[("route", "/candles")]),
        );
        registry.set_gauge(WS_CONNECTIONS, "sockets", &Labels::none(), 3.0);
        registry.observe(HTTP_LATENCY, "latency", &Labels::none(), 0.004);

        let text = registry.render();
        assert!(
            text.contains("# TYPE http_requests_total counter"),
            "{text}"
        );
        assert!(
            text.contains("# TYPE websocket_connections gauge"),
            "{text}"
        );
        assert!(
            text.contains("# TYPE http_request_duration_seconds histogram"),
            "{text}"
        );
        assert!(
            text.contains("http_requests_total{route=\"/candles\"} 1"),
            "{text}"
        );
        assert!(text.contains("websocket_connections 3"), "{text}");
        // The 5ms bucket contains the 4ms observation; the 1ms one does not.
        assert!(
            text.contains("http_request_duration_seconds_bucket{le=\"0.005\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("http_request_duration_seconds_bucket{le=\"0.001\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("http_request_duration_seconds_count 1"),
            "{text}"
        );
    }

    #[test]
    fn a_histogram_ignores_a_nan_observation() {
        let registry = Registry::new();
        registry.observe(HTTP_LATENCY, "latency", &Labels::none(), f64::NAN);
        assert!(registry.render().contains("_count 0"));
    }

    #[test]
    fn the_snapshot_is_sorted_so_output_is_stable() {
        let registry = Registry::new();
        registry.count("b_metric", "b", &Labels::none());
        registry.count("a_metric", "a", &Labels::none());
        let names: Vec<String> = registry.snapshot().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["a_metric", "b_metric"]);
    }

    #[test]
    fn the_global_registry_is_the_same_one_every_time() {
        let a = Registry::global();
        let labels = Labels::new(&[("test", "global")]);
        a.count("test_metric", "test", &labels);
        assert_eq!(Registry::global().counter("test_metric", &labels), 1);
    }
}
