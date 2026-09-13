# 18 — Observability

## Purpose
Make the platform's behavior legible to an on-call engineer without ad-hoc log diving —
especially important once live trading is involved (`docs/15-RISK-COMPLIANCE.md`).

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

## Logging
- Structured (JSON) logs with a consistent set of fields: `service`, `request_id`/
  `bot_id`/`strategy_id` where applicable, `level`, `message`.
- Every AI agent tool call is logged with its inputs and outputs (redacting nothing
  market-data-related; credentials are never logged at all, per
  `docs/15-RISK-COMPLIANCE.md`) — this log is what makes a `TradeThesis` auditable after
  the fact.

## Tracing
- Distributed tracing (e.g. OpenTelemetry) across API Gateway → AI Agent → tool calls →
  Analytics Core, so a single slow or failed request can be followed end-to-end across
  crate/service boundaries.

## Alerting
- Market data gap/disconnect beyond a threshold duration.
- Risk-limit breach / kill-switch activation (page immediately, this is real-money
  relevant once live trading exists).
- Backtest/paper-trade divergence beyond an expected tolerance for a bot that's
  supposedly running the same strategy in both modes (a strong signal something is
  inconsistent between the two execution paths).
- API error-rate or latency SLO breach.

## Dashboards
- One dashboard per service matching the metrics above, plus a single "platform health"
  overview combining market-data freshness, active bots, open risk exposure, and recent
  kill-switch events — this is what the exit criteria in
  `docs/17-DEPLOYMENT-INFRA.md`/Phase 8 of `02-ROADMAP.md` expects an on-call engineer
  to be able to use without additional tooling.

## Done criteria
- Every metric listed above is actually exported and visible on a dashboard, not just
  planned.
- A deliberately induced incident (e.g. kill a market-data connection, force a risk
  breach in staging) produces the expected alert within a defined time budget.
