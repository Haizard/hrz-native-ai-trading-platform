# 12 — API Gateway

## Purpose
The single REST + WebSocket surface the frontend talks to. Owns authentication, request
routing to internal engines, and real-time fan-out of market/AI data. Built with Axum +
Tokio.

## REST endpoints (representative, extend as needed — document additions here as built)

```
POST   /auth/register
POST   /auth/login
GET    /auth/me

GET    /symbols
GET    /candles?symbol=&timeframe=&from=&to=
GET    /orderbook?symbol=

GET    /skills
POST   /skills
GET    /skills/{id}
PUT    /skills/{id}          # creates a new version, never mutates in place

POST   /strategies                       # create from raw DSL or from agent output
GET    /strategies
GET    /strategies/{id}
PUT    /strategies/{id}                  # stores a NEW version, returns a new id
DELETE /strategies/{id}                  # 409 while any bot still runs it
POST   /strategies/validate              # validate text without storing it
GET    /strategies/reference             # the DSL reference shown in the editor
GET    /strategies/examples              # the documents the editor offers to load
GET    /strategies/schema                # the DSL vocabulary, for the visual builder
POST   /strategies/{id}/validate
POST   /strategies/{id}/backtest
GET    /strategies/{id}/backtests
GET    /backtests/{id}

POST   /bots                             # create paper or live bot from a strategy
GET    /bots/{id}
POST   /bots/{id}/pause
POST   /bots/{id}/resume
DELETE /bots/{id}

POST   /agent/ask                        # natural-language request -> TradeThesis
POST   /agent/generate-strategy          # natural-language -> StrategyDocument
```

### `GET /strategies/schema` — the DSL vocabulary, not a second copy of it

Unauthenticated, because it describes the language rather than anyone's data. It returns
the fields, functions, comparison operators, stop rules, take-profit kinds, document kinds
and timeframes that `strategy-dsl` accepts, and it is generated *from* `strategy-dsl`
(`ALL_FIELDS`, `ALL_FUNCS`, `CompareOp::ALL`, `ALL_STOP_KINDS`, `Timeframe::all`) rather
than hand-listed here.

That is the whole point: the visual builder (`docs/14`) has to offer the same vocabulary
the validator enforces, and the only way to guarantee that is for one side to be derived
from the other. A new stop rule in Rust shows up in the dropdown with no JS change, and
`every_stop_rule_is_described_exactly_once` fails if a rule is added without its
parameter list.

`concepts` is the one member that describes a **shape** rather than a list, because the
client invents the concepts: the most it can serve is what a definition may be made of —
`parts` (each with its `type` and a one-line `reads`), `selectors`, `ops`, `window`,
`max_concepts` and `sides`. It is derived from `analytics_core::concepts` and
`strategy_dsl::expr::ConceptPart` for the same reason. `sides` is deliberately `buy` /
`sell` and not the `Buy` / `Sell` that `analytics_core::Side` serializes as everywhere
else: a document that wrote `Buy` is refused by the validator, so a builder offering it
would be offering a document that cannot be saved.

### `POST /strategies/validate` echoes the document it parsed

The response carries the parsed `document` alongside `valid`/`errors`, so a client can
import a document without parsing YAML itself. The builder opens a strategy this way:
there is exactly one parser in this system, and it is the one `strategy-cli` and the
backtester use. Re-validating an echoed document must agree with the first verdict
(`revalidating_an_echoed_document_agrees_with_itself`).

### A backtest response carries its curve already scaled

`BacktestResponse` adds `equity_plot` beside the stored `report`: the run's cumulative-R
curve fitted to a `0..100` box, with each point's own value, and `zero_y` marking where
flat sits. It is `null` when there is nothing to draw — no trades, or a run stored before
the curve was kept.

It sits *beside* `report` rather than inside it because `report` is the backtester's
document, stored verbatim and handed back untouched; the scaling is the gateway's
presentational addition, and folding it in would mean the stored report is no longer
exactly what the backtester produced.

It exists because `docs/14` forbids arithmetic over market data in the shell. Normalising
a series is arithmetic, and so is finding where zero falls in it — see
`crates/api-gateway/src/plot.rs`, which is where that lives.

## WebSocket channels
```
/ws/market/{symbol}/{timeframe}   -> live candle + MarketState updates
/ws/orderbook/{symbol}            -> live order book ladders (not diffs -- see below)
/ws/agent/{session_id}            -> progress frames, then a thesis (not tokens -- see below)
/ws/bots/{bot_id}                 -> live bot status/trade events, filtered per bot
```
Use binary framing (e.g. a compact serialization like MessagePack or protobuf) for
high-frequency market channels; JSON is fine for the lower-frequency agent/bot channels.

### The agent channel streams the work, not the answer

`/ws/agent/{session_id}` takes the same body as `POST /agent/ask` (`symbol`, `question`,
`skill_id`, `timeframes`) and answers with the same payload, so a client written against
one works against the other. What it adds is what happens in between.

A question takes about a minute: the ladder is read from the database, then the model is
called once per turn and each turn may call tools that read more. Between the question
and the answer the channel now sends `progress` frames — `reading_market`, `thinking`
(with the turn number and whether the run has moved on to answering), `tool`,
`tool_done` (with whether it worked), and `correcting` when a thesis was rejected for
citing a level no tool reported. They are their own frame type rather than `data`,
because a client that mistook one for an answer would draw a thesis out of the agent's
shopping list.

This is progress rather than token streaming, deliberately. The answer is a
`submit_thesis` **tool call**, not prose — the answering phase announces that one tool
and refuses every other, and the nudge it sends says "Do not answer in prose". There is
no answer text to stream, and a token stream would carry narration this design
discards. See `ai_agent::Progress`.

### The bot channel is one socket per bot

`/ws/bots/{bot_id}` filters server-side, so a watcher receives only its own bot's events:
`started`, one `decision` per decision candle (including the ones that did nothing), and
`stopped` with the trade count and any halt reason. Broadcasting rather than polling is
the point — asking the database every second would turn one bot's activity into a query
per second per watcher.

### A stop means the task is gone, and the wait is sized for the database

`stop` (behind `DELETE /bots/{id}` and the shutdown path) sets a flag, **wakes** the task
rather than waiting for it to notice on its next flush tick, and only returns once the
task has finished — or aborts it after `STOP_GRACE`. It is not enough to signal and
return: the task's last act is to flush and write `bot.stopped`, and that event is the
only thing that tells a clean stop from a crash.

The bound is `STOP_GRACE` (30s) and deliberately **not** `flush_interval + 1s`, which was
the old value and which the tests set to 1100ms against a stop measured at ~1200ms. The
flush interval bounds how soon the task *notices*; it says nothing about how long the
write it is already inside takes, and one statement against the managed database measures
around a second. The 100ms margin meant a clean stop was periodically aborted and reported
as a crash.

### The order book is maintained in `market-data`, not rebuilt here

`/ws/orderbook/{symbol}` streams **ladders**: an `OrderBookSnapshot` with each level's
cumulative size and a bar width added, so a depth-of-market panel never sums market data
itself. It is a superset of the snapshot, not a second shape — see `docs/14`. It never
sends diffs, either. Venues do not ship a full
book on every message: they ship periodic diffs and expect the client to keep the book.
So the collector fetches a REST snapshot, subscribes to the diff stream *first* so
nothing is missed, bridges the two, and publishes a snapshot roughly once a second —
and only once the sequence is contiguous, because a half-applied book is worse than no
book. The channel therefore never sends a diff and never sends a book it cannot vouch
for.

It also never streams nothing. If no book arrives within five seconds of the socket
opening, the channel sends a `notice` naming the symbol and the likely cause and closes.
An empty ladder is indistinguishable from a market with no liquidity, and a socket that
never sends looks like a broken client; both are worse than an honest failure.

Depth rides the same connection as trades — the combined stream endpoint is there so one
socket can carry both — so a DOM has a book whenever anything at all is watching the
symbol.

## Auth
- JWT-based session auth for Phase 1 simplicity; keep the auth middleware isolated so it
  can be swapped for OAuth/SSO later without touching route handlers.
- Every route handler receives an authenticated `UserContext`; there is no
  unauthenticated access to any endpoint beyond `/auth/*` and public market data reads
  (if the product decides to allow anonymous chart viewing — decide explicitly, don't
  default to open).
- Two deliberate, explicit exceptions, both of which return no user data: the shell
  itself (`/`, `/app.js`, `/builder.js`, `/chart_engine.wasm`) and
  `GET /strategies/schema`, which describes the DSL rather than anyone's strategies. A
  logged-out visitor can read the vocabulary and load the page; every document, backtest
  and bot behind it still requires a session.

## Versioning rules behind the write routes

Two resource families are **append-only**, and the routes say so in their status codes:

- **Skills.** `POST /skills` publishes the first version, `PUT /skills/{id}` the next.
  Neither ever rewrites a row: a past thesis has to stay explainable against the skill
  version that produced it (`docs/10`). Publishing a version the user already has is
  `409 SKILL_VERSION_EXISTS`, and `PUT` requires the document's own `id()`
  (`{name-slug}-v{major}`) to match the path, or `400 SKILL_ID_MISMATCH`.
  Reads merge two sources: the caller's own rows, then the shipped library under
  `SKILLS_DIR`. Where both answer the same slug, the caller's copy wins.
- **Strategies.** `PUT /strategies/{id}` inserts a **new row** and answers `201` with a
  new `id` plus `supersedes` naming the row it came from, because a stored backtest has
  to keep pointing at the exact document that produced its numbers (`docs/13`).
  `DELETE /strategies/{id}` takes the strategy's backtests with it, and answers
  `409 STRATEGY_IN_USE` while any bot still references it.

`GET /skills` and `GET /strategies` are authenticated: both are user data.

## Rate limiting & backpressure
- Per-user rate limits on `/agent/*` endpoints specifically (LLM calls have real cost).
- WebSocket fan-out must apply backpressure per-connection (drop or coalesce ticks for a
  slow client) rather than allowing one slow consumer to back up the whole broadcast
  pipeline.

## Error contract
- Consistent error envelope across REST and WS:
```json
{ "error": { "code": "STRATEGY_VALIDATION_FAILED", "message": "...", "details": {...} } }
```
- Validation errors from `strategy-dsl` (`docs/06-STRATEGY-DSL.md`) are surfaced with
  their specific field-level detail, not flattened to a generic message — the frontend
  strategy editor and the AI agent's retry loop both depend on this detail.

## Done criteria
- OpenAPI (or equivalent) spec generated from the route definitions and kept in sync via
  a CI check.
- Load test confirms WebSocket fan-out to N concurrent clients does not degrade the
  Market Data Engine's own ingestion latency.
