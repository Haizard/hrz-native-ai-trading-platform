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

**Built, and where.** `trading_engine::LiveGate` checks all three and returns every
unmet reason at once, rather than the first — a gate that reports one failure at a time
turns a two-minute correction into one deploy per requirement. `POST /bots` calls it;
`LiveGate::check` is also the only place the thresholds live, so `GET /venues` reports
the same numbers a refusal is measured against.

The track record is aggregated **per strategy, across every paper bot that ran it**,
not per bot. A per-bot count would let a user who has run four paper bots over one
document be told they have proven a quarter as much as they have, and would let a fresh
bot reset the clock. Only `mode = 'paper'` rows count: a live trade is not evidence that
a strategy was ready to go live.

Revoking a venue is not a flag flip. It stops the live bots **already running** on that
venue (`BotSupervisor::kill_venue`), because a revoke that only changed what future bots
may do would leave a bot trading an account the operator has just withdrawn consent for.

## Auditability
- Every trade decision (including "no signal" evaluations, for investigation purposes),
  every risk-limit breach, and every kill-switch activation is written to the
  append-only `audit_log` table (`docs/13-DATABASE-SCHEMA.md`) — never overwritten,
  never deleted by normal application code paths.
- Every `TradeThesis` the AI produces is retained alongside the tool-call trace that
  produced it, so any live/paper decision downstream of it remains explainable after
  the fact.
- Live decisions carry their own event type, `bot.live_decision`, rather than sharing
  `bot.decision` with the paper path. The two live in one table and a reader asking
  "what did this bot consider?" should not have to guess whether the rows came from a
  simulator or a venue.
- `bot.started` and `bot.stopped` bracket every run, and their **absence** is the
  signal: a run that starts and never stops crashed. `bot.fatal` is written when a live
  bot stopped because it could not safely continue — an order whose fate is unknown —
  and says so, because the operator's next move is to reconcile rather than restart.

## Credential handling
- Exchange API keys are **never stored by the platform.** They are read from the API
  process's environment (`BINANCE_API_KEY` / `BINANCE_API_SECRET`) and exist only in
  memory for the lifetime of a request. This replaces the earlier "encrypted at rest"
  wording, which described a store that does not exist and would have been the wrong
  thing to build: a per-user settings page for secrets needs a purpose-built secret
  store, and doing that badly is worse than not offering it.
- Never logged. `ExchangeCredentials` has a hand-written `Debug` that prints
  `[redacted]`, and a unit test greps the debug line for the secret. The key travels in
  the `X-MBX-APIKEY` header and the secret is used only to sign the query string — a
  test asserts the secret never appears in the URL.
- The platform **cannot verify** a key's permissions; Binance does not expose them. So
  it does the next best thing: it never asks for a permission it does not need, and
  `docs/20-RUNBOOKS.md` §8 tells the operator to create the key with trading enabled and
  withdrawal disabled. A platform that claims to have verified something it cannot is
  worse than one that says so.
- `GET /venues` reports whether credentials are *present* and never their value. A
  boolean derived from reading the secret is the only safe way to answer that over HTTP.
- The sandbox (`docs/08-SANDBOX-WASM.md`) never has access to credentials under any
  circumstance — order placement happens outside the sandbox, driven by the sandbox's
  `Signal` output only. `trading-engine/src/execution.rs` is where that boundary is
  written down: the sandbox produces a `Signal`, everything after it is outside.

## The kill switch, and what it does not do
- `POST /bots/{id}/kill` trips the switch and **liquidates immediately**, rather than on
  the next decision bar. The switch is normally read inside the decision loop, which for
  a five-minute strategy is up to five minutes away, and a button labelled "stop" that
  takes five minutes is not a stop.
- What happens to an open position is the configured `OnBreach` policy, not a silent
  default: `Close` liquidates, `Hold` leaves it and logs a warning naming the position
  as unprotected. `docs/11` asks for that to be a policy, and a policy that is never
  honoured is decoration.
- The switch is tripped **before** the close is attempted, so a failed close leaves a
  bot that will not open anything else rather than one that carries on trading.
- It works with the agent and market data both degraded: it is a flag on an atomic, read
  on the task's next wakeup. Nothing about it needs the LLM or the feed.
- It does **not** close a position at a venue that is not answering. The runbook's answer
  is then to close by hand and reconcile.

### Reaching it
Both switches are in the bots pane of the shell: a **Kill switch** button on every bot
row, and a live-trading panel listing each venue with its opt-in state, whether this
deployment holds credentials for it, and an **Opt in** / **Revoke** button. Revoking
reports how many bots it threw the switch on, and the panel re-reads the bot list when
that number is non-zero — an operator who has just stopped trading should not have to
press Refresh to find out what stopped.

The two venue facts are shown separately because they fail identically from the outside.
"Opted in and it still refuses" is either this checkbox or an environment variable on the
API process, and only one of them is fixable from a browser. A single indicator would
leave the second case looking like a broken switch.

A disabled **Kill switch** button means the bot's status is already `killed`. It is not a
separate flag: the button reads the same field the row displays, so the two cannot
disagree about whether the switch is thrown.

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
