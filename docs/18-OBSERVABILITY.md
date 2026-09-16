# 18 — Observability

## Purpose
Make the platform's behavior legible to an on-call engineer without ad-hoc log diving —
especially important once live trading is involved (`docs/15-RISK-COMPLIANCE.md`).

## What exists, and what does not

**Built:** `crates/observability` (a leaf crate — `docs/03` forbids `api-gateway` from
depending on `trading-engine`, so without it the metric names and alert rules would exist
in five copies and drift), `GET /metrics` in Prometheus text exposition format, a
request-tracking middleware, JSON logs, and the alert rules. `docs/20-RUNBOOKS.md` maps
every alert name the code can raise to what an operator does about it.

**Every metric named in the list below now has a writer.** That sentence was not true
when this document was first written: `MD_*`, `AGENT_*`, `BACKTEST_*` and `WS_*` were
declared as constants and written by nothing, so the scrape served `# TYPE` lines with no
samples and two of the runbooks described alerts that could not fire. The writers, and
where they live:

| Metric group | Written by | In which process |
|---|---|---|
| `MD_CONNECTED`, `MD_MESSAGES`, `MD_GAPS`, `MD_RECONNECTS` | `market_data::CollectorHealth::publish`, on a 1s ticker spawned by `BinanceCollector::connect` | the collector |
| `MD_CLOSE_LATENCY` | `market_data::exchanges::binance::handle_trade`, at the moment a candle reaches the bus | the collector |
| `MD_FEED_AGE` | the gateway's alert task, sampled per symbol immediately before the rules are evaluated | api-gateway |
| `AGENT_*` | `api-gateway::ws::run_agent` and `agent_routes::ask` | api-gateway |
| `BACKTEST_*` | `api-gateway::strategy_routes::backtest` | api-gateway |
| `SIGNALS_GENERATED`, `ORDERS_EXECUTED`, `RISK_BREACHES`, `KILL_SWITCH`, `RECONCILE_MISMATCHES`, `OPEN_POSITIONS` | `trading_engine::live` | api-gateway (bots run in-process) |
| `HTTP_*` | `api-gateway::metrics::track` | api-gateway |
| `WS_CONNECTIONS`, `WS_DROPS` | `api-gateway::ws::Connection` | api-gateway |

**Not built, and named here rather than implied:** no dashboard. The scrape, the rules
and the runbooks exist; Grafana is a deployment artifact rather than a crate, and
`docs/17`'s deploy story is still unproven. See the debt table in
`docs/19-AFTER-THE-ROADMAP.md`.

### There is exactly one registry, and that is load-bearing

`Registry::global_handle()` is what a binary that serves `/metrics` must inject into
`AppState`. The first version of `main` built `Arc::new(Registry::new())` — a *different*
registry — while `trading-engine` wrote its counters to the global one. The result was
that `/metrics` served a registry the trading code never touched, and the alert task
evaluated that same empty one, so `kill_switch_engaged`, `risk_limit_breached` and
`reconcile_mismatch` were alerts that could not be raised by any event in the system.

The reason it is a *handle* rather than a forced global is the other half: tests inject
their own registry so they can assert on their own numbers instead of whatever the last
test left behind, and a test that shares the global would pass or fail depending on test
order.

### Health is copied on a ticker, not per message

`CollectorHealth` holds absolute totals in atomics; a `Registry` counter accumulates. The
publisher therefore sends **deltas**, tracked in `CollectorHealth::published`. Sending the
absolute value would make the counter climb by the whole total on every tick — 2, 4, 6, 8 —
and `rate()` would show a burst that never happened. `publishing_sends_deltas_rather_than_running_totals`
fails with `left: 4, right: 2` if that regresses.

A ticker rather than per message because per message would take a registry lock on the hot
path of a stream carrying every trade on the venue, to update numbers nobody reads more
than once every fifteen seconds.

## Metrics (per service, exported in a standard format e.g. Prometheus)
- **Market Data Engine**: per-exchange connection status, message rate, detected gaps,
  candle-close latency (time from candle close to persisted+published).
- **AI Agent**: request latency, tool-call count per request, LLM provider error rate,
  cost per request (token usage), thesis generation success/failure rate.
- **Backtester**: jobs queued/running/completed, average duration per backtest.
- **Trading Engine**: open positions count, signals generated vs. executed, risk-limit
  breach count, kill-switch activations.
- **API Gateway**: request rate/latency/error rate per route, WebSocket connection
  count, per-connection backpressure/drop events.

**Cost per request is not built.** `AGENT_*` covers latency, tool calls, theses and
provider errors; token usage is not currently surfaced by the Bedrock adapter, so there is
no number to publish. Named here so the gap is a known one rather than a missing metric
nobody noticed.

The names are constants in `observability::metrics` (`MD_FEED_AGE`, `SIGNALS_GENERATED`,
`KILL_SWITCH`, …) rather than string literals at the call site. A metric name is not
checked by the compiler anywhere — a rename is a series that silently stops existing and
a panel that goes blank — so the one place it is written down is a `pub const`, and a
test pins the JSON/render output.

### Non-finite values are refused on the way in
`set_gauge` has always dropped a NaN, because it renders as `NaN` and breaks every query
touching it. `add_gauge` did not, and that was the same bug with a worse shape: one NaN
delta added to a connection count makes every later read of that gauge return NaN for the
life of the process, and no subsequent correct delta can recover it. Both refuse now.


### Labels are sorted and deduped
`Labels` normalises on construction. Prometheus identifies a time series by its label
*set*, so `{route, status}` and `{status, route}` are the same series — but only if the
exposition is byte-identical. Building the string in whatever order a `HashMap` iterated
would produce two series for one thing, and the scrape would look like it was working.

### Route labels are templates, never ids
The request middleware labels on `MatchedPath` — `/bots/{id}`, not `/bots/8f3a…`. The
alternative is one time series per bot, which grows without bound and is the classic way
a metrics endpoint takes down the thing it was added to observe. `load_flow.rs` asserts
the id does not appear in the scrape.

## Logging
- Structured (JSON) logs with a consistent set of fields: `service`, `request_id`/
  `bot_id`/`strategy_id` where applicable, `level`, `message`.
- Every AI agent tool call is logged with its inputs and outputs (redacting nothing
  market-data-related; credentials are never logged at all, per
  `docs/15-RISK-COMPLIANCE.md`) — this log is what makes a `TradeThesis` auditable after
  the fact.
- `observability::init_logging(service)` installs the subscriber; `LOG_FORMAT=json`
  switches to JSON. Default is human-readable, because the common case for a
  human-readable log is a human reading it, and the common case for JSON is a collector.
- `request_id` is a span field, not a field on every `log` call. Threading it through by
  hand is the thing people forget at exactly the call site that mattered.

## Tracing
- Distributed tracing (e.g. OpenTelemetry) across API Gateway → AI Agent → tool calls →
  Analytics Core, so a single slow or failed request can be followed end-to-end across
  crate/service boundaries.

**Status:** spans exist and carry a `request_id`; nothing exports them. That is a real
gap rather than a completed item — see debt row 11 in `docs/19`. The honest version of
the requirement is "a request can be followed end to end", and today that is true within
one process and false across processes.

## Alerting
- Market data gap/disconnect beyond a threshold duration.
- Risk-limit breach / kill-switch activation (page immediately, this is real-money
  relevant once live trading exists).
- Backtest/paper-trade divergence beyond an expected tolerance for a bot that's
  supposedly running the same strategy in both modes (a strong signal something is
  inconsistent between the two execution paths). **No rule exists for this** — see below.
- API error-rate or latency SLO breach.

### Two kinds of rule, and conflating them is a bug
`observability::alerts::Rule` is an enum, not a closure, so a rule can be named, deduped,
logged and tested. The distinction that matters:

- **Level rules** (`StaleFeed`, `ErrorRate`) describe a *condition*. They fire once on the
  transition into breach and once on the transition out, because a feed that has been
  stale for an hour is one incident, not sixty.
- **Event rules** (`KillSwitch`, `RiskBreach`, `ReconcileMismatch`) describe a *thing
  that happened*. They fire on every new activation and **never** report themselves
  resolved, because there is nothing to resolve — the next activation is a new event.

The first version of this treated every rule as a level rule. A second kill-switch
activation was therefore silent, and the rule reported "resolved" when it had merely
stopped firing. `Rule::is_event()` and the test `an_event_rule_never_reports_itself_resolved`
exist because of that.

### A rule that cannot fire is worse than no rule

`Rule::Divergence` used to be in `default_rules()`, over a `DIVERGENCE_R` gauge that
nothing wrote. It could never fire, and that reads as coverage: an operator looking at the
alert list, or at the runbook in `docs/20`, would conclude divergence was monitored. Both
the rule and the constant were removed, and `there_is_no_rule_for_something_nothing_measures`
fails if either comes back without a writer. `docs/19` row 10 carries the intent — it needs
a job that compares a bot's realised R against its backtest over the same window, not a
threshold over an empty metric.

The same reasoning is why `MD_FEED_AGE` is now sampled by the alert task rather than
published by the collector: a value published at publish-time is already wrong by the time
the rule reads it, and it is wrong by an amount that grows with the scrape interval.

### Alerts reach a human two ways
`LogSink` always, and `QueueSink` when `ALERT_WEBHOOK_URL` is set — the queue is drained
into `platform.alert` rows in `audit_log`, so an alert is durable and greppable next to
the decisions that produced it, and a webhook becomes another reader of those rows rather
than another writer. Evaluation runs every 30 seconds (`ALERT_INTERVAL`).

## Dashboards
- One dashboard per service matching the metrics above, plus a single "platform health"
  overview combining market-data freshness, active bots, open risk exposure, and recent
  kill-switch events — this is what the exit criteria in
  `docs/17-DEPLOYMENT-INFRA.md`/Phase 8 of `02-ROADMAP.md` expects an on-call engineer
  to be able to use without additional tooling.

## Done criteria
- Every metric listed above is actually exported and visible on a dashboard, not just
  planned.

  **Half true.** Every metric listed above now has a writer and appears in the scrape —
  that half is verified by tests. "Visible on a dashboard" is not: there is no dashboard.
  Stated plainly because the first version of this section read as though both halves
  were done.
- A deliberately induced incident (e.g. kill a market-data connection, force a risk
  breach in staging) produces the expected alert within a defined time budget.

  **True for the four rules that exist**, and now actually reachable: the three
  conditions that used to make them unreachable (a second registry, an unwritten
  `MD_FEED_AGE`, and counter metrics with no writers) are fixed and each has a test.
  The remaining caveat is the deployment: `docs/17`'s container has never been built, so
  this has been demonstrated in-process rather than in staging.
