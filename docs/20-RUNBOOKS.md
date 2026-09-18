# 20 — Incident Runbooks

## Purpose

`docs/18`'s done criterion is that an on-call engineer can diagnose an incident
from dashboards alone. This document is the other half of that: what the
dashboards mean, what to do first, and — just as importantly — **what the
obvious action does not fix**.

Every section is one of two things, and says which: **an alert name that
`crates/observability/src/alerts.rs` can actually raise**, or **a situation with
no alert**, where the engineer is the detector. The first kind is indexed below;
the second kind is §5–§8, and each of them states plainly that nothing will page
you. §9 is a third thing: the procedure for turning live trading on, which is here
because its two failure modes look like each other and like a broken switch, and
neither names its own fix. §10 is a fourth: an alert whose symptom looks alarming
in a log and is usually nothing, so the section spends as long on what it does
*not* mean as on what to do. If an alert fires and its name is not in this table,
that is a gap in this document, not in the engineer.

| Alert | Severity | Runbook |
|---|---|---|
| `stale_market_data` | warning | [§1](#1-stale-market-data) |
| `stale_order_book` | warning | [§12](#12-no-order-book-is-arriving) |
| `http_error_rate` | warning | [§2](#2-http-error-rate) |
| `kill_switch_engaged` | critical | [§3](#3-kill-switch-engaged) |
| `risk_limit_breached` | critical | [§3](#3-kill-switch-engaged) |
| `reconcile_mismatch` | critical | [§4](#4-reconciliation-mismatch) |
| `socket_churn` | warning | [§10](#10-sockets-keep-opening) |

`stale_stored_data` used to be in this table. It is **gone on purpose**: market data is no
longer persisted, so there is no stored series that can go stale. Charts are served from an
in-memory buffer plus the venue's REST API — see `docs/18` and §11.

## Where to look first

| Question | Where |
|---|---|
| Is the process alive? | `GET /healthz` — liveness only, never touches the database |
| Can it serve? | `GET /readyz` — probes Postgres, 503 when down |
| What is it doing? | `GET /metrics` — Prometheus text, the same numbers the alerts read |
| What did it decide? | `audit_log` table, `event_type = 'bot.decision'` |
| What did it say about itself? | `audit_log` table, `event_type = 'platform.alert'` |
| What did it do with money? | `live_orders` table, keyed by the client order id we generated |

`LOG_FORMAT=json` is what makes the log greppable by field. In deployment it
should always be set; the default is human-readable for local runs.

---

## 1. Stale market data

**Symptom.** `market_data_feed_age_seconds` rises past 120s. No new candles are
being persisted.

**First action.** Check `market_data_connected` and
`market_data_reconnects_total`:

- `connected = 0` and reconnects climbing: the exchange socket is dropping. Let
  the collector's own reconnect loop work; it re-subscribes on every reconnect,
  which is the bug that used to make a dropped socket look like a quiet market.
- `connected = 1` but the feed age still climbs: the socket is up and silent.
  This is the worse case — it looks healthy. Check
  `market_data_messages_total` for movement. If it is flat, the subscription is
  gone even though the socket is not.

**What this does not mean.** A stale feed does **not** stop the bots. A live bot
with an open position still has its stop resting at the venue, which is exactly
why protective orders are placed separately from the bot's own loop. Do not
"fix" this by closing positions.

**Escalation.** If the feed has been stale for more than one decision timeframe,
stop opening new positions (`POST /bots/{id}/pause`), leave existing protection
alone, and investigate.

## 2. HTTP error rate

**Symptom.** `http_error_rate` fires: more than 5% of requests to one route are
5xx, over at least 50 requests.

**First action.** The alert names the route. The label set on
`http_requests_total` is `{route, status}`, where `route` is the matched
*template* (`/strategies/{id}/backtest`), so the number is per endpoint rather
than per id.

- `/agent/*` failing: check the Bedrock credentials and the provider error
  counter. The agent degrades to 503 rather than crashing; a 503 here is
  expected when `AWS_BEDROCK_MODEL_ID` is unset.
- `/strategies/{id}/backtest` failing: usually the database, or a window with no
  candles. Check `/readyz` first.
- Everything failing: `/readyz` and the database.

**What this does not mean.** 404s are not errors here. Ownership failures are
404s by design (`docs/12`), so a client asking for someone else's strategy
raises this counter at `4xx`, not `5xx`.

## 3. Kill-switch engaged

**Symptom.** `kill_switch_engaged` or `risk_limit_breached` fires — critical,
page immediately. A `bot.notification` row of kind `killed` is written, and the
decision trail shows `halted`.

**First action.**

1. Read the reason. It names the limit and the value (`daily loss limit
   breached: 3.51R of 3.00R`).
2. Check what happened to any open position: the notification's `positions`
   field says whether it was closed at market (`OnBreach::Close`) or held
   (`OnBreach::Hold`). **Do not assume.** A held position is still open.
3. If the position was held and you want it flat, press the kill-switch in the
   UI (`POST /bots/{id}/kill`), which is reachable even when the agent and the
   market feed are degraded, then close the position at the venue.

**What this does not do.** Tripping the switch is **not reversible by design**
(`crates/trading-engine/src/risk.rs`). Resuming means restarting the bot, which
is the point: an operator should have to decide. There is no "clear and
continue" endpoint, and adding one would defeat the control.

**Afterwards.** The switch tripping is a *result*, not a cause. Find out whether
the strategy is bad or the sizing is. If the paper track record was positive and
live is not, go to [§5](#5-backtest--live-divergence).

## 4. Reconciliation mismatch

**Symptom.** `reconcile_mismatch` fires. `trading_reconcile_mismatches_total`
rose. The mismatch kinds are in `crates/trading-engine/src/execution.rs`:

| Kind | Meaning | First action |
|---|---|---|
| `MissingAtExchange` | We think an order is live; the venue has never heard of it | If a position is open, **the bot already closed it at market** — see below |
| `UnknownToUs` | The venue holds an order we did not place | Do not cancel blind. Check whether the API key was used elsewhere, then cancel manually |
| `StatusDisagrees` | Both know it; states differ | Read the venue's status as authoritative and update our row |
| `QuantityDisagrees` | Both know it; fills differ | The venue's number is the real one |

**The one that is an emergency.** A *protective stop* reported missing while a
position is open is not a bookkeeping difference. The live bot treats it as an
emergency: it cancels what it can and closes at market, recording
`protection_lost`. If you see that outcome, the position is already flat — check
the venue before assuming otherwise.

**What this does not mean.** A terminal order (filled, cancelled) missing from
the venue's open-order list is **not** a mismatch. Open-order lists do not carry
history, and treating their absence as a disagreement makes reconciliation cry
wolf on every completed trade.

## 5. Backtest / live divergence — **no alert exists**

This section is deliberately not in the table above, and the alert it used to
describe (`backtest_live_divergence`) has been removed from the code.

**Why.** `docs/18` names divergence as the strongest signal that the two
execution paths disagree. The rule was implemented over a `trading_divergence_r`
gauge that **nothing wrote**, so it could never fire. A rule that cannot fire is
worse than a missing one: an operator reading the alert list, or this runbook,
would conclude divergence was being watched. `docs/19` row 10 carries the
intent — it needs a job that compares a bot's realised R against its backtest
over the same window, which is a piece of work rather than a threshold.

**What to do if you suspect it anyway.** The investigation below is still the
right one; nothing pages you to it.

1. Diff the decision trails. Both the backtester and the bot write what fired;
   the first decision that differs is where the divergence starts.
2. Check the fills, not the signals: slippage and fees are modelled in the
   simulator and *real* at the venue. A divergence that is small and one-signed
   is usually cost, not a bug.
3. Check the data window. A backtest over a period the collector had gaps in is
   a backtest over different data.

**What it does not mean.** It is not automatically a bug. Real fills, real
latency and real fees move a result.

---

## 6. An order whose fate is unknown

**Symptom.** The log shows `exchange unreachable`, and a placement did not
return an acknowledgement.

**What the platform does, automatically.** It asks the venue what it holds
before doing anything else:

- the order is there → it is adopted, not re-placed;
- the order is not there → it is retried **once**, with the same client order id;
- the venue cannot be asked → the platform **stops** and returns the error.

That last branch is the important one. Not knowing is not permission to place
again, and a second order here would be a second position the platform does not
know it has.

**What you should do.** Find the client order id in the log (it is
`{bot}_{symbol}_{time}_{intent}`), then look for it in `live_orders` and at the
venue. If it exists in both, the retry is done. If it exists at the venue and
not in `live_orders`, record it manually before restarting the bot.

## 7. Database unavailable

**Symptom.** `/readyz` returns 503 with `database: down`.

**What still works.** `/healthz`, market data reads served from memory, and the
running bots. Persistence is a *drain*, not a mirror: a slow database delays the
audit trail, never a trading decision.

**What breaks.** Backtests, strategy/skill writes, and the audit trail. The bot
keeps trading and buffers decisions in memory; a long outage means a long
unflushed buffer, and a crash loses it.

**First action.** Check the managed Postgres instance before touching the
gateway. The gateway deliberately starts with no database rather than
crash-looping, so a 503 here does not mean the process is broken.

## 8. Credential compromise

**Symptom.** A key is believed exposed.

**First action, in this order:**

1. Revoke the key at the venue. This is the only step that actually stops the
   exposure.
2. `POST /venues/binance/revoke` for every affected account, so no bot can place
   another order even if it restarts.
3. Press the kill-switch on every running bot.
4. Only then rotate the environment variable and redeploy.

**What this does not do.** Revoking in the platform does not revoke at the
venue, and vice versa. Both are required, and the venue's is the one that
matters.

**Design note.** The platform does not store exchange keys: they are injected as
environment variables and read at startup. A compromise therefore implies the
deployment environment was reached, not a database row — which changes the
investigation. See the note at the top of
`crates/trading-engine/src/credentials.rs`.

---

## 9. Turning live trading on

Not a failure — a procedure. It is here because the two errors below look like
each other, and like a broken switch, and neither names its own fix.

**Three conditions, and opting in is only the first.** All three must hold before
`POST /bots` with `mode: "live"` starts anything:

1. **Opted in** — `POST /venues/{venue}/opt-in`. Per venue, per account, and
   revocable. Revoking also throws the kill switch on every live bot running
   there, so it is the one button that stops money moving.
2. **Credentials on the API process** — `BINANCE_API_KEY` and
   `BINANCE_API_SECRET` in the deployment environment (`docs/17`). They are never
   stored in the database, and no button in the UI can set them.
3. **A paper track record that passes the gate** — by default **20 closed paper
   trades**, **48 hours** of paper trading, and a cumulative R no worse than
   **−10**. `GET /venues` reports these thresholds, so the panel shows the same
   numbers the gate enforces rather than a copy that can drift.

**`503 EXCHANGE_CREDENTIALS_MISSING`** is condition 2. The message names the
variables. The status is 503 rather than 403 deliberately: nothing about the
request is wrong, the *deployment* is not configured. Check the API process's
environment, not the worker's — the value is read where the request is served.
`GET /venues` answers `credentials_configured: false` for the same reason, and
both derive the variable names from one function, so they cannot disagree.

**`403 LIVE_GATE_REFUSED`** is condition 3. The message lists **every** unmet
condition at once, so one round trip tells you all of them — a strategy that has
never run a paper bot fails on both the trade count and the hours. The duration is
wall-clock: a paper bot that ran for an hour cannot satisfy a 48-hour requirement
however many trades it made.

**What this does not do.** Passing the gate is a statement about a *paper* track
record, not a promise about live behaviour. Live positions are sized against
`ASSUMED_EQUITY = 10_000` because no signed balance endpoint is implemented, and
backtest / paper / live divergence is not measured (`docs/19` rows 9 and 10). The
gate is a floor, not a validation.

**The first live bot, and what to expect in the log.** `POST /bots` logs
`starting a LIVE bot: orders placed by this bot spend real money` at `WARN`, once
per bot actually started. **Two of those lines is two bots** — the endpoint has no
idempotency key yet, so a retried or double-clicked request is a second bot
placing its own orders, not a duplicate that gets ignored (`docs/19` row 16). Read
that count before assuming one click made one bot.

## 10. Sockets keep opening

**Symptom.** `socket_churn` fires, or the log shows repeated `market socket opened`
/ `order book socket opened` lines for the same channels, seconds or minutes apart,
with no closes beside them.

**What the alert is actually claiming.** `> 20 socket openings per minute, sustained
across two consecutive checks` — at the 30s evaluation interval, more than ten new
sockets per check for a full minute. It fires on entering that state and reports
`resolved` on leaving it, so it will not repeat while the loop keeps looping. It
cannot fire on the first evaluation after a boot, and a single round above the
limit is ignored on purpose — see §"What it does not mean" below.

**First, rule out the feed.** The outbound venue feed is a different thing and has
its own line: `market feed connected`. It logs **once per symbol per boot**. If it
appears repeatedly, the gateway is reconnecting to the venue — go to §1 instead.
If it logged twice and the process booted twice, the feed is fine and the churn is
inbound.

**Then read the two numbers, not one.**
```
curl https://<host>/metrics | grep websocket_connections
```
| `websocket_connections` | `websocket_connections_opened_total` | Meaning | Action |
|---|---|---|---|
| flat | climbing | **Churn.** A client in a reconnect loop | Find the client. Usually a dashboard tab with a retry, or a browser waking a backgrounded tab |
| climbing | flat | **Leak.** Sockets are not being reaped | Bug. Check whether the handler task is being dropped |
| flat | flat | Nothing is wrong; those opens were distinct clients | None |

A gauge alone cannot separate the first two: forty distinct clients and one client
reconnecting forty times both leave it at 1.

**Then read the closes.** Every close is logged at `info` with a `reason` and an
`open_for`:
```
socket=market detail=/ws/market/BTCUSDT/5m reason=client closed open_for=214.8ms socket closed
```
`open_for` is the whole point — a socket that lived 200ms and one that lived six
hours otherwise print as the same pair of lines. Reasons: `client closed`,
`stream ended`, `socket error`, `send failed`, `bus closed`, `hello send failed`,
`notice send failed`, `no book within grace`.

**What this is usually not.** It is not the venue throttling you. `/ws/market/*`
and `/ws/orderbook/*` are *inbound* — they are our clients connecting to us, not
us connecting to the exchange. Exchange rate limits apply to the feed in §1.

**What it does not mean.** A handful of opens after a deploy is not churn: every
client reconnects when the process restarts, and that is expected.

## 11. The feed is live but the chart has nothing

**Symptom.** A chart opens and draws nothing, or draws only part of the window it asked
for. `market_data_feed_age_seconds` is small — candles are arriving — but
`market_data_history_bars{symbol=...,timeframe=...}` is still zero.

**This is not the old "table is frozen" incident.** Market data is no longer persisted at
all: the database is a free tier with 6 GB for every symbol of every market, and one
symbol's trades are about 110 MB a day. Recent bars live in RAM and older ones are fetched
from the venue on demand, so there is no stored series to go stale.

**Then, in order:**

1. **Has anything asked for the symbol?** The feed starts lazily — `ensure_feed_for`, from
   `/ws/market/*` and now from `GET /candles`. A gateway that has served nothing has an
   empty buffer, which is correct and not an incident.
2. **Is the *forming* bar the only thing missing?** `/candles` returns the bar being built
   as well as the closed ones. If the right-hand edge of the chart is up to one resolution
   stale, the recorder's trade subscription is not running (`run_binance_feed`).
3. **Is the venue refusing history?** A window older than the buffer costs one REST call
   per thousand bars. A failure is logged at `warn` (`the venue would not serve history for
   this window`) and the route still answers with what RAM had — which is why the response
   carries `source.memory` / `source.venue`. Both zero with a live feed means the feed has
   not produced a single bar; see §1.
4. **Is the window too deep?** One request fetches at most 5000 bars from the venue, and
   clamps from the *newest* end. A five-year `1m` window is served partially, on purpose.

**What it does not mean.** It is not a database problem. Nothing about a chart touches
Postgres any more, and `/candles` answers 200 with an empty list rather than 503 when there
is no data.

## 12. No order book is arriving

**Symptom.** `market_data_book_age_seconds{symbol=...}` rises past 60s. The DOM pane is
empty and `GET /orderbook` answers 404 `NO_ORDERBOOK_DATA`. **Candles are usually fine** —
`market_data_feed_age_seconds` is small and charts draw normally, because candles come from
a different aggregation than the book.

**This is the failure that used to be silent.** On 2026-09-18 a run went the whole way with
the book dead (`docs/19` row 24). The venue's REST snapshot lagged its own diff stream by
~15,700 update ids, so the bridging event had already passed before the subscription
existed, and every later diff was dropped as out-of-order. Nothing crashed, nothing logged,
and the empty DOM read as "no data" rather than "a synchroniser that has been failing for
an hour".

**Then, in order:**

1. **Read the log for the cause.** The collector warns every 100th unbridged diff with
   `snapshot`, `first` and `last` update ids, and logs `order book synced` once. If the
   `first` id is far ahead of `snapshot`, the venue's REST is behind its stream — that is
   the row 24 case, and the 2s resync should clear it within a minute. If there are no
   warnings at all, no diffs are arriving: this is §1 (a dead socket) wearing a different
   hat.
2. **Is the symbol even subscribed?** The age is published only for symbols something has
   waited on — `run_binance_feed` starts the clock when the depth subscription succeeds. If
   that subscription failed, the log says `the depth feed did not start` and there is
   deliberately **no** metric: that is a feed the platform chose not to open, not a book
   that went missing.
3. **Is it one symbol or all of them?** All of them points at the socket or at the venue's
   REST endpoint (`MARKET_REST_URL`). One symbol points at that symbol's snapshot.
4. **Has it recovered?** A book that can recover does so well inside 60s, because an
   unsynced book re-fetches a snapshot every 2s. Sustained breach means it cannot.

**What it does not mean.** It is not a database problem and it is not a trading problem.
Bots trade on candles, not on the book; a dead book is a **degraded panel**, and no position
is at risk because of it. Do not restart the gateway to "clear" it — a restart throws away
the RAM buffer that every chart is reading from (§11), which turns one missing panel into
every chart being empty.

## Deploy and rollback

1. `docker-entrypoint.sh` applies migrations before exec'ing the binary, under a
   Postgres advisory lock, so concurrent replicas serialise rather than race.
2. Migrations are forward-only. A rollback is a redeploy of the previous image
   **plus** a decision about the new tables, not a down-migration.
3. The image has never been built on this workstation (no container runtime —
   see `docs/17`). Before the first real deploy, build it once and confirm
   `/healthz` answers on port 8080.

## What is not covered yet

- **No alert delivery is configured by default.** `ALERT_WEBHOOK_URL` is unset
  in every environment right now, so alerts reach the log and the `audit_log`
  table and nobody's phone. Setting that variable is a deployment task.
- **The churn threshold is sized for today's traffic.** `socket_churn` fires at
  20 openings/min over two checks. That is above what one or two dashboards
  produce and below a client looping every few seconds, but it is not sized for
  50 concurrent users, whose ordinary reconnects would look identical to a loop.
  Raise `max_opens_per_min` with the user count — it is a field on the rule, not
  a constant.
- **No automatic position liquidation on stale data.** The feed-age alert is a
  warning and nothing acts on it. That is deliberate for now — closing positions
  because a websocket hiccuped is its own incident — but it means §1 is a manual
  runbook.
