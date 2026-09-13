# 15 — Risk & Compliance

## Purpose
Ensure the platform never places the user at unbounded risk, whether the strategy came
from natural language, a visual builder, or a developer SDK, and that live trading is
introduced deliberately rather than by default.

## Risk limits (enforced in `trading-engine`, not just suggested in the DSL)
- **Platform-wide hard ceiling** on `max_risk_pct` per trade, independent of what a
  strategy document requests (e.g. never above 5%, configurable but never unbounded).
- **Per-account daily/weekly loss limits**, configurable by the user within platform
  maximums.
- **Max concurrent open positions** per account and per strategy.
- **Kill-switch**: automatic on limit breach; manual, reachable from the UI, always
  available regardless of system state (must work even if the AI agent or market-data
  engine is degraded).

## Live-trading gating (see also `docs/11-BOT-TRADING-ENGINE.md`)
A strategy may not go live until:
1. It has run in paper mode for a minimum configured duration/trade count.
2. The user has explicitly opted in per venue (API keys scoped to trading-only
   permissions where the exchange supports it — never request withdrawal permissions).
3. Risk limits are configured and active for that account.

## Auditability
- Every trade decision (including "no signal" evaluations, for investigation purposes),
  every risk-limit breach, and every kill-switch activation is written to the
  append-only `audit_log` table (`docs/13-DATABASE-SCHEMA.md`) — never overwritten,
  never deleted by normal application code paths.
- Every `TradeThesis` the AI produces is retained alongside the tool-call trace that
  produced it, so any live/paper decision downstream of it remains explainable after
  the fact.

## Credential handling
- Exchange API keys are encrypted at rest, scoped to the minimum required permission set,
  and never logged (including in the `audit_log` payloads — redact before writing).
- The sandbox (`docs/08-SANDBOX-WASM.md`) never has access to credentials under any
  circumstance — order placement happens outside the sandbox, driven by the sandbox's
  `Signal` output only.

## Scope note
This document defines platform-level risk controls and operational safeguards. It is
not legal or regulatory advice — actual regulatory requirements (licensing, KYC/AML,
jurisdictional restrictions on offering automated trading tools) depend on where and to
whom the platform is offered and should be reviewed with qualified legal counsel before
any live-money launch.

## Done criteria
- A deliberately misconfigured strategy (e.g. requesting 20% risk per trade) is clamped
  or rejected, never executed as requested.
- A simulated daily-loss-limit breach triggers the kill-switch and halts further signal
  execution for that account, verified by an automated test.
