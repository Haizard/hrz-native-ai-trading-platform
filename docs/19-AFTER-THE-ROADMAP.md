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
| 3 | **No retention or downsampling** — **Closed 2026-09-22.** `crates/db/src/retention.rs` (new) holds `RetentionPolicy::from_env` (`RETENTION_TRADES_DAYS` default 30, `RETENTION_ORDERBOOK_DAYS` default 14, `RETENTION_INTERVAL_SECS` default 21600), `run()` in one transaction per pass: `trades` past its age deleted in id-bounded chunks, `orderbook_snapshots` past its age likewise, then the report counts what the pass removed so the log line is evidence rather than intent. `db::spawn_retention_task` runs it at boot and every interval; the first pass runs immediately, because a process that starts onto an over-budget database should not wait six hours to begin. `cargo xtask retention` gives the same pass to an operator without a restart. Tested against a synthetic dataset (`retention.rs::tests`, 7 tests: policy parsing, age bounds, report contents); the job is deliberately candle-free — candles are the one series the chart cannot reconstruct, and the 6 GB budget already names the trades/book tables as the expendable ones. `docs/13:138`'s done criteria now point at the module. (The original ask is preserved in the third column.) | `docs/13`'s own done criteria claim "a documented retention/downsampling job exists and is tested" | A tested job downsamples `trades`/`orderbook_snapshots` past a configured age, verified against a synthetic dataset |
| 5 | **`strategy-cli` reads the source series twice** | `tools/strategy-cli/src/main.rs` | — | The series is loaded once when source resolution is also declared |
| 6 | **The Docker image has never been built** | `docs/17:36-59`; guarded only by `crates/api-gateway/tests/packaging.rs` | The Dockerfile looks production-ready. It has never produced a container | A real Linux build starts and answers `/healthz`; the static checks still pass |
| 7 | **The shell cannot create a concept** — **Closed 2026-09-22.** `frontend/app/builder.js` grew the concept form model: the vocabulary (selectors, comparisons, window bounds, per-document cap) is read from `GET /strategies/schema` like every other dropdown, so a selector added in Rust appears in the editor with no JS change and the builder cannot offer a concept the validator then rejects. `GET /strategies/validate` remains the one parser — the builder round-trips documents through it and keeps anything it cannot model (`lower: {raw: "…"}`) as text rather than guessing, the same graceful degradation the condition parser has. `frontend/app/app.js` renders the editor: per-concept cards (name, side, window, band edges, requirements, min band ratio) with the concepts block created on demand rather than defaulted, `allFormIssues` surfaces concept problems on every keystroke, and the raw-text note says so out loud. `tools/check_builder.mjs` gained concept fixtures exercising emit → parse → emit idempotence, the raw-selector passthrough and the local refusal cases (66 checks, up from 54) | The concept layer reads as finished | A user can define a concept in the UI without writing YAML |
| 9 | **A live bot sizes against a fixed equity** | `crates/api-gateway/src/bot_routes.rs`, `ASSUMED_EQUITY` | Position size is fixed-fractional, so this number *is* the risk per trade — and it is invented | The account's real balance is read from a signed endpoint and used, or the assumption is made explicit in the create request |
| 10 | **Divergence between backtest, paper and live is not measured** | `docs/18` names the alert; nothing computes it | **Fixed in the honest direction 2026-09-16:** `Rule::Divergence` and `observability::metrics::DIVERGENCE_R` were **removed**. They could never fire, and an inert rule reads as coverage — `docs/20` had a runbook for an alert that did not exist in practice | A job compares a bot's realised R against its backtest over the same window and feeds a metric; the rule comes back with the writer, and `there_is_no_rule_for_something_nothing_measures` is updated in the same commit |
| 11 | **No tracing export** | `observability::logging` installs a `tracing_subscriber`; nothing exports spans anywhere | `docs/18` asks for distributed tracing. Request ids are generated and put in a span, and the span goes to stdout | An OTLP exporter, or a written decision that stdout + request ids is the whole tracing story |
| 15 | **The two long soaks are recorded at a bounded duration, not the duration the criteria name** | `docs/04:95` asks for a "24h continuous run"; `02-ROADMAP.md:181` asks for "≥48h". `reports/phase1-verification.md` and `reports/phase6-verification.md` now record real observations of both paths | Rows 12–14 were closed by building the missing mechanism and writing the missing record, which is what those rows were about. The *durations* are a different claim and they have not elapsed: a record that says "observed for N minutes" beside a criterion that says "24h" is honest and still unmet, and it is exactly the shape this document exists to catch | Either the duration elapses on a host that can stay up that long, or the criterion is amended to name the bounded run as the requirement |
| 18 | **`drawThesis` reimplements the price-to-y mapping in JavaScript** | `frontend/app/app.js`'s `drawThesis` computes `scene.plot.y + scene.plot.h - ((price - scene.price_min) / span) * scene.plot.h` — a second implementation of `chart-engine`'s `price_to_y`, and the only arithmetic over market data left in the shell | `docs/14` says "the chart engine calls the same Rust analytics code the backend does, and a second implementation of the trading math in JavaScript must never exist". This is a *presentation* mapping rather than a trading calculation, which is why it survived review — but it is the same shape, and it went from latent to live on 2026-09-17: now that the price axis can be zoomed, a change to `price_to_y`'s padding or to the price floor that is not mirrored here detaches the thesis stop/target lines from the candles they are supposed to mark, silently and by an amount that grows with the zoom | The thesis levels travel in the scene **request** and come back positioned, the way `levels`, `regions` and `profile` already do — so there is no price arithmetic anywhere in the shell, and a test asserts the thesis line's `y` matches the `price_to_y` of its own price |
| 19 | **The AI agent cannot see the user's drawings** — **Closed 2026-09-22, by building the tool the row named.** `crates/ai-agent/src/user_drawings.rs` defines `UserDrawingsSource` — a host-attached trait (`crates/ai-agent/src/agent.rs`'s `AskRequest::with_drawings`), so the agent core takes drawings from whoever constructed the request and never from the wire. `crates/api-gateway/src/market_data.rs` implements it over `db::drawings::list`, and both agent entry points (REST `agent_routes.rs`, WS `ws.rs`) attach it post-authentication. The `get_user_drawings` tool joins the registry, is described in the agent's system prompt, and its result folds into thesis grounding like every other read-only source. The row's premise held: storage and routes were already user-scoped, so no schema change was needed — it was wiring, 130 `ai-agent` tests green | A user draws a level, asks the agent about the chart, and the agent answers as though the chart were blank — while the levels are the most direct statement of what the user is looking at. The refusal is **deliberate but undecided**: `docs/14` says so in those words, which is better than an unstated gap and still not a decision | Either a read-only tool that hands the agent the drawings on the requested symbol (a context change — the storage and the route are already user-scoped, so no schema change), or a written decision that the agent does not read them and why |
| 22 | **`/footprint` and `/orderbook` still read from Postgres, which the gateway no longer writes** | **Fixed 2026-09-18.** Neither reads the database now. `crates/market-data/src/tape.rs` (new) holds a `TradeTape` -- a bounded ring of the last 100,000 trades per symbol, ~6.4 MB -- and a `BookCache` with the newest snapshot per symbol; the feed's recorder task in `run_binance_feed` writes both. `/orderbook` answers from the cache, so it serves the book as it is *now* rather than the last snapshot a pump wrote. `/footprint` builds from the tape, and a window older than it is filled from the venue's aggTrades only up to a 10-minute gap (one REST request per thousand trades, so an hour of a liquid symbol is ~180 requests): beyond that the response carries a `note` saying which part is missing, and with nothing at all on the tape it answers 404 `NO_TICK_DATA` naming the tape's span. `/footprint/coverage` reports the tape's span so a chart can pick a window that works. `/footprint` now also takes its candles from the same `candles_in` helper as `/candles`, so the two charts cannot disagree about a bar | **The limit is real and is not going away:** a footprint is a *recent* chart. 100,000 trades is ~33 minutes of BTCUSDT at ~50 trades/second. Covering more means storing trades, which the 6 GB forbids. If deeper footprint history is ever wanted, the tape size is the knob -- and the cost is RAM, not disk |
| 23 | **A cold start had no instruments to chart, so the buffer could never fill** | `GET /symbols` listed only what the in-memory buffer held, and the buffer is empty on every fresh boot now that market data is not persisted. So the page's symbol and timeframe selects came back empty, the pane had no instrument, and it could never ask for the one request that would have warmed the buffer -- a deadlock, and a regression introduced by closing row 21. **Fixed 2026-09-18:** `market_routes::watchlist()` reads `MARKET_SYMBOLS` (comma-separated, default `BTCUSDT`) once, `GET /symbols` reports the watchlist with the buffer's coverage per symbol rather than the buffer's symbols alone, and an unbuffered symbol still offers the whole standard ladder at zero bars with a `coverage_note` saying it is chartable anyway. `main.rs` starts a feed for each watchlist symbol at boot, so a fresh process warms instead of waiting. `STANDARD_TIMEFRAMES` is now one named constant in `market-data` shared by the candle builder, the buffer and this route, so the three cannot drift | Each extra symbol costs a socket and ~7 MB of RAM (candle buffer + trade tape), not disk. **The fixture gap is closed too:** `tools/shell_check.mjs`'s `/symbols` fixture now carries the whole ladder including `1d`, so the harness reads `1m,5m,15m,1h,4h,1d` and the check fails again if the shell ever stops sorting the options itself (verified by removing the sort). The fixture deliberately keeps the **alphabetical** order the deployment used to send, because a fixture already in ladder order would let the sort pass without ever having run — the harness would be asserting the server's answer rather than the page's. Two comments in `app.js` and `shell_check.mjs` claimed the server still sends alphabetical order; they were stale once `STANDARD_TIMEFRAMES` became the one source, and both now say so |
| 24 | **The order book never synced, and nothing said so** | **Fixed 2026-09-18.** `crates/market-data/src/orderbook.rs`'s `OrderBookSynchronizer` implemented Binance's documented handshake literally: take a REST snapshot at `lastUpdateId = L`, then discard every diff until one satisfies `U <= L+1 <= u`. Measured against live Binance, the REST snapshot returned `lastUpdateId 100299131199` while the first diff the socket delivered was `U = 100299146947` — a gap of **15,748 update ids, roughly 3 seconds** (~530 ids per diff). The bridging event had already been emitted before the subscription existed, so it could never arrive, and every subsequent diff failed the bridge test and was dropped: the book stayed `synced == false` for the entire run. Nothing panicked and nothing logged. `GET /orderbook` answered 404 and the DOM pane was simply empty, which reads as "there is no data" rather than "a synchroniser has been failing for an hour". The synchroniser now **retains** non-bridging diffs instead of discarding them (`MAX_BUFFERED_DIFFS = 4096`, oldest dropped when full) and bridges onto a later snapshot when one arrives; `run_binance_feed` additionally re-fetches a snapshot every `RESYNC_SECS = 2` for as long as any book is unsynced, which is what covers a venue whose snapshot leads its stream instead of lagging it. It also warns every 100th unbridged diff with the `snapshot`/`first`/`last` ids and logs `order book synced` once, so this class of failure can never again be silent. Verified live: synced after 28–29 diffs, 50 bids / 50 asks, spread `0.01` | **The row exists for the finding, not the fix.** 64 unit tests, a clean clippy and a green integration suite all passed while the book was dead; a 22-second run against the real venue found it in one request. Every guard in this repository is aimed at the *shape* of data, and none at whether the data arrived. The number is now there too, and the row is closed: `market_data_book_age_seconds` is published per symbol by `api_gateway::metrics::publish_book_ages` and `Rule::StaleBook` fires on it at 60s, so a missing book is a number a rule can fire on rather than a log line a human has to be reading. It is an **age** rather than a `synced` flag, deliberately — a flag would have to be maintained inside the collector's pump, which owns the synchroniser, and would report the state of the handshake rather than the thing a user notices; an age is measured by the registry that already holds the book and catches every way a book can go missing, not only the one that happened. It is measured from when the symbol was **first waited on** (`LiveRegistry::expect_book`, started by `run_binance_feed` the moment the depth subscription succeeds), because measured only from the newest book a symbol that never synced has no number at all, which is exactly the silence that hid the defect. Runbook: `docs/20` §12 |
| 25 | **The per-symbol cost was sized by arithmetic and had only ever been run at one symbol** | Every number behind closing row 21 — ~1 MB of candle buffer and ~6.4 MB of trade tape per symbol, against 6 GB of database — was derived, not measured, and the platform's stated goal is *many* symbols across *several* markets. A budget exercised only at N=1 has not been tested at the thing it is a budget for. **Witnessed 2026-09-18 at N=3:** `MARKET_SYMBOLS=BTCUSDT,ETHUSDT,SOLUSDT`, three feeds connected, three books bridged (56/73/73 diffs), three orderbooks at 50 bids / 50 asks, `market_data_book_age_seconds` labelled correctly per symbol, `/candles` returning 201 bars per symbol (2 from memory, 199 from the venue) at real prices, and 18 series registered in the buffer. RSS **21 MB** with three feeds open and ~12,000 trades on the tapes. Full record: `reports/multi-symbol-verification.md` | **Three symbols is not sixty, and 85 seconds is not a soak.** The ~8 MB/symbol and ~450–500 MB at 60 symbols in that report are arithmetic again — the same kind of number the report was written to replace. Two design facts came out of it and neither is a defect: one socket per symbol (`ensure_feed_for` builds a collector each; the combined `/stream` endpoint carries trades and depth for *one* symbol, not across symbols), and `BackfillClient` still speaks Binance's klines/aggTrades shape whatever `MARKET_REST_URL` points at, so a second market needs a venue adapter and not just a URL | **Three symbols is not sixty, and 85 seconds is not a soak.** The ~8 MB/symbol and ~450–500 MB at 60 symbols in that report are arithmetic again — the same kind of number the report was written to replace. **Partly closed 2026-09-19:** the venue adapter the row asked for exists. `crates/market-data/src/exchanges/venue.rs` defines `Venue` — path, interval spelling, envelope, row order, column map, and two paging facts (`end_is_inclusive`, `short_page_means_done`) — with `BinanceVenue` and `BybitVenue` both implemented against the venues' own documented APIs, behind 14 unit tests. `BackfillClient` now holds `Arc<dyn Venue>` and reads through it; `new(url)` is kept as `at_venue(BinanceVenue::at(url))` so every existing call site is unchanged. `RawKline::taker_buy_base` is an `Option`, and **Bybit is `None`** — its seventh column is *turnover*, not taker-buy volume, so there is no order-flow split to be had from a Bybit kline; filling it with `volume/2` would have made a volume-delta indicator chart a flat line and a reader take "no imbalance" for a finding. `window.rs::candle_from_kline` was collapsed into `RawKline::to_candle` rather than keeping a second copy of the buy/sell arithmetic. **Also built 2026-09-20: the live half.** A route to Bybit turned out to exist after all — REST answered in 2.7 s and a full WebSocket session to `stream.bybit.com` captured real order-book and trade frames — so the deferral's premise ('cannot be exercised without a route to the venue') was tested rather than repeated, and it was false. The live seam is now `WireCodec` beside `Venue`: a venue describes its socket (`ws_url`, `subscribe_payload`, `heartbeat`) and decodes its own frames, and **one** `Collector<C>` holds the reconnect loop, the bounded diff buffer, the trade-id gap detector, the candle fanout and `MD_CLOSE_LATENCY` — every one of which was a fixed defect, and a second `impl ExchangeCollector` would have duplicated all of them and re-opened each. `BinanceCodec` wraps the existing `wire.rs` unchanged; `BybitCodec` is built from captured frames. Three findings came out of building it and all three were invisible from the documentation: (1) the documented `u == 1` reset boundary is **false for `orderbook.*`** — a fresh Bybit subscribe answers with a full book at a real current id (measured `u = 219167606`) and the next delta continues from it, so the planned `last_update_id = u - 1` bridge would have rejected that delta outright — `u - 1` sits *behind* the snapshot's own levels, `try_bridge`'s filter `first_update_id <= snapshot_id + 1` is false, and `synced` would have stayed false forever, so the first delta would have been applied **zero** times rather than twice, and the screen would have blamed the network; (2) `publicTrade` frames carry **8–16 trades as an array**, so a codec decoding `data[0]` would have kept 1/16 of the trade stream with every field it checked still correct; (3) `S` is the **taker** side, so `is_buyer_maker` is an inversion and getting it backwards flips every delta and CVD figure while every price and size still looks right. Because Bybit carries its book in-band, `subscribe_order_book` makes **no REST call** for it at all — the `BookBootstrap::InBand` setting exists so a venue that could sync on its first frame is not made to poll. **Witnessed live 2026-09-20**, both venues through the same collector: Bybit `order book synced` after 29 diffs with 560 messages, 0 gaps, 1 candle written, and **zero REST requests**; Binance unchanged at 576 messages, 0 gaps, `order book synced` after 21 diffs. The five couplings the row named are gone: `FeedMode` is now a three-name switch with an `is_on()` predicate so the guard cannot drift from the constructor, `run_binance_feed` became a venue-aware `run_market_feed`, `wire.rs` stays honestly Binance-shaped beside `bybit_codec.rs`, `xtask collect --venue bybit` exists, and the three `with_defaults` call sites build a codec explicitly. **Still open:** `exchange_info`/`aggTrades` remain Binance-shaped on purpose, because they feed Binance-schema parsers and swapping only the URL would turn a payload into nonsense |

**Closed, and deleted from the table above** — the rule is to remove a row and note
the commit rather than strike it through. Numbers are not renumbered, because other
docs cite "row 10" and "row 1" by number; 2 and 8 are simply gone.

- **Row 1, the paper bot is not sandboxed** — closed 2026-09-20. `sandbox` was
  declared in `trading-engine/Cargo.toml` and every `sandbox::` occurrence in
  `trading-engine/src` was a comment. The backtester did not use it either.
  `Session<'a>` held a borrow of `Sandbox` (for `limits`, which is `Copy`), so a
  bot could not own a sandboxed strategy. The fix makes `Session` own its limits
  and drops the lifetime, adds `Decisions` (`Native | Sandboxed`) as the seam
  both bots drive, routes `POST /bots` through `Decisions::sandboxed`, and
  records the path in `RunningBot` so the guard
  `a_bot_created_by_the_route_is_sandboxed` can assert it. `docs/08` admitted
  the gap and now notes the backtester is native; `trading-engine/src/lib.rs`
  no longer claims a sandbox path that did not exist.
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
- **Row 20, the footprint's legibility rule implemented in three places** — closed
  2026-09-17, one day after it was written down, which is the shortest life any row in this
  document has had. The shell's `MIN_COLUMN_PX` is gone; `chart-engine::footprint` gained
  `cell_for_font` (the inverse of `font_for_width`) and reports `Grid::min_cell_px`, the
  narrowest a cell can be for *this window's* widest `bid x ask`, and the shell asks the
  engine instead of holding a constant matched by hand against `MIN_FONT_PX` in another
  language. The row was worth writing because the fix that created it was correct — 54 → 64
  kept the font above the floor — and the *reason* it was correct was exactly the thing
  nobody had made checkable. Closing it also made the chart better than it was before the
  defect existed: the derived count is the most that stay legible rather than the most that
  were safe under a guess, so a 794px plot now carries **14 columns where the constant gave
  12**, and the preview renders 19. The engine-side guard is shown to fail by substituting
  `cell_for_font(6.0, …)` for `cell_for_font(MIN_FONT_PX, …)`; the two shell-side guards
  ("the shell goes back to a fixed cell width", "the engine's cell width is never learned")
  are in `tools/guard_check.py` with the rest. Recorded in `docs/14` under "The footprint:
  a ladder, and the font that decides whether it is one".

  The audit's residue is **row 15**: three of those criteria name a *duration*, and a
  bounded observation does not satisfy one. Closing the rows that said "no record exists"
  is not the same as closing the ones that say "this has not run for 24 hours", and
  conflating the two is exactly the failure this document is written against.

- **Row 16, `POST /bots` had no idempotency key** — closed 2026-09-20. A retry made a
  second bot, and for `mode: "live"` each of those placed orders against the real
  account. The existing guard did not help and it is worth being exact about why:
  `BotSupervisor::is_running` keys on the bot id, so it refuses to start *the same bot*
  twice — two requests make two ids, so both pass it. It reads as protection against a
  double start and is protection against a retry of one id. The fix is a client-supplied
  key on `CreateBotRequest`, a unique index on `(user_id, idempotency_key)`
  (`crates/db/migrations/0004_bot_idempotency.sql`), and an insert that resolves the
  conflict in the database rather than by reading first — a check-then-insert has a
  window where two overlapping requests both read nothing and both insert, which is the
  bug surviving its own fix. A retry answers **200** with the bot the first attempt made
  and starts no task; a different key is still a deliberate second bot. Guarded by
  `a_retried_create_makes_one_bot_not_two`, which asserts the **row count** and not only
  the response — the response is what the route *says*, the count is whether a second bot
  exists, and two bots is the thing that costs money. Checked against the defect by making
  the paper path ignore the key: the retry became a second `201` and the **status**
  assertion failed first, so the count assertion is the independent one rather than the one
  that fired. Also guarded by `an_empty_idempotency_key_is_refused_not_ignored`, because an
  empty key is worse than no key: every request carrying `""` is the same request, so the
  first bot a user made would come back for all their later creates.
- **Row 17, the chart cannot zoom, cannot be drawn on, and exists exactly once** —
  deleted 2026-09-20, which is a correction rather than a build. The row's own text ends
  "**the second chart pane, which closes the row**" and had said so since 2026-09-17,
  while the row stayed in the table and the summary below kept listing it as open. Every
  clause of its exit criterion was built and guarded, so by this document's own rule it
  should have been removed and noted then. **This is the failure mode the document exists
  to catch, in the document** — the previous summary had already *named* it and left it,
  on the reasoning that naming an inconsistency is better than quietly fixing it. That
  reasoning is right about the naming and wrong about the leaving: a reader of the table
  sees an open row whose own text says it is closed, and cannot tell which to believe.
- **Row 21, the gateway's live feed is never written to the database** — deleted
  2026-09-20 for the same reason. Its text opens "**Closed 2026-09-18**, by deciding not
  to persist at all", and the summary still listed it as open alongside rows 1 and 3. The
  decision it records (market data is never persisted, under the 6 GB rule) is the
  constraint the whole platform is shaped by; leaving it in the open list understated
  what had been settled.

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
  reason recorded here.** *Not yet.* Rows 2, 4, 8, 12–14 and 20 are closed — 12 and 14 by
  building the mechanism, 13 by writing the records, 4 by carrying the value the aggregate
  was dropping, 20 by moving a number the shell had guessed into the engine that computes it.
  **Rows 16, 17 and 21 were deleted on 2026-09-20**, and two of those three are the reason
  this document needed correcting rather than extending: row 17's own text had said "the
  second chart pane, which closes the row" since 2026-09-17, and row 21's opened "Closed
  2026-09-18", while both stayed in the table and this summary kept listing them as open.
  A previous pass had *named* row 17's inconsistency and left it, on the reasoning that
  naming is better than quietly fixing — right about the naming, wrong about the leaving,
  because a reader of the table sees an open row whose own text says it is closed and has
  no way to tell which to believe. Row 16 is the one real build: a retried `POST /bots`
  made a second live bot.
  Rows 1, 5, 9–11, 15 and 18 are open and recorded, and none has been formally
  accepted as won't-fix — they are a backlog, not a decision. **Rows 3, 7 and 19 were
  closed 2026-09-22** (retention job, concept editor, agent drawings tool — each in its
  own row's text), which is also the day the concept editor closed the last "reads as
  finished but is not" row in the builder.
- **Phase 8B has not started without a recorded go/no-go from Haitham.** *Recorded.* The
  go was Haitham's instruction: "start implementing phase 8 and do not stop until all
  phase is completely done." 8B is built and gated; the funded-account test is the one
  criterion deliberately left to him.
- **Part 3 has at least one completed accuracy investigation with a written
  conclusion.** *Not started.* Part 3 describes a loop nobody has entered. The reference
  strategy still backtests at 26.9% win rate / PF 0.73, and that is still the largest
  open question about this platform.
