# 19 — After the roadmap: hardening, known debt, and accuracy

## Purpose

`02-ROADMAP.md` ends at Phase 8. It is a *build* plan, and it was never written to
answer the question that comes the day after: **what do we do once everything is
built?** This document is the answer to that question, and it exists because the
roadmap's silence on it was costing real decisions.

It carries three things, in priority order:

1. **Phase 8, split by risk** — because the phase bundles work that carries no money
   risk with work that carries all of it, and they should not share a go/no-go.
2. **Known debt inside phases already marked done** — every item here is something a
   doc or a comment claims is true and the code does not deliver.
3. **The accuracy loop** — the largest and least-defined category, and the only one
   that decides whether the platform is worth using.

## Status at the time of writing (2026-09-16)

Phases 0–7 are built and verified: workspace and CI, Binance collection and backfill,
`analytics-core` (native + wasm32), the Strategy DSL/runtime/backtester, the WASM
sandbox, the AI agent with 13 tools, the paper-trading bot, and the Rust/WASM chart
with all three editor modes. Last commit reviewed: `7045036`.

**Update, 2026-09-17:** the four rows the backward audit found (4, 12, 13, 14) are closed
in `7045036`, and row 15 records what the audit left behind. See the closure list under
Part 2.

**Phase 8 is built** — both halves, with one exit criterion left open on purpose (see
below). What exists now:

| Piece | Where |
|---|---|
| Metrics, alerts, structured logs | `crates/observability/` (new leaf crate) |
| Prometheus scrape, request middleware, alert task, feed-age sampling | `crates/api-gateway/src/metrics.rs`, `GET /metrics` |
| Collector-side metrics (`MD_*`) | `crates/market-data/src/health.rs`, published on a ticker by `BinanceCollector::connect` |
| Exchange adapter + idempotency + reconciliation | `crates/trading-engine/src/{execution,binance,credentials}.rs` |
| The live bot | `crates/trading-engine/src/live.rs` |
| Live persistence (`live_orders`, opt-in history) | `crates/db/migrations/0002_live_trading.sql`, `crates/db/src/live.rs` |
| The gate and the kill switch | `crates/trading-engine/src/gate.rs`, `POST /bots/{id}/kill` |
| Venue opt-in / revoke over HTTP | `crates/api-gateway/src/venue_routes.rs` |
| Runbooks | `docs/20-RUNBOOKS.md` |
| Load and chaos tests | `crates/api-gateway/tests/load_flow.rs`, `crates/trading-engine/tests/live_flow.rs` |

**Three observability bugs were found by auditing what the code actually writes**, all
of the same shape — a metric or a rule that existed on paper and could not work:

1. `main` injected `Arc::new(Registry::new())` into `AppState` while `trading-engine`
   wrote to `Registry::global()`. `/metrics` served a registry the trading code never
   touched, and the alert task evaluated that same empty one, so
   `kill_switch_engaged`, `risk_limit_breached` and `reconcile_mismatch` could not be
   raised by any event. Fixed by `Registry::global_handle()`.
2. `MD_FEED_AGE` had no writer, so `stale_market_data` could never fire. The gateway now
   samples it per symbol immediately before evaluating the rules, via
   `metrics::publish_feed_ages`.
3. `MD_*`, `AGENT_*`, `BACKTEST_*` and `WS_*` were declared and unwritten. All four
   groups now have writers (the table in `docs/18` maps each to its call site).

And one rule was **removed** rather than left inert — see debt row 10.

**The one exit criterion still open:** a funded account placing and reconciling a real
order. That needs live keys, and the platform will not ask for them — the live path is
proved end to end against a venue double that dedups by client id exactly as Binance
does (`live_flow.rs`), and the funded test is a checkbox for the operator. This is a
deliberate stop, not an omission.

## Part 1 — Phase 8, split by risk

### 8A — No money at risk. Built.

| Item | Source | State |
|---|---|---|
| Metrics per service, structured JSON logs, alerting | `docs/18` | Built. `observability` crate + `/metrics`; JSON logs via `LOG_FORMAT=json` |
| Load/chaos tests | `docs/16` §8 | Built. Fan-out conservation, lagged-reader backpressure, a dead feed resolving rather than hanging, a stop that leaves the feed serving |
| Incident runbooks | `docs/17`, `docs/15` | Built. `docs/20-RUNBOOKS.md`, one section per alert name the code can raise |
| A manual kill-switch reachable from the UI | `docs/15` | Built. `POST /bots/{id}/kill` liquidates and stops; the bots pane has a **Kill switch** button per bot. `GET /venues` + opt-in/revoke are surfaced in the same pane, so "can money move right now, and what stops it" is one panel |

**Exit criterion, restated honestly:** "an induced incident produces the expected alert
inside a defined time budget, diagnosed from a dashboard alone" needs a *dashboard*.
The scrape, the alert rules and the runbooks exist; Grafana does not, because that is a
deployment artifact rather than a crate, and `docs/17`'s deploy story is still unproven
(row 6 below).

### 8B — Real money. Built, and gated.

| Item | Source | State |
|---|---|---|
| `ExchangeAdapter` (`place_order` / `cancel_order` / `reconcile`) | `docs/11` | Built. `crates/trading-engine/src/execution.rs`; Binance spot REST in `binance.rs`, HMAC-SHA256 over the query string |
| Client-generated idempotent order ids; reconciliation that distrusts memory | `docs/11` | Built. `client_order_id` is deterministic and ≤36 chars by construction; `OrderGateway` adopts-or-retries-once-or-stops; `live_orders` makes the database a second line of defence |
| Credentials never logged, scoped to trading | `docs/15` | **Partly.** Never stored, never logged (redacting `Debug`, a test that greps the debug line for the secret), read from the environment. *Encrypted at rest* is not implemented because nothing is at rest — see the note below |
| Per-venue opt-in after a minimum paper track record | `docs/15` | Built. `LiveGate` collects every unmet condition at once; `venue_opt_ins` is append-only so a revoke does not erase why it was ever enabled |

**On "credentials encrypted at rest":** the row is unsatisfiable as written, because the
platform stores no credentials. They live in the API process's environment. The honest
version of the requirement is "the platform must not become a place secrets are kept
without a purpose-built store", and a per-user settings page would need exactly that —
so it is deliberately not offered. `docs/15` carries the corrected wording.

**Still open in 8B:** the funded-account test above, and `ASSUMED_EQUITY` in
`bot_routes.rs` — a live bot sizes against a fixed 10,000 because reading a real balance
needs a signed account endpoint the adapter does not implement. A bot that silently
sizes against an invented number is worse than one that says what it assumed, so it says
so; closing it needs `GET /api/v3/account` and a decision about which asset to size in.

## Part 2 — Known debt inside phases marked "done"

Each row is verified against the tree on 2026-09-16. The "lie" column is what the
repository currently claims.

Rows **12–14** came from an audit of Phases 1–6 against their own deliverables and exit
criteria rather than against the later phases that consume them — which is also what
corrected row 4's framing. Rows 1–11 were found by reading forwards; those three were
found by reading each phase's exit criterion and asking who had ever observed it. **All
four are now closed** and listed below. What the audit left behind is row 15: the part of
it that building code cannot answer, because it is a question about elapsed time.

| # | Item | Where | The lie | Done when |
|---|---|---|---|---|
| 1 | **The paper bot is not sandboxed** | `crates/trading-engine/Cargo.toml` declares `sandbox`; no `sandbox::` call site exists in `crates/trading-engine/src/` or `tools/paper-cli/src/`. `docs/08:236` admits it; `crates/trading-engine/src/lib.rs:5` and `:22` claim the opposite | The doc comment says the strategy runs through "the same `strategy-runtime`/sandbox path the backtester uses". It runs through `strategy-runtime` natively | Principle #6 (AI-authored logic never executes unsandboxed) holds on the paper path: an agent-authored document run by a bot executes inside the sandbox, with an equivalence test proving it matches native |
| 3 | **No retention or downsampling** | `docs/13:138` requires it; `crates/db/migrations/0001_init.sql:189-190` has it as commented-out Timescale suggestions. Nothing in `crates/` or `tools/` mentions retention | `docs/13`'s own done criteria claim "a documented retention/downsampling job exists and is tested" | A tested job downsamples `trades`/`orderbook_snapshots` past a configured age, verified against a synthetic dataset |
| 5 | **`strategy-cli` reads the source series twice** | `tools/strategy-cli/src/main.rs` | — | The series is loaded once when source resolution is also declared |
| 6 | **The Docker image has never been built** | `docs/17:36-59`; guarded only by `crates/api-gateway/tests/packaging.rs` | The Dockerfile looks production-ready. It has never produced a container | A real Linux build starts and answers `/healthz`; the static checks still pass |
| 7 | **The shell cannot create a concept** | `frontend/app/app.js` colours and draws concepts but has no input for one | The concept layer reads as finished | A user can define a concept in the UI without writing YAML |
| 9 | **A live bot sizes against a fixed equity** | `crates/api-gateway/src/bot_routes.rs`, `ASSUMED_EQUITY` | Position size is fixed-fractional, so this number *is* the risk per trade — and it is invented | The account's real balance is read from a signed endpoint and used, or the assumption is made explicit in the create request |
| 10 | **Divergence between backtest, paper and live is not measured** | `docs/18` names the alert; nothing computes it | **Fixed in the honest direction 2026-09-16:** `Rule::Divergence` and `observability::metrics::DIVERGENCE_R` were **removed**. They could never fire, and an inert rule reads as coverage — `docs/20` had a runbook for an alert that did not exist in practice | A job compares a bot's realised R against its backtest over the same window and feeds a metric; the rule comes back with the writer, and `there_is_no_rule_for_something_nothing_measures` is updated in the same commit |
| 11 | **No tracing export** | `observability::logging` installs a `tracing_subscriber`; nothing exports spans anywhere | `docs/18` asks for distributed tracing. Request ids are generated and put in a span, and the span goes to stdout | An OTLP exporter, or a written decision that stdout + request ids is the whole tracing story |
| 15 | **The two long soaks are recorded at a bounded duration, not the duration the criteria name** | `docs/04:95` asks for a "24h continuous run"; `02-ROADMAP.md:181` asks for "≥48h". `reports/phase1-verification.md` and `reports/phase6-verification.md` now record real observations of both paths | Rows 12–14 were closed by building the missing mechanism and writing the missing record, which is what those rows were about. The *durations* are a different claim and they have not elapsed: a record that says "observed for N minutes" beside a criterion that says "24h" is honest and still unmet, and it is exactly the shape this document exists to catch | Either the duration elapses on a host that can stay up that long, or the criterion is amended to name the bounded run as the requirement |

**Closed, and deleted from the table above** — the rule is to remove a row and note
the commit rather than strike it through. Numbers are not renumbered, because other
docs cite "row 10" and "row 1" by number; 2 and 8 are simply gone.

- **Row 2, the stale README status block** — closed in `d787695`. It claimed
  "Phases 0–4 complete — `ai-agent` and `trading-engine` are still stubs", which was
  three phases out of date. A doc that *understates* the build is the kind of thing
  the next agent reads and acts on.
- **Row 8, the kill switch and the venue opt-in having no UI** — closed in `d787695`.
  `POST /bots/{id}/kill` and `POST /venues/{venue}/revoke` existed with no control
  for either. The bots pane now has a Kill switch button per bot and a live-trading
  panel listing each venue, with `opted_in` and `credentials_configured` shown
  **separately** because they fail identically from outside and only one is fixable
  from a browser.
- **Rows 4, 12, 13 and 14** — closed in `7045036`, the four the backward audit found.
  In the order they were fixed:
  - **Row 12, the live collector never persisted candles.** `ExchangeCollector` gained
    a `candle_stream` method (the aggregator lives inside the collector, so the bus was
    unreachable from outside `market-data`), `collect()` drains it through a third pump,
    and the unused `db` dependency is gone. The counter counts **successes**: a counter
    that counts attempts reports a healthy run against an unreachable database.
    `tools/xtask/tests/collect_wiring.rs` parses `collect()` and fails if a subscription
    has no pump — and it was verified to fail against a deliberately re-broken `collect`,
    because a guard that has never fired is not a guard.
  - **Row 4, BOS/CHoCH computed then thrown away.** `MarketState` carries a bounded tail
    of breaks (`structure_breaks`, default 8) plus `bars_since_break`, and five DSL fields
    read them: `market_structure.break`, `.break_direction`, `.break_level`,
    `.break_distance`, `.break_age`. They read `Absent`, not `"none"`, before anything
    breaks — `"none"` would make `!= "choch"` true on an empty chart. `MarketStructure`
    gained `candle_count` in the same change: an index means nothing without the length of
    the slice it indexes into. No shell change was needed, because the agent prompt and
    `GET /strategies/schema` both derive from `ALL_FIELDS`.
  - **Row 14, notifications with no reader.** `GET /bots/{id}/notifications`, plus a
    Notifications button in the bots pane. The route lifts `kind`/`severity`/`title`/`body`
    out of the audit payload and a test builds a **real** `notification_payload` and
    asserts the SQL's keys are all in it, so a rename at either end is a failing test
    rather than a row with an empty title.
  - **Row 13, three exit criteria with no verification record.** Phase 3:
    `strategy-cli verify`, 40 of 219 trades re-derived from the candle table, all passing,
    with six single-field corruptions all caught — `reports/phase3-spot-checks.md`. Phase 1:
    a 51-minute live collector run, 65 candles across five resolutions, 50 of 50 identical
    to Binance's own klines — `reports/phase1-verification.md`. Phase 6: a 30-minute live
    paper run, one decision per closed 5m bar, clean stop — `reports/phase6-verification.md`.

  The audit's residue is **row 15**: three of those criteria name a *duration*, and a
  bounded observation does not satisfy one. Closing the rows that said "no record exists"
  is not the same as closing the ones that say "this has not run for 24 hours", and
  conflating the two is exactly the failure this document is written against.

**Also open, and a decision rather than a task:** whether the concept layer
(`analytics-core/src/concepts.rs`, `regions.rs`) becomes its own roadmap phase. It was
built after Phase 7 and is not in `02-ROADMAP.md`. `02-ROADMAP.md` has not been edited.

## Part 3 — Accuracy: the loop no phase covers

This is the part the roadmap never asks about. **Nothing in Phases 0–8 asks whether the
trading is any good.** A platform can satisfy every exit criterion in `02-ROADMAP.md`
and still lose money, and this one currently would: the reference strategy's own
backtest is **219 trades, 26.9% win rate, profit factor 0.73**. The pipeline is
correct. The strategy is unprofitable. Those are different problems and only one of
them is a bug.

Marked **[P]** = proposed, not yet investigated. **[V]** = verified today.

- **Execution realism [P]** — slippage, partial fills, and fees exist as models, but
  nothing has checked them against a real fill. A backtest that assumes mid-price fills
  on a market order is optimistic by construction.
- **Measurement fidelity [P]** — footprint and delta are only useful if they match what
  a trader sees on a reference terminal. Nobody has diffed our footprint columns
  against an ATAS/Exocharts screenshot of the same window.
- **Backtest ↔ paper ↔ live divergence [V as a gap]** — `docs/18` names this as an
  alert we should have. It is not implemented, and as of 2026-09-16 there is no longer
  even a rule pretending to be it (see debt row 10), so today a bot can silently
  disagree with its own backtest and nothing surfaces it.
- **Cost per agent request [P]** — `docs/18` lists token usage. The Bedrock adapter
  does not surface it, so `AGENT_*` covers latency, tool calls, theses and errors and
  stops short of cost. Not a bug; a number nobody currently has.
- **Base rates [P]** — `backtest_similar_setups` exists as a tool, but "historical win
  rate for similar setups" is only meaningful once "similar" is defined and the sample
  is large enough to mean something.
- **Parameter sensitivity [P]** — a strategy whose edge disappears when the threshold
  moves 10% has no edge. There is no sweep tooling.
- **Look-ahead and causality [V as covered]** — this *is* guarded:
  `a_views_history_ends_at_the_candle_it_decides_on` and the equivalence suite. Do not
  re-litigate it; keep the guards green.

## Part 4 — How to work this document

- One slice at a time. A slice is a row from Part 2 or an item from Part 1/3.
- Every slice gets a test that **fails before the fix and passes after** — the house
  rule from every phase so far, and the reason the debt above is visible at all.
- Close a row by deleting it and noting the commit, not by striking it through.
- Anything that changes an external contract (API route, DSL field, tool signature)
  must be reflected in the owning doc in the same change, per `docs/16`.
- **Part 3 never closes.** It is a loop, not a milestone. Add to it whenever the
  platform is shown to be wrong about the market.
- **Read each phase's exit criteria and ask who has ever observed them.** Rows 12–14
  were found this way, after eleven rows found by reading forwards. A ledger that lists
  what the code fails to do will still miss what nobody tested, and the two directions
  find different things: reading forwards finds a missing writer, reading backwards finds
  a criterion with no witness. Do both before declaring a phase audited.

## Done criteria

Each line carries its own status, because "this document exists" is not "the criterion
is met" — and that distinction is the whole reason this document exists.

- **Phase 8A is live: metrics, tracing, alerts, runbooks, and a chaos test that fires
  them.** *Mostly.* Metrics, alerts, runbooks and the chaos tests are live, and all 24
  metric constants have a writer (audited 2026-09-16 — the audit that found the three
  bugs above). Tracing installs a subscriber and puts a request id in a span, but nothing
  exports spans anywhere: row 11. There is no dashboard, and row 6 is why — a dashboard
  is a deployment artifact and the deployment has never been exercised.
- **Every Part 2 row is either closed or explicitly accepted as won't-fix, with the
  reason recorded here.** *Not yet.* Rows 2, 4, 8 and 12–14 are closed — 12 and 14 by
  building the mechanism, 13 by writing the records, 4 by carrying the value the aggregate
  was dropping. Rows 1, 3, 5–7, 9–11 and 15 are open and recorded, and none has been
  formally accepted as won't-fix — they are a backlog, not a decision.
- **Phase 8B has not started without a recorded go/no-go from Haitham.** *Recorded.* The
  go was Haitham's instruction: "start implementing phase 8 and do not stop until all
  phase is completely done." 8B is built and gated; the funded-account test is the one
  criterion deliberately left to him.
- **Part 3 has at least one completed accuracy investigation with a written
  conclusion.** *Not started.* Part 3 describes a loop nobody has entered. The reference
  strategy still backtests at 26.9% win rate / PF 0.73, and that is still the largest
  open question about this platform.
