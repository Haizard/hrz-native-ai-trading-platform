# 52 — Subscriptions, Entitlements & Usage Metering

## The idea

A plan is not a marketing page. It is the answer to one question, asked on
every request that costs the platform something: **what may this user do right
now?** This document defines the five-rung matrix, the subscription lifecycle,
the metered allowances, the routes that enforce them, and the seam a payment
provider will later plug into.

The subscription works end to end today with no payment provider configured.
`POST /billing/subscribe` starts a card-free trial or a period and Postgres is
the system of record. A live merchant account is a later integration, not a
prerequisite for the feature to be real.

## Ground rules

- **The matrix is code, not data.** Plans, prices, limits and feature flags
  live in [`crates/api-gateway/src/billing/plans.rs`](../crates/api-gateway/src/billing/plans.rs).
  A deployment cannot grant a plan whose limits the enforcement code has never
  heard of, and every pricing change is a compile-checked one. The database
  stores only *which* plan a user is on and when the period ends.
- **Negative means unlimited, zero means none.** Every limit is an `i64`
  (`-1` = unlimited). The convention crosses the JSON boundary into a UI that
  renders numbers; an `Option<i64>` would introduce a `null` a client could
  read as `0`.
- **A refusal is 402, not 403.** "You have not paid for this" is a different
  outcome from "this is not yours". The error envelope carries a
  machine-readable code plus the plan that would unlock it, and the frontend
  keys its upgrade prompt off that.
- **Server entitlements are the authority.** The frontend's gates are a hint
  that saves a round trip; every route still checks.
- **No paid feature is advertised before it exists.** Every level in the
  matrix is enforced by a real route. Community indicators used to be a
  reserved row; it is now a working library (`docs/53`) that is included on
  every plan, and the matrix describes what the server actually does.

## The matrix

Prices are in cents, per billing period. Annual is charged once and renews
yearly; the effective monthly column is the annual price divided by twelve,
for comparison copy only.

| | Free | Core | Pro | Elite | Desk |
|---|---|---|---|---|---|
| Monthly | — | $12 | $24 | $36 | $55 |
| Annual | — | $108 | $228 | $348 | $528 |
| Annual / month | — | $9 | $19 | $29 | $44 |
| Panes per layout | 4 | 16 | 50 | 100 | 100 |
| Saved layouts | 3 | 25 | 100 | ∞ | ∞ |
| Price alerts | 5 | 50 | 200 | 500 | 1,000 |
| Zone watchers | 1 | 5 | 20 | 50 | 100 |
| AI credits / period | 25 | 250 | 1,200 | 2,500 | 5,000 |
| Backtests / period | 5 | 50 | 300 | ∞ | ∞ |
| History depth | 31 d | 365 d | 3 y | ∞ | ∞ |
| Concurrent streams | 4 | 16 | 50 | 100 | 100 |
| Paper bots | 1 | 3 | 10 | 25 | 50 |
| Live bots | 0 | 0 | 1 | 3 | 5 |
| Order flow | — | ✓ | ✓ | ✓ | ✓ |
| Advanced order flow | — | — | ✓ | ✓ | ✓ |
| Bring your own model key | — | — | — | ✓ | ✓ |
| Priority support | — | — | ✓ | ✓ | ✓ |
| Dedicated onboarding | — | — | — | — | ✓ |
| Community indicators | ✓ | ✓ | ✓ | ✓ | ✓ + priority |

`max_concurrent` matches `max_panes` on every rung on purpose. The shell keeps
one market stream per pane, and the market socket itself is public
(`docs/12`), so counting sockets would gate data anyone can read without an
account. The pane a signed-in user may open is the part a plan can honestly
limit, and that is the enforced bound. Per-user stream counting on the server
is therefore not a separate check.

Features are booleans except community indicators, whose level is `included`
or `priority`. The library is not a paywall: every plan can browse, install,
publish and manage its own items, as on TradingView, LuxAlgo and MT5. Desk's
`priority` only orders an author's items first in the catalog's featured sort.
No route refuses a community request for plan reasons, so there is no
`PLAN_FEATURE_REQUIRED` on this path. See `docs/53` for the library itself.

## The lifecycle

### States

| State | Access | What put it there |
|---|---|---|
| `trialing` | the paid plan | a first subscribe, inside `TRIAL_DAYS` (14) |
| `active` | the paid plan | a period after trial conversion, renewal, or webhook activation |
| `past_due` | the paid plan through `period_end` | a failed renewal webhook |
| `canceled` | the paid plan through `period_end` | `POST /billing/cancel` or a cancellation webhook |
| `expired` | Free | the period ended without renewal |
| Free | Free | no row, or an unpriced/unknown plan |

`canceled` and `past_due` are deliberately not "no access". A user who
canceled paid through the end of the period, and a failed renewal is a payment
problem, not a reason to cut an active session off mid-month.

### Trial rules

- One trial per user, ever. `subscriptions.trial_used` is the fact; the client
  is not trusted to remember it.
- A plan change during a trial keeps the original trial end and plan period.
  Restarting the clock would let a user buy a second trial by changing rungs.
- When the trial period ends, the trial converts to `active` and a paid period
  begins. There is no card on file to fail.
- Subscribing to Free is refused (`BILLING_REQUEST_INVALID`); reducing access
  is what `cancel` means, and it takes effect at the period end.

### Expiry is reconciled lazily, on read

There is no scheduler flipping rows at midnight. The first read after a
period ends advances the row:

- a running trial becomes `active` and starts its first paid period;
- an `active` plan without a pending cancellation renews (`period_start` moves
  to the old `period_end`, `period_end` advances one cycle);
- an `active` plan with `cancel_at_period_end`, or a `canceled`/`past_due`
  plan, becomes `expired`;
- Free users are untouched and reset on the calendar month.

A user who never opens the app costs nothing to reconcile, and a process that
was down at the boundary has no backlog -- the next read **is** the backlog.
The update is a normal row write, so concurrent reads may race; the loser
writes the same outcome.

### Cancel and resume

- `POST /billing/cancel` sets `cancel_at_period_end`. Access continues until
  `period_end`. With no live paid subscription it is `409 SUBSCRIPTION_REQUIRED`.
- `POST /billing/resume` clears the flag while the period is still running. An
  `expired` row cannot be resumed; the user subscribes again.
- Both are idempotent: canceling twice leaves one flag; resuming a running
  subscription changes nothing.

## Usage metering

### Two clocks

The **subscription** clock decides entitlement. The **usage** clock decides
when counters reset. They share an anchor for a paid user -- the period start --
and a Free user resets on the first of the calendar month. Usage periods are
stored as dates, which is what lets `usage_counters` promise exactly one row
per `(user, period_start, metric)`.

### Period counters vs standing resources

| Kind | Examples | Stored as | Reset |
|---|---|---|---|
| Period usage | AI credits, backtests | `usage_counters.used` | every period |
| Standing resource | panes, layouts, alerts, watchers, bots | a row count | only when deleted |

Mixing them would either reset a user's bots every month or let credits
accumulate forever.

AI credits are weighted by the kind of work. The weights are code
(`plans.rs`) and exposed to the UI through `GET /billing/plans.credit_costs`:

| Action | Credits |
|---|---|
| Quick answer | 1 |
| Author-mode socket turn | 2 |
| Chart analysis | 3 |
| Workspace turn | 3 |
| Revision review | 3 |
| Watcher run | 3 |
| Backtest interpretation | 5 |
| Strategy generation | 8 |

A backtest or parameter sweep is one `backtest` unit, separate from the credits
its interpretation may cost.

### Consumption is atomic

`db::consume_usage` does the check and the increment in a single
`INSERT ... ON CONFLICT DO UPDATE ... WHERE` statement. Reading the counter,
comparing, then writing has a time-of-check-to-time-of-use window, and two
concurrent agent calls are exactly the traffic that finds it: a user with one
credit left must not spend it twice by asking two questions at once. A refused
consumption writes nothing.

Standing-resource caps are counted and checked before insert. A check-then-
insert race remains possible (two tabs can both see one free bot slot); the
consequence is one extra bot, not an extra charge, and it is accepted rather
than hidden behind a global lock.

## Enforcement boundaries

| Route(s) | Gate |
|---|---|
| `GET /footprint`, `/footprint/coverage`, `/bar-delta-stats`, `/delta-by-size` | `orderflow` feature |
| `GET /profile-memory`, `/icebergs`, `/vpin` | `orderflow_advanced` feature |
| `GET /candles` | history window clamped to `history_days` |
| `POST /chart-sessions`, `PUT /chart-sessions/{id}` | panes per layout, saved layouts |
| `PUT /indicator-workspaces/{id}/alerts` | saved alerts (replacing one is not a new alert) |
| `POST /watch-monitors`, enable/update paths | enabled zone watchers |
| `POST /bots` | running/paused paper or live bots by mode |
| `POST /strategies/{id}/backtest`, `POST /compositions/backtest` | one backtest each |
| `POST /agent/ask` | weighted AI credits (chart analysis) |
| `POST /ws/agent/{session_id}` ask/author turns | weighted AI credits |
| `POST /providers` (user model key) | `byo_model_key` feature |
| Concept/indicator backtests | backtest unit + `history_days` clamp |

Anonymous callers on the order-flow reads are treated as Free, so a public
deployment does not leak a paid read through an unauthenticated call. When the
deployment has no database, `state.billing` is `None`: billing routes answer
`503 BILLING_UNAVAILABLE`, and gated routes **degrade** to Free behaviour rather
than refusing to work at all.

### Error vocabulary

| HTTP | Code | Meaning |
|---|---|---|
| 402 | `PLAN_LIMIT_REACHED` | a standing resource is at its cap; `details` names resource, limit, plan |
| 402 | `AI_CREDITS_EXHAUSTED` | the period's weighted credits are spent |
| 402 | `BACKTEST_QUOTA_EXCEEDED` | the period's backtests are spent |
| 402 | `PLAN_FEATURE_REQUIRED` | a boolean feature is above the plan; `details.required_plan` |
| 409 | `SUBSCRIPTION_REQUIRED` | cancel/resume with no live paid subscription |
| 400 | `UNKNOWN_PLAN`, `UNKNOWN_CYCLE`, `BILLING_REQUEST_INVALID` | malformed billing request |
| 503 | `BILLING_UNAVAILABLE`, `PAYMENT_PROVIDER_REQUIRED` | no store, or provider mode with no checkout client |

## Community indicators

The library is implemented and open to every plan (`docs/53`), the way a
script library works on TradingView, LuxAlgo and MT5: an account is the only
requirement.

| Plan | Level | What it gets |
|---|---|---|
| Free | `included` | browse, search, install, update, report, publish, manage own items |
| Core | `included` | same |
| Pro | `included` | same |
| Elite | `included` | same |
| Desk | `priority` | same, plus priority placement in the featured sort |

No community route answers `402 PLAN_FEATURE_REQUIRED`; the only refusals are
ownership, status and the abuse bounds. The author decides whether an item is
public or private at publish time and can switch it later; private items stay
visible to their author only. Those bounds on publishing, versions and installs
(25 / 50 / 100) are in the community routes, not the plan matrix — a shared list
needs a cap per account, and selling a higher cap would sell more spam. The
moderation console is the part still deferred.

`GET /billing/plans` and `GET /billing/subscription` report
`community_indicators_available: true` whenever a database is configured, which
is when the library can serve requests at all. There is no reserved state left
to report.

## The payment-provider seam

### Modes

- `BILLING_MODE=internal` (default): subscribe/cancel/resume act directly on
  the database. Suitable for development, demos and self-hosted installs.
- `BILLING_MODE=provider`: the provider is the system of record. The subscribe
  route refuses (`503 PAYMENT_PROVIDER_REQUIRED`) until a checkout client is
  written; only a signed webhook may grant access.

`BILLING_WEBHOOK_SECRET` is the HMAC-SHA256 key. Neither value is ever logged;
the config's `Debug` prints `<configured>`/`<absent>`.

### Webhooks

`POST /billing/webhook` is public in the HTTP sense -- a provider has no JWT --
and authenticated by the signature instead. The signature is checked over the
**raw bytes before parsing**, in constant time, against
`x-billing-signature` (a `sha256=` prefix is accepted). Parsing first would let
an unauthenticated caller learn the schema from error messages without proving
they can sign anything.

Accepted events:

| `type` | Effect |
|---|---|
| `subscription.activated` | start `active` on the named plan/cycle |
| `subscription.renewed` | start a new `active` period |
| `subscription.canceled` | `canceled`, keep access to `period_end` |
| `subscription.expired` | `expired`, fall back to Free |
| `payment.failed` | `past_due`, keep access to `period_end` |

Unknown kinds are recorded and ignored. Events without a `user_id` (and
non-activation events without a subscription row) are recorded but not applied.

Replay is handled by the insert, not by the handler: `billing_events` has a
unique `(provider, event_id)` constraint, so two concurrent deliveries cannot
both extend a period. The response says `duplicate: true` for the second.

The event shape accepts `id` as an alias for `event_id`, since most providers
spell the dedupe key that way. Activation/renewal events may carry `plan_id`,
`cycle`, `period_end` and `provider_ref`; missing optional fields fall back to
the subscription's current values, and `period_end` defaults to one cycle from
now.

## Subscribe is idempotent

`POST /billing/subscribe` takes `{ plan, cycle?, idempotency_key? }`. The
frontend always sends a client-generated UUID; curl clients may omit it. The
database stores the last key on the row, and a replay with the same key returns
the current entitlements unchanged. A different key is a deliberate new
request and may start a new period.

## Frontend

The side panel's **Plan** tab (`#pane-billing`) renders:

- the current plan, status line, trial/period end, and whether it will renew;
- AI-credit and backtest meters for the current period;
- cancel / resume, or a sign-in prompt when signed out;
- a monthly/annual toggle and the five plan cards, with the current rung
  marked and community indicators shown as `included` on every plan, or
  `included + priority placement` on Desk;
- the billing mode and, when billing is unavailable, why.

The catalog (`GET /billing/plans`) is public, so the cards render before
sign-in. The subscription (`GET /billing/subscription`) needs a token; a signed-
out shell shows Free limits without inventing a subscription.

Client-side gates avoid firing calls the server will refuse: `paneCap()` takes
the smaller of the layout cap and the plan's `max_panes`, the footprint option
is disabled below Core, and the order-flow panel shows a locked sentence
instead of numbers. Every refusal that still arrives -- over HTTP or as an
agent-socket `error` frame -- goes through one `billingUpgradePrompt`, which
deduplicates to at most one toast every four seconds. A 402 is a hint with
`details.required_plan`; it is never silently swallowed.

## What is not here

- Real checkout and card handling: the provider seam exists; the client does
  not.
- A proration engine for mid-period plan changes: an upgrade starts a fresh
  period at the new price. A provider integration will supply proration.
- Server-side per-user concurrent stream counting: the pane cap is the
  enforceable bound, for the reason above.
- The community moderation console: the library and its report trail exist
  (`docs/53`); an operator acts on them through the database until the console
  ships.

## Tests

`crates/api-gateway/tests/billing_flow.rs` drives the real router and database:

- the catalog is public, five plans, Free first, 14-day trial, internal mode;
- a paid plan starts one trial, a replayed key does not extend it, cancel and
  resume toggle the flag;
- Free cannot be "subscribed", unknown plans are 400, cancel without a
  subscription is 409;
- `/delta-by-size` climbs Free → Core and `/vpin` climbs Core → Pro;
- a five-panel layout is 402 on Free and 201 on Core;
- the sixth backtest is 402 on Free, with `used` 5 of 5;
- `/candles` is clamped to 31 days on Free and unlimited on Elite;
- the catalog reports community indicators available, with `included` on Free
  through Elite and `priority` on Desk.

`plans.rs` and `service.rs` carry unit tests for price ordering, the annual
discount, the Free rung, trial conversion, renewal, cancellation expiry, and
the unlimited line. Integration suites that need more than Free's allowance buy
the plan they need (`Harness::register_with_plan`) rather than loosening the
matrix for tests.
