# Capability → Tool → Skill → MCP Architecture

**Audit + target design for the AI-native order-flow trading terminal**

Date: 2026-10-04 · Scope: analysis and design only — no code was modified · Method: direct
inspection of the repository at `C:\Users\haizard\Desktop\ai-trading-platform`. Every claim
below cites the file (and where useful the line) it was verified against. Items that could
not be verified are marked **NOT VERIFIED**; things that do not exist are marked
**NOT IMPLEMENTED**; partial work is marked **PARTIAL**.

---

## 1. Executive Summary

The platform is a Rust-first, AI-native order-flow trading terminal: a Binance (and now
partially Bybit) market-data engine, a pure deterministic analytics crate
(`analytics-core`), a Pine-like scripting language (`pine-lite`) and a declarative
Strategy DSL, a WASM sandbox, an LLM agent with a 24-tool registry, a paper/live trading
engine with a hard risk gate, an Axum API gateway, and a vanilla-JS + Rust/WASM chart
workstation. The deterministic-core / LLM-reasoning separation (docs/00 principles #1–#2)
is real and enforced in code, not just documented.

The audit's headline findings:

1. **The bones for a capability architecture already exist in three disconnected places.**
   - Data honesty at the *venue* seam: `Venue` / `WireCodec` abstractions with
     `RawKline::taker_buy_base: Option` and `has_order_flow_split()`
     (`crates/market-data/src/exchanges/venue.rs:301`, `:372`).
   - Data honesty at the *tool* seam: tick-dependent tools return
     `{available:false, note: NO_TICK_DATA}` instead of approximating
     (`crates/ai-agent/src/tools.rs:952-954`, guards at :1076/:1130/:1176/:1216/:1258/:1488/:1533),
     and `get_volume_profile` labels its fallback `"source": "candles"|"trades"` (:1062).
   - Operational honesty at the *deployment* seam: `capabilities.rs` reports
     `ready | not_configured | degraded`, distinguishing *configured* from *verified*
     (`crates/api-gateway/src/capabilities.rs:60-118`).
   What is missing is the layer that **joins these three**: a per-(provider, symbol,
   timeframe) capability model that the agent, the skills, the UI, and a future MCP
   gateway can all query before promising an analysis.

2. **Two load-bearing promises are broken in code today** (both cheap to fix):
   - `BacktestRunner` is **never wired**: `with_backtests` has zero call sites, so the
     `backtest_strategy` and `backtest_similar_setups` tools always answer "no backtest
     runner is attached", and the thesis's `historical_similar_setups` /
     `historical_win_rate` fields are permanently `None` (tools.rs:242; thesis.rs:653-654).
   - User-created skills (DB table `skills`, migration 0001) are **invisible to the
     agent**: the agent's `SkillLibrary` is built only from the shipped YAML files
     (`load_skills`, api-gateway/src/lib.rs:584-609; `resolve_agent`,
     provider_routes.rs:377-381). The `/skills` CRUD API works, but what users create
     there never reaches a thesis.

3. **MCP does not exist.** Zero occurrences in `crates/`, `frontend/`, `docs/`. It must
   be designed as a thin adapter over the existing tool/capability layer — never as a
   new compute engine.

4. **Chart tools exist and are production-wired, contrary to a naive reading of the
   docs**: `create_drawing` / `update_drawing` / `delete_drawing` are capability-gated by
   an optional `DrawingWriter` (tools.rs:169,234,1832-1874), provenance-stamped
   (`created_by:"ai"`, migration 0009), and hidden behind an off-by-default AI layer in
   the shell (app.js:575,2914,4430-4448). What is missing is a **plan → execute → verify**
   loop: the agent writes drawings fire-and-forget, with no re-read verification step.

5. **No dynamic tool selection**: every analysis turn offers all 24 tools +
   `submit_thesis` (agent.rs:648,702). Skills declare timeframes and risk, but not
   *capabilities* — a skill cannot say "I need order flow" or "I accept a tick-derived
   substitute", which is the exact information a multi-provider future needs.

The recommended evolution (detailed in §15–§28): introduce a **Capability Registry**
(§16) as the single join point; evolve the **ToolRegistry** into a capability-aware
registry where every tool declares its data requirements and permission tier (§17); grow
the existing skill documents into **Tool Skills** and **Trading Skills** that declare
required/preferred/optional/fallback capabilities (§18–§19); expose the registry through
**one MCP gateway with modular domains** (§20); and formalize the provider layer around a
**Provider Capability Profile** so Deriv and future providers plug in without touching
the quant engines (§22). The first implementation target (§37) is the capability
vocabulary + provider data profiles + a resolver wired into the existing tool context —
small, additive, and the dependency of everything after it.

---

## 2. Verified Current Architecture

### 2.1 What the system actually is (runtime)

```text
                          ┌──────────────────────────────────────────────────┐
                          │  BROWSER: frontend/app (vanilla JS shell)         │
                          │  index.html + app.js (10,073 lines) + builder.js  │
                          │  drives chart_engine.wasm over a 4-function ABI   │
                          └───────┬───────────────────────────────▲──────────┘
                  REST /candles /footprint /orderflow …           │ scene JSON (positioned
                  /agent/ask  /indicator-workspaces/…             │  geometry; shell never
                  WS /ws/market /ws/orderbook /ws/agent /ws/bots  │  computes prices)
                          ┌───────▼───────────────────────────────┴──────────┐
                          │  api-gateway (one Axum process, one binary)       │
                          │  auth(JWT HS256) · rate limit · routes · ws ·     │
                          │  event_engine · indicator_alerts worker ·         │
                          │  capability report · static shell                 │
                          └──┬───────┬────────┬─────────┬─────────┬──────────┘
              WindowService  │       │ Agent  │ Bots    │ Backfill│ db (Postgres, 6GB free tier)
              (RAM + venue   │       │ (ask / │ super-  │ (Venue  │ users·skills·strategies·
               REST fallback)│       │ gen /  │ visor   │  trait) │ backtests·bots·drawings·
                             │       │ review)│         │         │ workspaces·broker accts·
                ┌────────────▼───────▼────────▼─────────▼──┐      │ provider cfgs·agent memory
                │ market-data (in-process, same process)   │      │ audit_log·venue_opt_ins …
                │ WireCodec per venue (binance|bybit) +    │      └──────────────────────────
                │ generic Collector (reconnect, gaps,      │                  ▲
                │ book sync, candle fan-out)               │      xtask collect/backfill = the
                │  ├─ MarketEventBus (raw lanes)           │      ONLY writers of market data
                │  ├─ HistoryRegistry (1500 bars × 6 TF)   │
                │  ├─ LiveRegistry: TradeTape(100k ring) + │
                │  │  BookCache + 1000-snapshot book hist. │
                │  └─ CandleBuilder (candles FROM trades)  │
                └───────────────┬──────────────────────────┘
                                │ pure functions, no I/O, wasm32-clean
                ┌───────────────▼──────────────────────────┐
                │ analytics-core (the only math)           │
                │ delta·cvd·vwap·volume_profile·footprint· │
                │ imbalance·absorption·liquidity·structure·│
                │ size_classes·bar_delta·vpin·iceberg·     │
                │ profile_memory·regions·concepts·events·  │
                │ sessions·forecast·rsi_divergence·        │
                │ volume_score·resample·indicators(ema,sma,│
                │ rsi,atr)·state(MarketState)              │
                └──────────────────────────────────────────┘
                 ▲ used by: strategy-runtime · backtester · sandbox(-guest) ·
                           pine-lite ta.* · chart-engine (wasm) · ai-agent tools

  strategy-dsl ──► ValidatedStrategy ──► strategy-runtime ──► backtester (replay/report)
       │                                        │                parameter_sweep route
       │                                        ▼
       │                              sandbox (wasmtime, 4-import allowlist,
       │                              fuel/memory/wall ceilings) ◄── sandbox-guest
       ▼
  pine-lite (lex→parse→typecheck→limits→interp) ──► chart script layers,
     studio generation (pine_codegen vet/repair loop), in-chart strategy sim
```

### 2.2 The actual market-data flow (traced in code)

```text
Binance WS  aggTrade / depth@100ms
   │  crates/market-data/src/exchanges/binance_codec.rs (WireCodec impl)
   ▼
Collector (exchanges/collector.rs) — reconnect/backoff, gap detection
   (health.rs counters: MD_* metrics), order-book sync (orderbook.rs),
   candle fan-out via CandleBuilder (candle_builder.rs; LIVE_TIMEFRAMES)
   │ publishes to MarketEventBus (bus.rs) — raw lanes only: trades, closed
   │ candles, book snapshots
   ├─► HistoryRegistry (history.rs) — 1500 bars/(symbol×TF), RAM only, plus the
   │     forming bar kept separately
   ├─► LiveRegistry (tape.rs) — TradeTape 100k-trade ring + BookCache (newest
   │     book) + ~1000-snapshot rolling book history (iceberg input, tape.rs:205-218)
   ├─► BotSupervisor.feed_candle (api-gateway/src/bots.rs:418-460) → paper/live
   │     bots (trading-engine, sandboxed strategy-runtime)
   ├─► event_engine (api-gateway/src/event_engine.rs) — per closed bar,
   │     analytics_core::events::detect_events → derived lane → GET /events,
   │     WS /ws/events/{symbol}
   └─► indicator_alerts worker (indicator_alerts.rs) — workspace alerts on
       bot decisions AND derived events; killzone gating; dedup; QueueSink delivery
```

Older history is **never in the DB by decision** (docs/19 Part 2 note: 6 GB total).
`WindowService` (market-data/src/window.rs) merges the RAM buffer with on-demand venue
REST (`BackfillClient` over the `Venue` trait, backfill.rs:60-117) and drops what it
fetched. The only DB writers of market data are the deliberate CLI paths
(`xtask collect`, `xtask backfill`), plus a retention job that trims the
`trades`/`orderbook_snapshots` tables (db/src/retention.rs).

### 2.3 The actual AI request flow (traced in code)

```text
User types in the workstation chat
  → POST /agent/ask (agent_routes.rs:161)  or  WS /ws/agent/{session} (ws.rs:497)
  → auth: UserContext from JWT (auth.rs:274); per-user token-bucket rate limit
    (rate_limit.rs; AGENT_REQUESTS_PER_MINUTE/AGENT_BURST; 429 + Retry-After)
  → resolve_agent (provider_routes.rs:348-393): user's sealed provider config
    (provider_configs table, AES-256-GCM under BROKER_KEK, scope (user,"ai-provider"))
    else deployment primary from AI_PROVIDER/AI_MODEL/… (providers/mod.rs:168-257)
  → Agent::ask_with_progress (agent.rs:518-866):
      1. select_skill: pinned skill_id, else scored retrieval over the SHIPPED
         SkillLibrary (skills.rs:374-443; agent.rs:1135-1147)
      2. build TimeframeLadder (request → skill → default 1d/4h/1h/5m;
         chart viewport TF promoted in, agent.rs:532-551)
      3. ORCHESTRATOR pre-reads the ladder: analyze_ladder → per-TF MarketState +
         deterministic digest + confluence score (multi_timeframe.rs) — injected
         before the first model call so the model cannot skip the reading
      4. system prompt = base rules + skill render + recalled memories (≤24) +
         ChartContext render (viewport/drawings/screenshot as ILLUSTRATION) +
         ladder digest + drawing capability (only if a writer is attached)
      5. tool loop: ≤6 analysis turns (all 24 tools + submit_thesis) +
         ≤2 answer turns (submit_thesis ONLY, forced ToolChoice; any other tool
         call is refused, not executed — agent.rs:696-703,762-778)
      6. every tool result recorded in trace + folded into PriceRange grounding
      7. submit_thesis → parse_thesis → finalize: clamp confidence, Bias::None
         zeroes levels, reject wrong-side stop/target and risk < 2bp, RECOMPUTE
         risk_reward in Rust, ground every level against observed prices
         ([min*0.8, max*1.25]) — ungrounded → correction turn
      8. chart writes during the loop go through DbDrawingWriter
         (market_data.rs:235-406): kind checked against db::drawings::KINDS,
         anchors validated, provenance {created_by:"ai", confidence, reason}
      9. thesis facts auto-stored to agent_memory (facts_from_thesis)
  → AskResponse { thesis, skill, ladder, trace?, turns, usage } — narrative
    generated from structured fields, never the reverse (docs/09; thesis.rs)
```

### 2.4 One data-flow walk-through (task §4.3)

`BTCUSDT` aggTrade on the live socket → `BinanceCodec::decode` →
`Incoming::Trade(Trade{price, quantity, is_buyer_maker, …})` (timestamps normalized to
ns by the codec, codec.rs:78-86) → `Collector` records health + feeds
`CandleBuilder` (which updates all six live resolutions and closes bars) and publishes
the trade on the bus → `LiveRegistry::record_trade` (tape) → closed 5m bar published →
`HistoryRegistry` append; `BotSupervisor.feed_candle` → bot (if any); `event_engine`
runs `detect_events` on the trailing window → a `liquidity_sweep` event lands on
`/ws/events` → user's chart asks `/candles` (WindowService: RAM buffer; older than the
buffer → venue REST) → user asks the agent "was that a sweep?" → agent's ladder reads
`MarketState` (state.rs:328: candles + tape trades → CVD/VWAP/profile/structure/
liquidity/imbalances/absorption) → `detect_liquidity` tool returns the computed reclaim
verdict (tools.rs:1451-1479) → thesis grounded against those numbers → optional drawing
on the chart with provenance → answer rendered; thesis facts remembered.

---

## 3. Repository Architecture Map

```text
ai-trading-platform/
├── 00-VISION-AND-PRINCIPLES.md / 01-ARCHITECTURE-OVERVIEW.md / 02-ROADMAP.md /
│   03-PROJECT-STRUCTURE.md       # root-level product + phase contract
├── docs/04..37-*.md              # per-subsystem specs (04 market data … 37 primitives);
│                                 # code is truth; docs drift is recorded in docs/19
├── crates/
│   ├── analytics-core/           # THE deterministic quant core (leaf, wasm32-clean, no I/O)
│   │   └── src: types, delta, cvd, vwap, volume_profile, footprint, imbalance,
│   │       absorption, liquidity, market_structure, size_classes, bar_delta, vpin,
│   │       iceberg, profile_memory, regions, concepts, events, sessions, forecast,
│   │       rsi_divergence, volume_score, resample, state(MarketState), indicators/{ema,sma,rsi,atr}
│   ├── market-data/              # ingestion + normalization (seams: WireCodec live, Venue REST)
│   │   └── src: exchanges/{codec,collector,venue,wire,binance_codec,bybit_codec},
│   │       orderbook, candle_builder, bus, health, history (RAM), tape (RAM),
│   │       window (RAM+REST merge), backfill, scanner, symbols
│   ├── strategy-dsl/             # declarative StrategyDocument schema/parser/validator/expr
│   │                             # (ValidatedStrategy is a TYPE gate; deny_unknown_fields)
│   ├── strategy-runtime/         # interpreter: engine, context (MarketContext, no-lookahead),
│   │                             # signal, RollingLadder, Simulator (fill model)
│   ├── backtester/               # deterministic replay + report (R-multiples), replay_golden tests
│   ├── sandbox/ + sandbox-guest/ # wasmtime host (4-import allowlist, fuel/mem/wall ceilings)
│   │                             # + guest (interpret + ABI); build.rs builds & embeds guest
│   ├── ai-agent/                 # LlmClient trait + providers/{bedrock, openai_compat},
│   │                             # tools.rs (ToolRegistry, ToolContext), agent.rs (orchestrator),
│   │                             # skills.rs, thesis.rs, multi_timeframe.rs, chart_context.rs,
│   │                             # agent_memory.rs, user_drawings.rs, progress.rs, sigv4.rs
│   │                             # deps: analytics-core + strategy-dsl ONLY (Cargo.toml:22-26)
│   ├── trading-engine/           # paper.rs + live.rs bots (both Decisions::Sandboxed),
│   │                             # risk.rs (caps/windows/kill-switch), gate.rs (3-condition
│   │                             # live gate), execution.rs (client-order-id dedup, reconcile),
│   │                             # binance.rs (signed REST), credentials.rs (deployment env),
│   │                             # secrets.rs (AES-256-GCM user-key vault), decisions.rs,
│   │                             # store.rs / live_store.rs (decision+trade persistence)
│   ├── api-gateway/              # the single Axum surface; owns AppState wiring
│   │   └── src: main, lib(router), auth, auth_routes, agent_routes, provider_routes,
│   │       skills_routes, strategy_routes, script_routes, indicator_workspace_routes,
│   │       pine_codegen, pine_cheatsheet, indicator_preview, indicator_alerts,
│   │       parameter_sweep, plot, dom, drawing_routes, market_routes, orderflow_routes,
│   │       footprint_routes, scan_routes, tickers, venue_routes, broker_routes, bots,
│   │       bot_routes, market_data (WindowMarketData + DbDrawingWriter), ws,
│   │       event_engine, capabilities, metrics, rate_limit, extract, error
│   ├── db/                       # sqlx layer; migrations 0001..0011; models per domain
│   │                             # (market, repositories, drawings, skills, strategies,
│   │                             # bots, live, broker_accounts, provider_configs,
│   │                             # indicator_workspaces, agent_memory, retention, pump, loading)
│   ├── observability/            # leaf: metrics Registry (global_handle), alert rules +
│   │                             # QueueSink, logging; every crate may depend on it
│   └── pine-lite/                # Pine-like language (leaf): lex, parse, typecheck, limits,
│                                 # interp (per-bar series VM), ta (wraps analytics-core), sim
├── frontend/
│   ├── app/                      # THE workstation: index.html (1679 ln), app.js (10073 ln),
│   │                             # builder.js (1189 ln, visual strategy builder), chart_engine.wasm
│   ├── chart-engine/             # Rust→WASM scene builder (4-fn ABI, no wasm-bindgen):
│   │                             # scene, drawing (16 kinds + registry), script (ScriptDraw
│   │                             # mapping), primitives (PRIMITIVES registry; fib), indicator,
│   │                             # footprint, viewport (gesture math)
│   └── mvp/                      # minimal MVP page (kept as reference)
├── skills/                       # shipped SkillLibrary: footprint/absorption.yaml,
│   │                             # liquidity/sweep.yaml  (exactly two files today)
├── strategies/                   # two reference strategy YAMLs (incl. the base-rated one)
├── tools/                        # xtask (migrate/backfill/collect…), strategy-cli, paper-cli,
│   │                             # agent-cli, + *.mjs/py check harnesses (bedrock, guard, shell,
│   │                             # wasm ABI, smc e2e)
├── reports/                      # verification reports, golden comparisons (this document's home)
├── Dockerfile / docker-compose.yml / docker-entrypoint.sh   # one image; entrypoint migrates then serves
└── target/, target-itest/, node_modules/                    # build outputs (not source)
```

Notable: the workspace dependency rules are documented as *enforced* in
`Cargo.toml:18-31` and `03-PROJECT-STRUCTURE.md:91-118`, and the code honors them
(e.g. `ai-agent` cannot name `trading-engine` types — verified in
`crates/ai-agent/Cargo.toml`).

---

## 4. Existing Data Providers

### 4.1 What exists

| Provider | Live (WS) | Historical (REST) | Status | Evidence |
|---|---|---|---|---|
| **Binance** | `BinanceCodec` + generic `Collector`: aggTrade trades, `@depth@100ms` diffs → synced book; candles built from trades (six live TFs) | `BinanceVenue`: `/api/v3/klines` (12-col, has `takerBuyBaseAssetVolume` → real buy/sell split), `/api/v3/aggTrades` paging, `exchangeInfo` listing | **Implemented, primary** | `market-data/src/exchanges/binance_codec.rs`, `venue.rs` (`BinanceVenue`), `backfill.rs:81-87`, `candle_builder.rs` |
| **Bybit** | `BybitCodec`: spot/linear/inverse sockets, one socket per category, topics instead of stream names; `BYBIT_BOOK_DEPTH` | `BybitVenue`: `/v5/market/kline` (7-col all-strings, **descending** pages, no taker split → `taker_buy_base: None`) | **PARTIAL** (codec + venue exist; not wired into boot — `FeedMode`/env only name Binance; see below) | `exchanges/bybit_codec.rs`, `venue.rs:760-827` tests |
| **Deriv** | — | — | **NOT IMPLEMENTED** | zero matches repo-wide |
| Funding / open interest / liquidations / options | — | — | **NOT IMPLEMENTED** (no streams, no types, no tables) | zero matches in `crates/` |

### 4.2 The two provider seams (both verified)

1. **Live seam — `WireCodec`** (`exchanges/codec.rs`): a venue describes its socket and
   decodes frames; the generic `Collector` owns reconnect/backoff, gap detection, diff
   buffering, and candle fan-out **once**. `Subscription::{Trades, OrderBook}` is the
   whole subscription vocabulary — there is no kline/ticker/OI/funding subscription kind.
   `Frame::{Unrecognised, Malformed, Control}` are deliberately distinct so subscribe-acks
   never kill a pump (codec.rs:26-34).
2. **REST seam — `Venue`** (`exchanges/venue.rs`): `klines_path`, `interval()` (may
   refuse — "this venue has no weekly bars"), envelope decode, row-order normalization,
   page-shape semantics (`short_page_means_done`, `end_is_inclusive`). Crucially
   `RawKline.taker_buy_base: Option<f64>` with `has_order_flow_split()` — a venue that
   cannot supply the split **says so**, and the documented Bybit fallback
   (direction-attributed taker volume) is labeled in-code as *not a measurement*
   (venue.rs:35-47, 301, 372). `BackfillClient` holds `Arc<dyn Venue>`; `new(url)` keeps
   the legacy env contract by selecting a `BinanceVenue` pointed at that host
   (backfill.rs:46-117). Aggregate-trade history is explicitly **Binance-only** until a
   venue supplies one (backfill.rs:327-333). The instrument-listing parser is still
   Binance-shaped (`/api/v3/exchangeInfo`, backfill.rs:160-164) — a known seam.

### 4.3 Boot wiring (what actually runs)

`main.rs:56-74`: `FeedMode::from_env()` (off unless `MARKET_FEED=binance`) →
`BotSupervisor` owns the collector registries; `BackfillClient::new(MARKET_REST_URL |
https://api.binance.com)`; `WindowService::new(bots.history(), bots.live(), backfill)` —
the agent and the chart read through **the same** WindowService. `SymbolIndex` lazily
fetches the venue listing. A feed is evicted after ~3 min idle; freshness is reported
per symbol (`capabilities.rs`, `FRESHNESS_WINDOW=120s`).

### 4.4 The provider gaps that matter for the target design

- No single **provider descriptor**: live capability (WireCodec subscriptions), REST
  capability (Venue), symbol model, and *data-kind availability* are scattered across
  three types and env vars. Adding Deriv means: a new `WireCodec` (tick stream), a new
  `Venue` (or a refusal where candles are served differently), a symbol-model decision
  (synthetic indices are not "symbols" with exchangeInfo), and — the part nothing
  models today — a declaration that **ticks replace trades**, order book is absent, and
  kline volume semantics differ.
- `Subscription` has no Tick/Ticker/OI/Funding variants; `Incoming` has Trade/Book/Candle
  shapes only.
- Live boot names Binance only (`FeedMode::from_env`, main.rs:56-59); Bybit codecs are
  exercised by tests/CLI, not by the running gateway (**PARTIAL**).
- No per-provider *quality* metadata (e.g. aggTrade vs raw trade granularity) survives
  into the analytics boundary — everything becomes `Trade`.

---

## 5. Existing Platform Indicators

The platform's built-in deterministic indicators live in
`crates/analytics-core/src/indicators/` — **exactly four classics plus helpers**:

| Name | Implementation | Input | Output | Chart integration | API | AI-invokable | User-configurable |
|---|---|---|---|---|---|---|---|
| SMA | `indicators/sma.rs:12` `sma(values, period)` | price series | `Vec<Option<f64>>` (None = warm-up) | via pine-lite `ta.sma` (pine-lite/src/ta.rs wraps analytics-core) + chart script layers | none directly | indirect only (inside pine-lite scripts / DSL fields; no `get_sma` tool) | as script input / DSL param |
| EMA | `indicators/ema.rs:16` `ema(values, period)` | price series | `Vec<Option<f64>>` | same | none directly | indirect only | same |
| RSI | `indicators/rsi.rs:17` `rsi(closes, period)` | closes | `Vec<Option<f64>>` | same; plus `rsi_divergence` engine (below) | none directly | indirect (MarketState carries `rsi_divergence`) | same |
| ATR | `indicators/atr.rs:14,38,68` `true_range/atr/atr_percent` | candles | `Vec<f64>` / `Vec<Option<f64>>` | via ta.*; also DSL stop rule `{kind: atr, multiple, period}` (schema.rs) | none directly | indirect (stop rules) | same |
| (helper) mean | `indicators/mod.rs:31` | series | `Option<f64>` | — | — | — | — |

**NOT IMPLEMENTED**: MACD, Bollinger Bands, Stochastic, ADX, and every other classic —
deliberately minimal (docs/05: "included mainly for completeness/parity"), because the
platform's center of gravity is order-flow analytics + user/AI-authored scripts, not a
built-in indicator zoo.

Key architectural facts:

- There is **no "indicator registry" abstraction** for platform indicators: they are
  pure functions. The user-facing indicator surface is **pine-lite** (`plot()` etc.,
  `input.*` knobs) whose `ta.*` builtins call these functions (docs/23:55-58) — one
  math implementation, two presentation paths.
- They are **not exposed as agent tools** one by one; the agent meets them through
  `MarketState` derivatives (`rsi_divergence`, `volume_score`) and through DSL fields.
- Warm-up is `None`, never `NaN` — a load-bearing convention (analytics-core/src/lib.rs:37-39).
- Persistence: none (pure functions). Configuration: per-call parameters; the agent's
  analytics tuning is `MarketStateConfig` (bucket size, lookbacks, session windows —
  state.rs:49-78).

---

## 6. Existing AI Studio System

There is no component *named* "AI Studio" (zero `studio` crate/module; the word appears
as "the studio" = the indicator-workspace chat, pine_codegen.rs:1-3). What exists is a
coherent **generation subsystem** with three artifact kinds and one shared lifecycle:

```text
A. Indicator generation (the "studio"):
   user chat ─► POST /indicator-workspaces/{id}/messages[/stream]
             (indicator_workspace_routes.rs:432/451 → run_workspace_turn:511)
             ─► pine_codegen::generate_script (pine_codegen.rs:533-688)
                · system prompt teaches the real pine-lite surface (+ cheat sheet)
                · model answers via a `submit_script` TOOL CALL (not prose)
                · pine_lite::vet = lex→parse→typecheck→limits, FULL error list
                · deterministic `sec=` autofix (630-650) + repair loop ≤ attempts
                · SSE progress: drafting/repairing/autofix/previewing (docs/36)
             ─► preview replay over stored candles (indicator_preview.rs)
             ─► stored as an indicator_revisions row (append-only; restore/promote)
             ─► shell attaches it as a script LAYER on the pane (docs/25)

B. Strategy generation (documents):
   NL ─► POST /agent/generate-strategy (agent_routes.rs:272)
      ─► Agent::generate_strategy (agent.rs:878-1053): `draft_strategy{yaml}` tool call
         → strategy_dsl::parse_and_validate → errors fed back verbatim ≤3 attempts;
         refuses kind:indicator (indicators are pine-lite now, agent.rs:993-1001)
      ─► NOT persisted by the route; the client saves via POST /strategies
         (created_by: ai_agent)

C. Visual builder (no AI): builder.js — a form over StrategyDocument driven by
   GET /strategies/schema (generated from strategy-dsl itself; one vocabulary source).
   Never parses YAML; unknown grammar degrades to a raw text clause (builder.js:1-26).
```

Lifecycle facts (verified): workspaces/revisions/messages/bot-drafts/alert-preferences
tables (migrations 0005/0006); revisions are **versioned, append-only, restorable,
promotable**; generated artifacts are **user-attributed** (workspace owner); a workspace
revision can be **hand-edited** (scripts route, hand-submitted revisions carry the same
validation record); the **review** endpoint (vision: document vs chart screenshot) exists
(agent.rs:1067-1132; indicator_workspace_routes.rs:1019-1033); **bot-drafts** promote a
workspace to a bot **behind an approve gate** (migrations 0006 + bot_routes); preference
learning records deliberate layer edits as prose notes injected into the next generation
(docs/30); generation feedback is streamed honestly (docs/29/36).

What generated artifacts **cannot** do today (verified):

| Question | Answer |
|---|---|
| Persisted? | Yes — revisions/strategies tables |
| Versioned? | Yes — append-only revisions; strategies carry `created_by` provenance |
| User-specific? | Yes — all rows user-scoped |
| Editable? | Yes — edit-mode generation + hand-submitted revisions + builder |
| Attachable to charts? | Yes — script layers (compose, eye/× chips, settings forms) |
| Callable by AI agents as tools? | **No** — the agent can *produce* them but cannot *invoke* a stored indicator/strategy as a tool |
| Consume quant (order-flow) data? | **No** — pine-lite interp runs on candles (+ pooled `sec=` candles) only; no trades/footprint access (docs/23 execution model; pine-lite/src/interp) |
| Consume market data? | Candles of its pane + requested second instrument; nothing else |
| Call platform indicators? | Yes — `ta.*` wraps analytics-core |
| Shareable between users? | **No** sharing mechanism |
| Backtestable? | DSL strategies: yes (backtester). Pine-lite strategy scripts: **PARTIAL** — in-chart simulation shipped (docs/24 S1–S3: trades/positions/equity panes), full replay parity (S4) pending |

---

## 7. Existing Quant Brain / Quant Engines

All engines live in `analytics-core` (pure, deterministic, native+wasm32, no I/O).
This table is the verified inventory — the "Quant Brain" is a *library*, not a service.

| Engine | File | Public entry | Exact input data | True/Derived | Stateful? | AI tool | Route |
|---|---|---|---|---|---|---|---|
| Delta | delta.rs:16,31 | `calculate_delta(candle)`, `calculate_delta_from_trades` | candle buy/sell split **or** trades | True when split/trades real; derived on Bybit REST (documented fallback) | no | `get_delta` (tools.rs:1314) | via /candles (bv/sv fields) |
| CVD | cvd.rs:35,104 | `calculate_cvd`, `detect_cvd_divergence`, `Cvd` engine | candles (+split) | as delta | `Cvd` engine is incremental | `get_cvd` (:1345) | — |
| VWAP | vwap.rs:25,40,47,62,144 | `calculate_vwap{,_anchored,_from_trades,_series}`, `session_vwaps` | candles(+vol) or trades | true; price+volume only | `Vwap` engine incremental | `get_vwap` (:1363, labels `source`) | — |
| Volume profile (POC/VAH/VAL/HVN/LVN) | volume_profile.rs:142,217 | `calculate_volume_profile(trades)`, `…_from_candles` | trades **or** candles (uniform spread) | True w/ trades; **derived w/ candles and labeled `"source"`** | no | `get_volume_profile` (:1030) | — |
| Footprint | footprint.rs:133,143,164,194 | `build_footprint{,_for_candle,s}`, `build_footprint_from_candle` | trades w/ aggressor side; candle-only variant exists and **refuses to emit imbalances** (:180-200) | True w/ trades; candle variant = coarse volume-at-price only | no | `get_footprint` (:1066; `available:false` w/o trades) | GET /footprint, /footprint/coverage |
| Imbalance | imbalance.rs:109,121,199 | `detect_imbalances{,_with}`, `has_stacked_imbalance` | FootprintCandle (trade-derived) | true | no | `detect_imbalance` (:1526) | — |
| Absorption | absorption.rs:149 | `detect_absorption` | FootprintCandles + candle deltas | true (structurally inert on candle-derived footprints: uniform spread can never reach 2× mean — see §14 P3) | no | `detect_absorption` (:1481) | — |
| Liquidity (levels, sweeps, reclaim) | liquidity.rs:101,153,197,206 | `detect_liquidity_levels{,_with}`, `nearest_*` | candles only | true (price-action derived) | no | `detect_liquidity` (:1396, reclaim verdict computed in Rust :1451-1479) | — |
| Market structure (swings, BOS/CHoCH, trend) | market_structure.rs:185,265 | `find_swings`, `detect_market_structure` | candles only | true | no | `detect_market_structure` (:1569) | — |
| Order-size classes (delta/CVD by S/M/L, classed profiles) | size_classes.rs:189,221 + | `delta_by_size{,_per_candle}`, `calculate_cvd_by_size`, `calculate_volume_profile_for_class` | **trades only** | true | no | `get_delta_by_size` (:1124), `detect_size_divergence` (:1252) | GET /delta-by-size |
| Bar delta stats (intra-bar extremes, intrabar VWAP) | bar_delta.rs:66,101 | `bar_delta_stats{,_window}` | candle + its trades | true | no | `get_bar_delta_stats` (:1170) | GET /bar-delta-stats |
| VPIN (flow toxicity) | vpin.rs:152 + | `calculate_vpin_series`, `latest_vpin`, `bucket_imbalances` | trades (bulk volume classification) | true estimate | no | `get_vpin` (:1210) | GET /vpin |
| Iceberg detection | iceberg.rs:148 | `detect_icebergs` | **ordered L2 snapshots + trades** | true; live-window only (1000-snapshot history) | no | — (no tool) | GET /icebergs (orderflow_routes.rs:504) |
| Profile memory (per-level session delta history) | profile_memory.rs:145 | `build_profile_memory` | candles + trades | true | no | — (no tool) | GET /profile-memory |
| Regions/zones (supply-demand, FVG …) | regions.rs | `detect_zones` | candles | true (price-derived) | no | via concepts/events | chart scene regions |
| Concepts (client-defined patterns as data) | concepts.rs:487,602,693,730 | `validate`, `detect`, `swing_pivots`, `trendline_segments` | candles | true | no | inside DSL documents | preview/indicator routes |
| Derived events (sweep/BOS/CHoCH/FVG/zone/volume/delta) | events.rs:234,517 | `detect_events`, `replay_events` | candles (window diff contract) | true | no (stateless diff) | consumed by event lane | GET /events, WS /ws/events/{symbol} |
| Sessions | sessions.rs | `SessionEngine`, `session_of` | timestamps | true | incremental | inside MarketState | — |
| Forecast cone (stationary bootstrap) | forecast.rs:94 | `bootstrap_forecast` | closes; seeded deterministic; refuses < MIN returns | true (statistical, deterministic) | no | — | chart forecast overlay |
| RSI divergence | rsi_divergence.rs | `latest_rsi_divergence`, `rsi_divergences` | candles | true | no | inside MarketState | — |
| Volume score (aggression × rel. volume) | volume_score.rs | `latest_volume_score`, `volume_scores` | candles (+split) | as split | no | inside MarketState; DSL field | — |
| Resample | resample.rs | `resample{,_all}` | finer candles | exact (lossless up-sampling only) | no | internal | — |
| **MarketState aggregate** | state.rs:328 | `build_market_state` | candles + optional trades + `MarketStateConfig` | mixed — no provenance on the struct (tool edge patches `data_note`, tools.rs:2131) | no | `analyze_timeframe` (:956), `analyze_multi_timeframe` (:965) | — |

**NOT IMPLEMENTED** (verified absent): GEX/options anything, funding, open interest,
liquidations, GEMA (no such identifier anywhere), regime detection beyond
`Trend::{bullish,bearish,ranging}`, statistics library beyond the bootstrap cone, risk
math in analytics (risk lives in trading-engine, not the quant core — correctly).

Cross-cutting: everything is deterministic and pure; nothing is cached (each call
recomputes from the window — fine at current scale, a real cost line later); nothing is
persisted (derived on read); nothing carries quality/confidence metadata except the
`available:false`/`source` patterns at the tool edge.

---

## 8. Existing Chat / Chart Tools

The agent's tool registry today (verified against the dispatch table, tools.rs:454-478;
all implementations in tools.rs). Permissions today are binary: a capability trait is
either attached to `ToolContext` or absent (reported honestly).

| # | Tool | Class | Purpose | Mutates | Permission tier (today) |
|---|---|---|---|---|---|
| 1 | `analyze_timeframe` | quant | MarketState for one TF | no | READ+ANALYZE |
| 2 | `analyze_multi_timeframe` | quant | ladder of states | no | READ+ANALYZE |
| 3 | `get_candles` | market | raw OHLCV+split | no | READ |
| 4 | `get_volume_profile` | quant | POC/VAH/VAL/nodes (labels source) | no | ANALYZE |
| 5 | `get_footprint` | quant | per-price bid/ask cells (refuses w/o trades) | no | ANALYZE |
| 6 | `get_delta_by_size` | quant | S/M/L delta & CVD (refuses) | no | ANALYZE |
| 7 | `get_bar_delta_stats` | quant | intra-bar delta extremes (refuses) | no | ANALYZE |
| 8 | `get_vpin` | quant | flow toxicity (refuses) | no | ANALYZE |
| 9 | `detect_size_divergence` | quant | large/small CVD correlation (refuses) | no | ANALYZE |
| 10 | `get_delta` | quant | per-bar delta from candle split | no | ANALYZE |
| 11 | `get_cvd` | quant | CVD + divergence | no | ANALYZE |
| 12 | `get_vwap` | quant | session VWAP (labels source) | no | ANALYZE |
| 13 | `detect_liquidity` | quant | levels + computed reclaim verdict | no | ANALYZE |
| 14 | `detect_absorption` | quant | absorption events (refuses) | no | ANALYZE |
| 15 | `detect_imbalance` | quant | footprint imbalances (refuses) | no | ANALYZE |
| 16 | `detect_market_structure` | quant | swings/trend/BOS/CHoCH | no | ANALYZE |
| 17 | `backtest_strategy` | research | backtest a DSL doc | no | ANALYZE — **DEAD: runner unwired** |
| 18 | `backtest_similar_setups` | research | skill base rate | no | ANALYZE — **DEAD: runner unwired** |
| 19 | `get_user_drawings` | chart-read | user's own drawings (≤24, read-only) | no | READ |
| 20 | `create_drawing` | **chart-write** | draw object w/ provenance | **chart** | DRAW (gated by DrawingWriter) |
| 21 | `update_drawing` | **chart-write** | move/relabel by id | **chart** | DRAW |
| 22 | `delete_drawing` | **chart-write** | remove by id | **chart** | DRAW |
| 23 | `remember` | memory-write | store fact (symbol/global) | **memory** | (own tier) |
| 24 | `recall_memories` | memory-read | read facts | no | READ |
| 25 | `forget_memory` | memory-write | drop fact by key | **memory** | (own tier) |

Plus the terminal/control tools per mode: `submit_thesis` (ask), `draft_strategy`
(generate), `submit_script` (studio pipeline in the gateway).

Chart-side observations (verified):

- There is **no** `get_chart` / `get_visible_range` / `measure_price` / `find_swing` /
  `inspect_region` tool. The viewport arrives as a **request attachment**
  (`AskBody.chart` → `ChartContext`, chart_context.rs), not as a tool answer — a sound
  design (viewport = hint, never evidence) that the target architecture keeps.
- Drawing vocabulary: 16 kinds shared between toolbar, DB and AI
  (`chart-engine/src/drawing.rs`; `db::drawings::KINDS` pinned by test against
  `DrawingKind::ALL`; third anchor from migration 0011).
- **No verify step**: the agent issues drawing writes inside the tool loop and never
  re-reads the chart; correctness rests on server-side validation +
  kind/anchor checks in `DbDrawingWriter` (market_data.rs:252-309).
- No execution/order tools exist anywhere in `ai-agent` — the crate physically cannot
  name trading-engine types.

---

## 9. Existing Skills

- **Schema** (`ai-agent/src/skills.rs:27-55`): `name, version, category, knowledge
  (prose), rules: Vec<String> (prose), conditions{timeframes[], risk.max_risk_pct},
  examples[], invalidation[], preferred_markets[], preferred_timeframes[]`. Stable id
  `{slug}-v{major}` (skills.rs:105-115). Both shipped files match exactly
  (skills/footprint/absorption.yaml, skills/liquidity/sweep.yaml).
- **Stores**: (a) shipped YAML under `SKILLS_DIR`, loaded at boot (`load_skills`,
  lib.rs:584-609) — the **only** library the agent sees; (b) per-user DB rows
  (`skills` table, db/src/skills.rs; CRUD at skills_routes.rs:151-318; versions
  append-only, duplicate version → 409). The merge exists only in the **read** routes —
  the agent never consults user skills (§14 P2).
- **Retrieval**: deterministic scoring (category +5; market covered +3 / uncovered −2;
  timeframe +2; term matches) — no embeddings (skills.rs:392-443). Agent pins by id or
  takes the single top hit; the skill is rendered into the system prompt, never
  wholesale-stuffed (agent.rs:1264-1271).
- **Versioning discipline**: append-only everywhere; theses cite `skill_used =
  name vVersion` so past theses stay explainable (agent.rs:796).
- **Content character**: skills are *methodology knowledge + rules that reference tool
  outputs* (e.g. the sweep skill names `detect_liquidity.reclaim.long_reclaim.reclaimed`
  and encodes the UNKNOWN-when-no-tick-data honesty rule, sweep.yaml:26). They are **not**
  executable, declare **no data/capability requirements**, and there is no
  "tool skill" (usage doctrine for one tool) or composite "trading methodology skill"
  concept yet. The AI has **no create-skill tool**; users create skills only via REST.

---

## 10. Existing AI Agent Architecture

Verified in `crates/ai-agent` (subagent audit cross-checked by direct reads):

- **Orchestrator** (`agent.rs`, 2670 lines): three entry modes — `ask` (thesis),
  `generate_strategy` (DSL doc), `review_document` (vision review). The ask loop
  pre-reads the timeframe ladder **itself** before the first model call (grounding
  guarantee), runs ≤6 analysis turns + ≤2 forced-answer turns, and refuses non-terminal
  tool calls in the answer phase. This is a **bounded tool loop with a forced terminal
  action** — not a plan-execute-verify loop, not multi-agent (docs/09's decomposition is
  a documented stretch goal, implemented as one agent with separated steps).
- **Grounding**: every tool result feeds a `PriceRange`; `TradeThesis::finalize`
  recomputes derived fields, enforces `Bias::None` semantics ("no trade" is first-class),
  three-way `CheckStatus::{pass,fail,unknown}`, and rejects ungrounded levels
  (thesis.rs:218-311, 393-512). Provenance = full `Vec<ToolTrace>`.
- **Model abstraction**: `LlmClient` trait + two wire families — Bedrock Converse
  (SigV4-signed, vision-model routing) and one Chat Completions adapter serving OpenAI,
  Anthropic, OpenRouter, DeepSeek, Grok, HuggingFace, and any compatible gateway;
  `ProviderId` wire names frozen (providers/mod.rs). Deployment primary from env;
  per-user override sealed server-side. Adding a vendor = one enum variant + a base URL.
- **Cost/limits**: per-user token bucket (30/min + burst 5, env-tunable) on every agent
  path incl. WS and workspace generation; turn caps; lookback clamps (300/2000);
  per-tool caps; screenshot edge validation ≤4 images/≤4 MB before any paid call.
- **Deliberately absent**: provider retries (single `.send()`), token streaming
  (one-shot `complete`; what streams is *work progress* — WS frames + SSE generation
  events, docs/36), context compaction (bounded by turn caps), dynamic tool selection
  (all 24 tools every analysis turn), conversation persistence (stateless asks;
  continuity = `agent_memory` facts, auto-recall ≤24 + auto-store from theses).

---

## 11. Existing ToolRegistry

Structure (tools.rs:270-491): `ToolRegistry { specs: Vec<ToolSpec> }`,
`ToolRegistry::market_analysis()` statically builds 24 specs; `ToolSpec{name,
description, input_schema}`; descriptions are model-facing consts (:838-946); timeframe
enums generated from `Timeframe::all()` so schema/parser cannot drift (:505-524);
dispatch = name check (`UnknownTool` vs `ToolFailed` deliberately distinct) → match to
free async fns; test `every_registered_tool_is_dispatchable` pins spec/dispatch parity.

The part that matters most for the target design is **`ToolContext`**
(tools.rs:141-262): a bundle of *capability traits* — `MarketDataSource`,
`BacktestRunner`, `UserDrawingsSource`, `DrawingWriter`, `MemorySource`,
`MemoryWriter` — each an `Option`, each absent-by-default, each honestly reported when
absent. **This is already a capability-injection mechanism**; it is host-granted
(gateway wires it per authenticated identity), which is exactly the shape a
permission/capability system needs. What it lacks: per-tool *declaration* of data
requirements, quality reporting in a standard shape, permission tiers, and any
registration beyond one static method.

Registration mechanism: compile-time static vec — no dynamic registration, no MCP
exposure, no per-request filtering.

---

## 12. Existing MCP Architecture

**NOT IMPLEMENTED.** Zero occurrences of MCP / Model Context Protocol in `crates/`,
`frontend/`, and `docs/` (verified by search). There is no MCP server, client, SDK
reference, or route. The closest existing assets usable by an MCP layer later: the
`ToolSpec` JSON schemas (already model-facing), the `ToolContext` capability injection,
the capability report (`/capabilities`), and the honest-unavailable result conventions.

---

## 13. Data Requirements and Capability Matrix

### 13.1 The data kinds the platform actually has

| DataKind | Definition in code | Binance live | Binance REST | Bybit live | Bybit REST |
|---|---|---|---|---|---|
| `candles` (with buy/sell split) | `Candle{buy_volume,sell_volume}` (types.rs) built from trades; or kline `takerBuyBaseAssetVolume` | ✅ true (trade-built) | ✅ true | ✅ true (trade-built) | ⚠️ **absent** — `taker_buy_base: None`; documented direction-attributed fallback is *not a measurement* (venue.rs:35-47) |
| `trades` (aggressor-side) | `Trade{is_buyer_maker}` from aggTrade | ✅ (100k-trade RAM ring) | ✅ aggTrades paging — explicitly Binance-only (backfill.rs:327-333) | ✅ public trades | ❌ no adapter |
| `orderbook` snapshots | synced L2, ~1000-snapshot rolling history | ✅ | ❌ (point-in-time only) | ✅ | ❌ |
| `ticks` (no volume / no side) | — | n/a | n/a | n/a | n/a |
| OI / funding / liquidations / options | — | **none anywhere** | | | |

Persistence constraint that shapes everything: live market data is **not persisted**
(6 GB free-tier decision, docs/19); trade history exists only for (a) the live-watched
symbol's recent window and (b) windows someone deliberately backfilled via xtask.
`/footprint/coverage` exists precisely to answer "where do I even have ticks".

### 13.2 Capability matrix (required data → availability → quality)

Legend: **T** = true measurement · **D** = derived/approximate (labeled) · **U** =
unavailable, honestly reported · **—** = not applicable.

| Capability | Required data | Optional | Binance (live) | Binance (backfill-only) | Bybit (REST-only) | Deriv (projected) | Derived? | Limitations today |
|---|---|---|---|---|---|---|---|---|
| Candles / OHLCV | candles | — | T | T | T | T (candles endpoint) | no | none |
| Delta | candle split **or** trades | — | T | T | **D** (fallback, labeled in code, NOT labeled in API) | U unless Deriv candle volume semantics defined | sometimes | Bybit path not surfaced to AI |
| CVD | delta | session config | T | T | D | U | follows delta | — |
| VWAP | price+volume | trades for exactness | T | T | T | D (tick-count VWAP possible — must be declared, not assumed) | no | — |
| Volume profile | trades *or* candles | — | T | T/D (`source` labeled) | D | D (candle spread) | **yes when candles** | uniform spread; fine for POC/VAH/VAL shape, not for per-level order flow |
| Footprint | trades w/ side | book | T | **U** (`available:false`) | U | U (tick-derived variant conceivable: price-level tick counts — different metric, must be a *different capability id*) | never silently | no persistence → live window + deliberate backfills only |
| Imbalance | footprint | — | T | U | U | U | no | — |
| Absorption | footprints + deltas | — | T | U (tool refuses; **MarketState field silently empty — §14 P4**) | U | U | no | — |
| Liquidity (levels/sweeps/reclaim) | candles | — | T | T | T | T | price-derived by nature | — |
| Market structure | candles | — | T | T | T | T | no | — |
| Delta by size / per-class CVD | trades | — | T | U | U | U (tick size classes need volume per tick — Deriv ticks lack it) | no | — |
| Bar delta stats | trades | — | T | U | U | U | no | — |
| VPIN | trades | — | T | U | U | U | estimate by design | — |
| Icebergs | ordered L2 + trades | — | T, **live window only** | U | U (no book history kept yet) | U (no L2) | no | 1000-snapshot rolling window; nothing historical |
| Profile memory | candles+trades | — | T | U | U | U | no | — |
| Regions/zones, concepts, events (sweep/BOS/CHoCH/FVG…) | candles | split enriches | T | T | T | T | price-derived | — |
| Sessions | timestamps | — | T | T | T | T | no | — |
| Forecast cone | closes | — | T | T | T | T | statistical | refuses small windows |
| RSI divergence / volume score | candles (+split for score) | — | T | T | D (score degrades w/o split) | D | — | — |
| Classic indicators (ema/sma/rsi/atr) | closes/candles | — | T | T | T | T | no | — |
| **GEX / options positioning** | options chain + Greeks + OI | — | **U — no such data anywhere** | U | U | U | — | NOT IMPLEMENTED end to end |
| Funding / OI / liquidations analytics | perp streams | — | **U (spot venue only; no streams)** | U | U | U | — | NOT IMPLEMENTED |

### 13.3 Where the platform computes from insufficient data today (the honest list)

1. **Volume profile from candles** — allowed, **labeled** (`"source":"candles"`,
   tools.rs:1062). Correct behavior; the label must become structured provenance.
2. **Bybit REST delta/CVD** — direction-attributed fallback exists in the venue layer
   and is documented there as *not a measurement* (venue.rs:400-410), but nothing
   carries that flag up to the tool/AI layer. **This is the closest the platform comes
   to presenting a derived metric as equivalent — the provenance dies at the seam.**
3. **Absorption inside `MarketState`** — runs over candle-derived footprints when trades
   are absent; uniform spread means the 2× level-volume condition can never fire
   (state.rs:353-365 + absorption.rs), so the field is empty — reading as "no
   absorption" rather than "absorption unknowable". No false positives; a silent
   false-negative class.
4. **Icebergs** — true but live-window-bound; a chart opened minutes ago has thin
   evidence. No "evidence window" is reported with the result (**NOT VERIFIED** whether
   the route reports it — orderflow_routes.rs:504-565 returns `snapshots` count, which
   is the honest proxy; keep).
5. **agent-cli** — its tape is always empty (agent-cli/main.rs:259-272), so CLI agent
   runs always see the no-tick-data answers. Correct but worth knowing in tests.
6. **Candle-derived footprint rendering** — the chart can render candle-derived
   footprints for pre-tick history; `build_footprint_from_candle` refuses to emit
   imbalances (footprint.rs:180-200) and a property test pins it
   (`candle_derived_footprints_never_report_imbalances`). Correct.

### 13.4 The rule the target architecture must encode

> **Availability is a function of (provider, symbol, timeframe, window), not of the
> capability alone — and the answer is never a boolean.** The current code already
> behaves this way per-tool (`trades.is_empty()` guards); the architecture must lift it
> into a queryable, typed layer so the AI, the skills, the UI, and MCP all read the
> same answer.

---

## 14. Current Architectural Problems

Only code-confirmed issues are listed. Severity: **SEVERE** = broken promise in a core
flow · **HIGH** = blocks the target architecture · **MEDIUM** = real debt, livable ·
**LOW** = polish.

| # | Problem | Location (evidence) | Why it matters | Severity | Recommended solution | Can it wait? |
|---|---|---|---|---|---|---|
| P1 | `BacktestRunner` never wired — `backtest_strategy` / `backtest_similar_setups` always answer "no backtest runner is attached"; thesis `historical_*` permanently `None` | tools.rs:116-137,242 (no call sites repo-wide); thesis.rs:653-654 | The explainability pillar ("historical base rate") is dead in the flagship flow; the model is taught tools that can never work | **SEVERE** | Implement `BacktestRunner` in api-gateway over `backtester` + `WindowService` (the `/strategies/{id}/backtest` route already solves data loading — reuse it); reference-strategy map for `similar_setups` from the skill id | **No — fix first, independent of everything else** |
| P2 | User-created skills (DB) invisible to the agent; agent library = shipped YAML only | lib.rs:584-609; provider_routes.rs:377-381; skills_routes merge exists only for reads | docs/10's core promise (user methodology drives the agent) is half-built; /skills CRUD is a write-only store | **HIGH** | Per-request library = shipped ∪ owned (user's copy wins by name), resolved in `resolve_agent` with a cheap cache keyed on user | No — pairs with skill schema v2 |
| P3 | **No capability model**: availability is re-derived ad hoc (`trades.is_empty()`) per tool; no per-provider/per-symbol declaration; nothing a skill, the UI, or MCP can query | tools.rs guards; venue.rs `has_order_flow_split` unused above the data layer | The entire target architecture (data-aware skills, dynamic tools, Deriv, MCP capability answers) depends on this join | **HIGH (foundational)** | Capability Registry (§16): descriptors + provider data profiles + resolver; tools consult it instead of raw emptiness checks | No — it *is* the first implementation target (§37) |
| P4 | `MarketState` carries no provenance/availability; absorption silently empty without trades; profile fallback unlabeled inside the aggregate | state.rs:328-391; render patches `data_note` at the tool edge only (tools.rs:2131) | The AI's main context object cannot distinguish "none" from "unknown" per section | **MEDIUM** | Add a `provenance` block to MarketState (per-section: source, availability, quality); keep render budget | Soon — required by skill data-awareness |
| P5 | Provider model = two partial seams + env vars; live boot Binance-only; `Subscription` lacks ticks/tickers/OI/funding; instrument listing parser Binance-shaped | main.rs:56-74; codec.rs:48-64; backfill.rs:160-164 | Deriv/multi-provider requires a descriptor, not three more traits discovered one defect at a time | **HIGH** | Provider Capability Profile (§22) bundling codec + venue + data-kind matrix + symbol model | Before any second live provider |
| P6 | No dynamic tool selection — all 24 tools + submit_thesis every analysis turn | agent.rs:468,648,702 | Token cost, decision noise, and it does not scale to the ~40+ tools the target adds (chart inspection, research, MCP-bridged) | **MEDIUM** | Skill/capability-scoped tool sets (§17.4, §21) with a safe default | With tool skills |
| P7 | Chart mutations are fire-and-forget: no plan-execute-verify, no post-write re-read | tools.rs:1832-1874; agent loop (agent.rs:518-866) has no verify phase | The design goal "controlled chart interaction" (task §21) is unmet; errors surface as wrong pixels | **MEDIUM** | Verification turn: after writes, re-read via `get_user_drawings` + viewport echo; cap writes per turn; keep provenance + AI layer | With chart tool skills |
| P8 | pine-lite executes on candles only — generated indicators are structurally blind to order flow | docs/23 execution model; pine-lite/src/interp.rs | "AI-generated order-flow study" is impossible without a host-data extension; the studio will hit this wall as users ask for delta studies | **MEDIUM** | Host-provided derived series (delta/cvd of the pane's candles) as script-visible series — data flows in, still no I/O in guest | P2 phase |
| P9 | Market data non-persistence (6 GB decision) makes tick-dependent analytics live-window-only and makes order-flow backtesting impossible | docs/19; history.rs/tape.rs headers | Correct economic decision today; it silently caps Footprint/CVD-class research and any Deriv tick story | **HIGH (constraint, not defect)** | Keep the decision; add *capability windows* to the registry ("footprint: live+72h backfilled") so the limit is visible instead of discovered; revisit with per-provider storage policy | Revisit at provider #2 |
| P10 | No tool-level permission model — tiers exist only as "trait attached or not"; execution is separately and strongly gated, but READ/DRAW/ANALYZE are one undifferentiated grant to any authenticated user | ToolContext; auth.rs; bot_routes/gate.rs for execution | MCP exposure and multi-user roles need scopes; today "logged in" ⇒ full analysis+drawing+memory write | **MEDIUM** | Permission tiers on tool descriptors (§28); execution stays outside the agent entirely | Before MCP |
| P11 | `get_orderbook` tool absent though docs/09 lists it and `/orderbook` exists | tools.rs dispatch (no entry) | Symmetry; book shape is a core order-flow input the agent cannot ask for directly (it gets book-derived signals only) | **LOW** | Add thin tool over BookCache | Anytime |
| P12 | Stale doc comment: user_drawings.rs:11-16 still says "list and nothing else" though `DrawingWriter` exists in the same file | user_drawings.rs:186 vs :11-16 | Doc/code drift in the exact file a future editor will read first | **LOW** | Fix comment | Trivial |
| P13 | No provider retries, no token streaming, no context compaction | bedrock.rs:232; openai_compat.rs:446; docs/36 | Deliberate today; retries matter once MCP/agent calls leave the request path (monitoring) | **LOW** | Add bounded retry w/ jitter in providers when monitoring lands | Yes |
| P14 | Two authoring languages (DSL + pine-lite) — deliberate, documented (docs/23:46-58), but doubles the teaching surface for skills and generation prompts | strategy-dsl, pine-lite | Risk of skills referencing the wrong vocabulary; manage by clear artifact-kind routing in skills | **LOW** | Keep both; skills declare which artifact kind they produce | — |
| P15 | JS-side metric duplication residues: `drawThesis` kept its own price→y mapping (docs/19 row 15); builder.js has a deliberate expr-subset parser (degrades to text) | app.js:2924; builder.js:1-26 | Each is documented and contained; they are the known seam where drift re-enters | **LOW** | Prefer engine-supplied geometry for thesis overlays when touched | Yes |
| P16 | Two LLM-calling endpoints bypass the agent rate limiter; workspace routes use only the deployment primary model, ignoring per-user provider overrides | indicator_workspace_routes.rs:1016-1033 (review), :1180 (bot-draft approve), :545 | A user on their own paid key is silently billed to the deployment; the limiter's cost promise has holes | **MEDIUM** | Route both through `resolve_agent` + `check_agent_limit` | Soon |
| P17 | Public unauthenticated compute: `/strategies/validate`, `/scripts/vet`, `/scan`, `/footprint`, orderflow routes, `/events` have no rate limit; login/register unthrottled | lib.rs:253-299; rate_limit.rs:3-9 (only /agent/*) | Free-tier venue bans (scan saturating per-IP REST budget) and credential stuffing are unpriced | **MEDIUM** | A cheap global/IP bucket on compute routes; login throttling | Before public exposure grows |
| P18 | JWT: 7-day lifetime, no refresh, no revocation, claims = sub/email/exp only | auth.rs:55-62,204-212 | Permission tiers (P10) and MCP scopes need claims to hang on; a leaked token is a week of full access | **MEDIUM** | Add `scope` claim + short-lived tokens when permissions land; revocation list can wait | With P10 |
| P19 | Live equity is an assumed 10 000 (real balance read NOT IMPLEMENTED); graceful shutdown unwired (`BotSupervisor::stop_all` exists, never called); broker ciphertext AAD binds (user, venue) not the account row | bot_routes.rs:55-62; bots.rs:1529-1536 vs main.rs:189-190; secrets.rs:32-37 | Position sizing on live is approximate; SIGTERM can interrupt a live order flow; two same-venue accounts share a crypto scope | **MEDIUM** | Balance read in the live gate; call stop_all on shutdown; extend AAD to account id | Before live money grows |
| P20 | `KNOWN_VENUES = ["binance"]` only, though Bybit codecs exist — venue catalog, opt-in and live paths are single-venue | venue_routes.rs:54 | Provider #2 is blocked at the *product* layer too, not just the data layer | **MEDIUM** | Venue catalog driven by the provider registry (§22), not a const | With provider registry |

Also verified **non-problems** (worth recording so nobody "fixes" them): the honest
`available:false` pattern; the WindowService unification (chart/bot/agent share one
source); the `ValidatedStrategy` type gate; the sandboxed bot decisions
(`Decisions::Sandboxed` is the only path the gateway creates); provenance-stamped AI
drawings with an off-by-default display layer; the capability report's
configured-vs-verified honesty; the `event_engine` watcher belonging to the symbol, not
the feed.

---

## 15. Target Capability Architecture

### 15.1 The model

```text
DataProvider          ──►  declares: ProviderDataProfile (which DataKinds, per symbol class)
(Binance, Bybit,                live + REST + history semantics + known caveats
 Deriv, …)

Normalized data       ──►  the existing pipes: bus / history / tape / WindowService / Venue
(no change)                   (live data stays live, historical stays fetchable — §22.3)

CapabilityResolver    ──►  answers, per (capability, provider, symbol, timeframe, window):
                           available | partial | derived | degraded | unavailable
                           + source + quality + confidence-basis + explanation

CapabilityRegistry    ──►  the catalog: one CapabilityDescriptor per analytical capability;
                           each declares its DataRequirements (required / preferred /
                           optional / forbidden-derived) and which engines implement it

Tools                 ──►  each tool references capabilities it consumes; the registry
                           filters/annotates what the agent is offered and stamps every
                           result with provenance

Skills                ──►  declare required/preferred/optional/fallback capabilities;
                           selected and rendered only when resolvable; unresolved →
                           skill is offered as degraded with named gaps, or refused

AI Agent            ──►  orchestrates: reads capability summary, plans within it, calls
                           tools, verifies mutations, produces grounded theses; NEVER
                           computes, NEVER assumes data the resolver did not vouch for

MCP gateway         ──►  thin protocol adapter exposing the SAME registry + tools +
                           capability answers to external MCP clients; no new compute

Quant engines       ──►  unchanged: analytics-core stays the only math; everything above
                           routes to it, never around it
```

Responsibilities (one line each, expanded in the referenced sections):

| Component | Owns | Does NOT own | § |
|---|---|---|---|
| ProviderAdapter (evolves WireCodec+Venue) | transport, decode, normalize, declare data profile | analytics, storage policy, symbol meaning beyond its venue | §22 |
| CapabilityRegistry | catalog of descriptors + resolution answers | computing anything; caching market data | §16 |
| ToolRegistry (evolved) | tool specs, dispatch, capability annotations, permission tiers | prompting, orchestration | §17 |
| Tool skills | usage doctrine per tool/tool-family (HOW + WHEN-to-call + limits) | trading decisions, selection of other skills | §18 |
| Trading skills | methodology (WHEN/WHY), capability requirements, validation rules, risk defaults | executing tools, computing | §19 |
| Agent | orchestration, grounding, verification, thesis | math, data access internals, execution | §21 |
| MCP gateway | protocol translation, auth scoping, exposure policy | any computation | §20 |
| Quant engines | deterministic math | knowing providers exist | §7 |
| Market Intelligence State | the aggregated per-symbol snapshot + provenance | long-term memory, narrative | §23 |
| Execution | existing trading-engine gate/risk/vault — unchanged | signals (they come from runtime) | §28 |

### 15.2 The three rules that make it cohere

1. **Data availability determines analytical capability — explicitly.** Nothing may
   infer capability from "data happened to be there"; every availability claim routes
   through the resolver with a declared requirement.
2. **True and derived are different capability ids.** A tick-derived footprint is not
   "footprint, degraded" — it is `footprint.tick_derived` with its own descriptor and
   its own quality label, so skills can accept or refuse it by name and the UI can
   label it without special cases.
3. **Everything above the engines is interface.** Tools, skills, agent, MCP — all read
   capability + data; none compute. (Already true for tools; the rule now covers the
   new layers too.)

---

## 16. Capability Registry Design

### 16.1 Vocabulary

```text
DataKind            = candles | candles_with_split | trades | ticks
                    | orderbook_snapshots | open_interest | funding
                    | liquidations | options_chain
                    (closed enum; new kinds = new enum variants + provider profiles)

Availability        = available | partial | derived | degraded | unavailable
                    (partial = some timeframes/symbols; degraded = present but stale
                     or short-window; derived = computed from a lower-fidelity kind,
                     never presented as the true kind)

Quality             = { source: venue|backfill|live|derived,
                        window: (earliest_ns, latest_ns) | live_since,
                        basis: "trades" | "candles" | "ticks" | …,
                        caveats: [string] }         -- human/machine-readable honesty

DataRequirement     = { kind: DataKind, min_window_bars?, needs_aggressor_side?,
                        allows_derived: bool }       -- "forbidden-derived" = allows_derived:false

CapabilityDescriptor = {
  id: "volume_profile" | "footprint" | "footprint.tick_derived" | …,
  summary, category (orderflow|structure|volatility|statistical|positioning|…),
  inputs: [DataRequirement],          -- required
  preferred: [DataRequirement],       -- upgrades quality when present (e.g. trades for profile)
  optional: [DataRequirement],        -- enriches (e.g. book for iceberg context)
  implemented_by: "analytics_core::volume_profile::calculate_volume_profile",
  exposed_via: { tool: "get_volume_profile", routes: ["/footprint"] , chart_pane?: …},
  cost: cheap|moderate|expensive,     -- informs monitoring budgets, not correctness
  notes: [string]                     -- known caveats surfaced to skills/UI
}

ProviderDataProfile = {
  provider: "binance" | "bybit" | "deriv",
  symbol_classes: { "spot": {kinds…}, "linear": {kinds…}, "synthetic_index": {kinds…} },
  per kind: { live: bool, rest: bool, history_window: unbounded|paged|none,
              split: real|attributed|absent, granularity: aggtrade|trade|tick },
  caveats: [string]
}

Resolution          = resolver(descriptor, provider, symbol_class, timeframe, window)
                      -> { availability, quality, missing: [DataRequirement],
                           explanation: string }     -- explanation is FOR the AI/UI
```

### 16.2 Placement (fits the crate rules)

- New leaf crate **`crates/capabilities`**: the vocabulary + descriptors + resolver —
  pure types and pure logic, no I/O, depends only on `analytics-core` types. It may not
  depend on market-data, ai-agent, db, or api-gateway (same discipline as
  analytics-core; keeps it wasm-clean and testable).
- **Provider profiles** are *declared by* market-data (each venue/codec contributes its
  profile; that is the only place the truth about a venue lives) and *assembled by*
  api-gateway at boot into a resolver instance handed to the agent, the routes, and
  (later) the MCP gateway. This mirrors the existing pattern: ai-agent declares
  capability *traits*, api-gateway supplies the implementations.
- The existing `/capabilities` deployment report (capabilities.rs) is **extended, not
  replaced**: it gains a per-(provider, capability) section driven by the same
  resolver, so "configured vs verified" (ops view) and "available vs derived"
  (analysis view) are two readings of one registry.

### 16.3 Integration points (who calls it)

| Caller | Question it asks | Today |
|---|---|---|
| Agent system-prompt builder | "capability summary for (symbol, ladder TFs)" — one compact table | nothing (model discovers by calling) |
| ToolRegistry dispatch | annotate each result with `{capability, availability, quality}`; refuse early with explanation | per-tool `trades.is_empty()` guards (kept as the enforcement floor) |
| Skill selection | "is this skill's required set resolvable here?" → select / degrade / refuse with named gaps | nothing — skills are data-blind |
| `/capabilities` route | render the analysis-view section | deployment view only |
| Chart shell (later) | gray out footprint/iceberg panes on symbols with no trades, with the explanation | nothing |
| MCP gateway (later) | answer `list_capabilities` per server | n/a |

### 16.4 Update/recompute

Resolution is **cheap and pure** — no market data is read, only the provider profile
plus the freshness facts the gateway already tracks (feed ages, tape spans,
`/footprint/coverage`). Recompute per agent request and per `/capabilities` call; cache
the assembled profiles (they change only on boot or provider config change). Dynamic
facts (window depth, staleness) come from the existing `HistoryRegistry`/`LiveRegistry`
spans — the same source `/footprint/coverage` already uses.

### 16.5 AI access

The agent sees capabilities **three ways**, in order of cost: (1) the compact summary
in the system prompt (per request, ~1 table); (2) a `get_capabilities` tool for
on-demand detail ("why is absorption unavailable for ETHUSDT on 1m?") — the
explanation string is written for exactly this consumer; (3) stamped on every tool
result. The model never needs to guess, and skills never need to embed per-provider
knowledge in prose.

---

## 17. Tool Architecture

### 17.1 Tool types (target taxonomy, mapped onto what exists)

| Type | Contract | Examples (existing → target) |
|---|---|---|
| Market-data read | fetch raw data within a window; no math | `get_candles` (+ `get_orderbook` — P11) |
| Quant analysis | thin wrapper over analytics-core engines | the 14 orderflow/structure tools |
| Research | runs backtests/sweeps; async-capable | `backtest_strategy`, `backtest_similar_setups` (wire them) |
| Chart read | inspect chart state/objects | `get_user_drawings` (+ `inspect_region`, `measure_range` — new) |
| Chart write | mutate chart objects w/ provenance | `create/update/delete_drawing` |
| Memory | user/agent memory CRUD | `remember`/`recall_memories`/`forget_memory` |
| Generation | draft artifacts (strategy YAML, pine script) via repair loops | `draft_strategy`, `submit_script` |
| Control | terminal answers | `submit_thesis` |
| Execution | **NOT a tool type. Execution stays outside the agent.** | — |

### 17.2 The evolved ToolRegistry

Keep: `ToolSpec{name, description, input_schema}`, match dispatch, the
parity test, `ToolContext` capability injection. Add per tool:

```text
ToolDescriptor = {
  spec: ToolSpec,                        -- unchanged shape (MCP-friendly by design)
  kind: ToolType,
  capabilities_used: [CapabilityId],     -- for annotation + filtering + skills
  permission: READ | ANALYZE | DRAW | ALERT | (EXECUTE never),
  exposes: { mcp: bool, domains: ["quant","chart",…] },
  annotate_provenance: bool              -- stamp results via the resolver
}
```

Registration stays **static and compile-time** (dynamic plugin loading is an
anti-pattern here — §35); what becomes dynamic is *exposure* (17.4). The dispatch stays
a match; the parity test extends to descriptors.

### 17.3 Thin-wrapper discipline

The existing tools are already thin (fetch → engine call → render). The rule is made
explicit and enforced by review + a lint-level convention: a tool body may fetch,
authorize, bound, call one engine (or a short pipeline like `analyze_timeframe`'s
state build), render, and annotate. **No new math in tools, no math re-implementation
anywhere above analytics-core.** When an analysis needs a pipeline, the pipeline moves
into analytics-core as a pure function (the `MarketState` precedent, state.rs) and the
tool stays a wrapper.

### 17.4 Dynamic exposure (selection, not registration)

- Per request, the offered tool set = base set ∪ skill-recommended set, filtered by
  (a) permission grant, (b) resolver availability for the request's symbol/TFs,
  (c) generation-mode restrictions (already exist: submit_thesis-only turns).
- Unavailable tools are either **hidden** (noise control) or **present but marked
  unavailable with explanation** — a per-skill choice, because sometimes the model must
  be able to *say* "I would need footprint data" instead of silently omitting it.
  Default: present-marked for the current small set; hide when the set grows past
  ~30 specs.
- This is the answer to P6 without any plugin machinery.

### 17.5 One implementation, many callers

Each tool's implementation is a free async function taking `(&ToolContext, Value)` —
already true. Internal calls (agent loop), REST (where a matching route exists), and
MCP (§20) are **three bindings over the same function**. New tool work happens once;
bindings are generated/trivial.

---

## 18. Tool Skill Architecture

A **tool skill** is usage doctrine for one tool or one coherent tool family — the HOW
to call it well, WHEN it applies, what it costs, how to interpret outputs, and its
known limits. It is distinct from a trading skill (§19): tool skills never express
market opinion; trading skills never restate tool mechanics.

### 18.1 Format (extends the existing skill document — schema v2)

```yaml
name: footprint-analysis            # stable slug; id = name-v<major> (existing rule)
version: 1.0.0
category: tool-skill                # NEW: tool-skill | trading-skill (existing = trading)
applies_to:                         # NEW
  tools: [get_footprint, detect_imbalance, detect_absorption]
knowledge: |                        # what the tool family measures, in words
rules:                              # usage doctrine (prose, rendered into prompts)
  - "Call get_footprint only when capabilities report trades available…"
  - "…"
capability_requirements:            # NEW — the data-awareness contract
  required:  [{capability: footprint}]            # must be 'available'
  preferred: [{capability: orderbook_snapshots}]  # upgrades interpretation
  optional:  []
  fallback:  []                                   # e.g. derived variants allowed
conditions: { timeframes: [1m, 5m, 15m], risk: { max_risk_pct: 1.0 } }
examples: []
invalidation: []                    # n/a for tool skills, kept for schema unity
```

Storage: same two tiers as today (shipped YAML under `skills/tool/*.yaml`; user copies
in the existing `skills` table — category column derives from the document, no
migration needed for v1→v2 since unknown fields are tolerated on read **NOT VERIFIED** —
if the loader is strict, a tiny `category` default covers it).

### 18.2 Which tools get skills

**Not one skill per tool.** One skill per *coherent family* (the unit of doctrine):
footprint/imbalance/absorption; liquidity/structure; volume-profile/value-area;
delta/CVD/vpin/size-classes; chart-drawing (all three mutations + read); research
(backtest tools); memory. Target: **7–10 tool skills**, each covering 1–5 tools.

### 18.3 Examples of tool-skill content

- *footprint-analysis*: call only when resolver says trades available; window guidance
  (footprint reads are heavy — cap bars); how to read `available:false` (answer the
  user with the gap, don't substitute); stacked-imbalance meaning; never infer
  absorption from candle-derived footprints.
- *chart-drawing*: prefer updating over duplicating; anchor to observed swing prices
  from tool outputs, never invented prices; one idea = one drawing set; fib usage
  (swing selection rules); always state the drawn ids in the answer; the AI layer is
  user-toggleable — never assume visibility.
- *research*: always attach a base rate before quoting a strategy; report assumptions
  (fees/slippage) verbatim; a sweep that shows a lucky spike is a *warning*, not a
  result (parameter_sweep's middle-stability reading).

### 18.4 Lifecycle

Create/edit via REST (existing `/skills` routes) + shipped YAML; the AI may **propose**
new tool skills through a create-skill tool (§24.4) that validates schema + stores as
unpublished until user-approved — same gate philosophy as bot-drafts
(indicator_workspace_routes approve flow). Retrieval: tool skills attach to the
prompt **through** trading skills and tool exposure (when a family is exposed, its
skill is eligible), not by free-text search alone.

### 18.5 Validation & interpretation

Schema-validated at write (deny unknown fields — strategy-dsl precedent); capability
ids checked against the registry at write time (unknown id → 422 with the known list —
the `/strategies/schema` pattern). Rendering into prompts keeps today's header +
numbered-rules format. Wrong interpretation is prevented by (a) rules referencing
*fields* of tool outputs (the sweep-skill precedent, skills/liquidity/sweep.yaml:26),
(b) the UNKNOWN-first rule already encoded, (c) resolver explanations quoted back.

---

## 19. Trading Skill Architecture

A **trading skill** is a methodology: WHEN/WHY to look, WHAT must be true, how to
validate, how to invalidate, and what to do when data cannot answer. It composes
capabilities; it does not contain formulas.

### 19.1 Structure (schema v2, category: trading-skill)

```yaml
name: mean-reversion-value-area
version: 1.0.0
category: trading-skill
knowledge: |                        # the methodology, in words (existing field)
rules: |                            # validation doctrine (existing, now typed refs allowed)
capability_requirements:            # NEW
  required:  [{capability: market_structure}, {capability: volume_profile}]
  preferred: [{capability: footprint}, {capability: cvd_divergence}]
  optional:  [{capability: iceberg_detection}]
  fallback:  [{needs: footprint, accept: none},      # explicit "no substitute"
              {needs: volume_profile, accept: volume_profile.candle_derived}]
artifact_kind: thesis               # NEW: thesis | strategy-dsl | pine-script | monitor
conditions: { timeframes: [5m, 15m, 1h], risk: { max_risk_pct: 1.0 } }
preferred_markets: [BTCUSDT]
examples: []  invalidation: []
```

### 19.2 Data-awareness behavior

At selection time (and render time), the resolver checks the requirements against
(provider, symbol, TFs):

- **required unavailable** → skill is refused *by name*: "mean-reversion-value-area
  requires market_structure — available — and volume_profile — **unavailable on this
  venue**". The agent either picks another skill or answers INSUFFICIENT DATA with the
  named gap. **A skill that cannot run must not silently run degraded.**
- **preferred unavailable** → skill runs; the prompt render lists the gap and the rules
  say what that means for confidence (e.g. "no footprint → conviction cap 0.6").
- **fallback declared** → the substitute is used and *labeled in the thesis evidence*
  ("value area from candle-derived profile"); no fallback declared → no substitution.
- All of this is mechanical (registry), not prompt-engineering.

### 19.3 Examples of composition

- *sweep-reversal* (exists as liquidity/sweep.yaml) gains:
  `required: [liquidity_events, market_structure]`, `preferred: [footprint, delta_by_size]`,
  fallback none for footprint — honest: without trades it is a pure price-action sweep
  read, and the skill's own UNKNOWN rule (sweep.yaml:26) already says how to score it.
- *breakout-continuation*: required structure + volume events; preferred vpin
  (toxicity filter); fallback: vpin → none; risk defaults tighter without it.
- *fibonacci confluence*: required structure + swing detection; uses **chart tool
  skills** for drawing (fib anchors from detected swings — no invented prices);
  preferred volume_profile (confluence with POC/VAH/VAL).

### 19.4 Selection, versioning, creation

Selection keeps the deterministic scoring (skills.rs:392-443) with two added steps:
capability filtering before scoring, and `artifact_kind` used by the caller (ask-mode
prefers thesis skills; studio prefers pine-script skills). Versioning stays
append-only. Users create via REST; the AI proposes via the gated create-skill flow
(§24.4). **No skill may request EXECUTE permission** — skills can produce theses,
documents, monitors; execution stays on the existing bot path.

---

## 20. MCP Architecture

### 20.1 Shape: one gateway, modular domains

**NOT "one MCP server per domain" at this stage.** The verified system has ~25 tools
across four natural domains; four MCP servers would quadruple auth/transport/
deployment surface for zero compute benefit (MCP is an interface layer — §36 I-10).
The recommendation:

```text
crates/mcp-gateway (NEW, additive; depends on ai-agent tools + capabilities + market-data)
  └─ one MCP server process (stdio first; streamable-HTTP later if remote clients appear)
       ├─ domain "market"   → candles, symbols, tickers, capability answers
       ├─ domain "quant"    → the 14 analysis tools (+ research when wired)
       ├─ domain "chart"    → drawing read/write (scoped permission)
       └─ domain "skills"   → skill listing/reading (+ propose-skill later)
  Each domain is a filter over the SAME ToolRegistry + capability resolver;
  domains exist as namespaces + exposure policy, not as processes.
```

Split into separate servers later **only** when one of: a second process must expose a
subset (e.g. a public read-only server), per-domain auth policies diverge, or load
forces it. The domain boundary is already drawn (ToolDescriptor.exposes.domains), so
the split is configuration, not refactor.

### 20.2 Domain → tool mapping (from the verified registry)

| Domain | Tools (existing) | New (target) |
|---|---|---|
| market | `get_candles` | `get_orderbook` (P11), `get_capabilities`, `get_data_coverage` (wraps /footprint/coverage + tape spans) |
| quant | `analyze_timeframe`, `analyze_multi_timeframe`, `get_volume_profile`, `get_footprint`, `get_delta*`, `get_cvd`, `get_vwap`, `get_vpin`, `detect_*` (7) | `backtest_*` once wired; `get_market_state` (23) |
| chart | `get_user_drawings`, `create/update/delete_drawing` | `inspect_region`, `measure_range` (25) |
| skills | — | `list_skills`, `get_skill`, later `propose_skill` |

Explicitly **NOT exposed**: `submit_thesis`, `draft_strategy`, `submit_script`
(orchestration internals), memory writes (initially), anything execution — forever.

### 20.3 What MCP clients can do

Call exposed tools with the same inputs/outputs as the agent sees (including
`available:false` + explanation), list capabilities with availability for their
authorized symbols, read skills. Auth = the same JWT (or scoped API tokens minted from
it) with **permission scopes per token** (§28); a chart-scope token cannot call quant
tools, and no token reaches execution.

### 20.4 Mapping to internal engines

Trivial by construction: MCP tool call → descriptor lookup → the same free function
the agent loop calls → same resolver annotation → JSON result. The MCP crate contains
zero math and zero market-data access of its own. Generation loops (draft/repair) are
**not** first-class MCP tools initially — they are orchestrations; a later
`generate_indicator` MCP *prompt* or high-level tool can wrap `pine_codegen` once its
SSE lifecycle has a non-streaming completion mode.

### 20.5 Recommended deployment

In-process library + thin binary that can also be linked into api-gateway behind an env
flag (`MCP_ENABLED`) — mirroring how the sandbox guest is embedded (build.rs pattern).
Phase 6 in the roadmap; **not before the capability registry and permission tiers
exist** (MCP without them would freeze today's ad-hoc answers into a protocol).

---

## 21. AI Agent Architecture

Keep the orchestrator — it is the strongest verified component. Evolve four things:

1. **Capability-aware context.** System prompt gains the resolver's compact capability
   summary for the request's (symbol, ladder). The model learns "no trades on this
   venue" *before* planning, not after a refused call. (Builds on the ladder pre-read
   pattern, agent.rs:559-578.)
2. **Dynamic tool sets** (§17.4) selected per request from skill requirements +
   permissions + availability; turn caps unchanged. Token savings fund the capability
   summary.
3. **Plan-execute-verify for mutations** (§25): drawing writes are batched at the end
   of analysis; a verify turn re-reads via `get_user_drawings` and the prompt rules
   require the model to confirm placement against observed prices or delete. The
   forced-answer phase already exists as the insertion point (agent.rs:696-778).
4. **Robustness, in order**: bounded provider retries with jitter (P13) *before* any
   monitoring loop depends on the agent; token streaming stays off (docs/36);
   context compaction stays off (turn caps bound it); **multi-agent decomposition stays
   out** until the single-agent loop is measurably the bottleneck (§35).

Model independence: unchanged (LlmClient + ProviderId). Prompt strategy: skills render
as today (header + rules), tool skills attach with their tools, capability summary is a
table not prose. Safety: grounding/finalize unchanged and extended — thesis evidence
items now may carry `{capability, availability, source}` stamped by tools, and
finalize rejects evidence stamped `derived` when the skill required the true kind.

---

## 22. Provider Architecture

### 22.1 The adapter contract (evolves, does not replace, WireCodec + Venue)

```text
ProviderAdapter = {
  identity: { id: "binance"|"bybit"|"deriv", name },
  live:     Option<WireCodec-impl>,        -- none ⇒ REST-only provider (valid!)
  rest:     Option<Venue-impl>,            -- klines listing/paging semantics
  profile:  ProviderDataProfile,           -- §16.1: the capability truth of this venue
  symbols:  SymbolModel,                   -- exchangeInfo-shaped | custom listing |
                                           -- synthetic (Deriv: fixed instrument set)
  extras:   Vec<ProviderExtra>             -- venue-unique data preserved verbatim
                                           -- (e.g. Binance aggTrade ids), typed,
                                           -- opt-in, never required by engines
}
```

Normalization rules (binding): timestamps → ns at the codec (existing codec.rs:78-86
rule); `Trade` requires an aggressor side or the provider must declare
`granularity: tick` and emit `Tick` instead — **no fabricated sides**; kline split
`real | attributed | absent` stays on the profile (the venue.rs precedent, lifted);
refusals stay first-class (interval refusal, no-aggTrades answer).

### 22.2 Deriv, worked end-to-end (projected)

```text
Deriv profile (synthetic indices):
  candles:        REST + WS candles — available (volume semantics = tick count, DECLARED)
  candles_split:  absent (no taker-buy volume)
  ticks:          available (the native stream — price + epoch, no side, no size)
  trades:         absent as aggressor-side trades; ticks are NOT trades
  orderbook/OI/funding/liquidations/options: absent

Resolver consequences (mechanical, per §13.2):
  market_structure, liquidity events, sessions, forecast, indicators  → available (true)
  vwap  → derived (tick-count VWAP) — own capability id, labeled
  volume_profile → derived from candles; profile preferred-source absent → quality note
  delta/cvd/vpin/size-classes/bar-delta → unavailable (no side/size) — refused with
      explanation, and skills requiring them are refused BY NAME (§19.2)
  footprint/imbalance/absorption → unavailable; footprint.tick_derived MAY be added
      later as a distinct capability (price-level tick counts) — never aliased to footprint
```

The provider plugs in with: one codec (ticks+candles), one venue (candle paging),
one profile, one symbol model. **Zero changes in analytics-core, tools, skills** —
the registry produces the honest Deriv story automatically. That is the acceptance
test of this architecture.

### 22.3 Live vs historical vs stored (unchanged policy, made visible)

Live stays in RAM; historical stays venue-REST-on-demand; deliberate persistence stays
xtask-only. The registry surfaces the *effective window* per capability ("footprint:
live window + deliberately backfilled ranges") using tape/history spans — the P9
constraint becomes a first-class answer instead of a surprise.

### 22.4 Versioning

Provider profiles are code (per venue) + serde-checked; the registry version is
reported in `/capabilities`. Engine contracts don't version (pure functions pinned by
golden tests); tool schemas version by additive fields only (MCP clients depend on
them); skill documents carry `version` + append-only storage (existing).

---

## 23. Market Intelligence State

### 23.1 What it is (evolved, not replaced)

`MarketState` (state.rs:84-161) remains the per-(symbol, TF, window) aggregate. It
gains a **provenance block** and nothing else is restructured:

```text
MarketStateProvenance = {
  candles: { source, window_bars },
  split:   real | attributed | absent,
  trades:  { available, window: live_since | (earliest,latest), count } | absent,
  book:    { available, snapshots } | absent,
  sections: { footprint: availability, absorption: availability (→ P4 fix),
              profile: "trades"|"candles", imbalances: availability, … },
  explanations: [string]            -- resolver-generated, for the render
}
```

`render_state` (tools.rs:2085-2135) then renders `absorption: {available:false, why}`
instead of a silent `[]`, and the ad-hoc `data_note` patch (tools.rs:2131-2133) is
replaced by the structured block (same bytes budget — the render already truncates).

### 23.2 Producers / consumers / invalidation

- Producer: `build_market_state` (+ resolver call) — the only producer; tools,
  event engine, and (later) monitors consume it; the chart does not (chart consumes
  raw + scene geometry — keep that split).
- Caching: a small per-(symbol, TF, window-end-bar) memo in `WindowMarketData`
  (market_data.rs:95-156), invalidated on bar close — the natural key already exists
  (`latest_candle_time`). **Not** a cache inside analytics-core (purity), **not** a
  persisted cache (6GB rule). This is the one caching opportunity that matters: the
  ladder pre-read + user prompts repeatedly rebuild identical windows (P-cost noted
  in §7).
- The `MarketIntelligenceState` is **per-request scope, not a global mega-object**;
  the multi-timeframe ladder (multi_timeframe.rs) already composes per-TF states +
  a deterministic confluence score — that composition is kept and gains the same
  provenance rollup.

---

## 24. AI Studio Integration

### 24.1 Where it fits

The generation subsystem (§6) becomes the **artifact factory** of the architecture:
indicators (pine-lite), strategies (DSL), and — new — **skills** are all
model-authored, machine-validated, versioned, user-attributed artifacts. The studio's
existing guarantees map one-to-one onto the skill layer's needs: validation gate
(vet/parse) → versioned storage (revisions/append-only) → preview/attachment →
approval gates for anything that acts (bot-drafts).

### 24.2 Interaction with quant engines and tools

- Generated **indicators** keep today's boundary: `ta.*` → analytics-core; candle data
  in, drawings out; no I/O in guest. The P8 extension (host-provided derived series —
  delta/CVD of the pane's candles, computed by analytics-core and passed in) is the
  single planned widening, and it keeps the guest pure.
- Generated **strategies** keep the DSL pipeline; `backtest_strategy` (once wired, P1)
  becomes callable *inside* the generation loop so "generate → backtest → report" is
  one studio turn instead of two requests.
- Generated artifacts are **not tools** and are not dynamically registered as tools
  (§35). The agent learns *about* a user's pinned indicator via the chart context /
  workspace references; it does not gain new tool surface per artifact. If artifact-
  invocation ever becomes a requirement, it arrives as ONE generic tool
  (`run_indicator(id, …)` / `run_strategy_backtest(id, …)`) — never per-artifact tools.

### 24.3 What the studio adds to the architecture

- **Skill authoring UI**: a third workspace kind ("skill workspace") — chat to draft a
  trading-skill YAML, validated against schema + capability ids, previewed by rendering
  the exact prompt block + the resolver's availability report on the user's chart
  symbol. This is the missing piece of docs/10's vision, built entirely from existing
  machinery.
- **Prompt-block preview**: reuse the indicator-preview pattern to show "what the model
  will see" — the strongest correctness tool skills can have.

### 24.4 The gated create-skill tool

`propose_skill` (agent tool, permission ALERT-tier): validates schema, checks capability
ids against the registry, stores with `created_by:"ai"` + unpublished state; user
approves in the skills UI before it enters retrieval. Mirrors bot-draft approve exactly.
Direct AI-created-and-live skills are an anti-pattern (§35).

### 24.5 What NOT to add yet

No user-defined quant engines (sandbox covers custom logic via pine-lite/DSL), no
artifact marketplace/sharing (no demand evidence, heavy abuse surface), no
artifact-as-tool dynamic registration, no third authoring language.

---

## 25. Chart Interaction Architecture

### 25.1 Current state (verified)

Read: `get_user_drawings` (≤24, user's own, read-only — user_drawings.rs:31-59);
viewport/drawings/screenshots as request attachments (chart_context.rs:17-34: "the
viewport is a hint, never an instruction"). Write: `create/update/delete_drawing`
through `DbDrawingWriter` with kind/anchor validation + provenance (market_data.rs:235-406);
shell renders AI drawings behind an off-by-default layer (app.js:4430-4448). No
verification step; no measurement/inspection tools; system prompt carries anti-clutter
rules (agent.rs:1247-1262).

### 25.2 Target: plan-execute-verify, budgeted

```text
PLAN     the model declares intended drawings (kind + anchor prices from TOOL OUTPUTS)
         inside its reasoning; prices must exist in the grounding PriceRange — the
         existing grounding machinery (thesis.rs:393-512) is reused for drawings:
         create_drawing with a price outside observed ranges is rejected with a
         correction turn, exactly like ungrounded thesis levels.
EXECUTE  the writes (existing tools, unchanged server-side validation)
VERIFY   one re-read (get_user_drawings filtered to created_by:"ai", this session) —
         the model confirms ids/anchors match the plan or repairs (update/delete);
         then the answer phase. Verify turn is free of new data (read-only).
```

- **Budgets**: max drawing writes per ask (default 6 — one idea ≈ 2–4 objects), enforced
  in the drawing writer wrapper; counted in the trace.
- **Provenance/visibility**: unchanged (created_by:"ai", confidence/reason fields,
  off-by-default layer, user deletes via existing UI). AI drawings are a *suggestion
  layer*, never an overlay the user can't remove.
- **New read tools**: `measure_range` (price/time between two observed points — thin
  arithmetic over anchors, clearly a READ), `inspect_region` (drawings + zones +
  events overlapping a price/time band — composes regions.rs + events.rs + drawings;
  ANALYZE tier). `get_visible_range` is NOT a tool — the viewport stays a request
  attachment by design (chart_context.rs:17-34), and a tool would tempt the model to
  treat it as evidence.
- **Skills integration**: the *chart-drawing* tool skill (§18.3) carries the doctrine
  (prefer update over duplicate, anchor from detected swings, fib usage rules); trading
  skills declare whether they draw at all (`artifact_kind` + rules).

### 25.3 The fib example (task §41c), end to end

User: "draw the current fib retracement on BTC 1h." → capability summary (structure
available) → `detect_market_structure` (1h) → swings from Rust, not the model →
skill *fibonacci-confluence* rules pick the operative swing pair (last BOS leg) →
`get_volume_profile` optional confluence → PLAN: fib from swing low → swing high
(prices inside PriceRange ✓) → `create_drawing{kind:"fib", anchors:[…]}` → VERIFY
re-read → answer cites the drawing id + levels + the confluence note + what would
invalidate. On Deriv: identical except volume_profile is `derived` → the skill's
fallback declaration decides whether the confluence note appears labeled or the
confluence check is marked UNKNOWN.

---

## 26. Research / Backtesting Architecture

### 26.1 Verified assets

`backtester` (deterministic replay over the finest declared TF; report in R with fee/
slippage assumptions, 2026-09-16/17 golden comparisons in reports/); `strategy-runtime`
Simulator; `parameter_sweep` (threshold() grid, textual substitution re-validated —
parameter_sweep.rs:1-16); `POST /strategies/{id}/backtest` (in-process, stored runs,
strategy_routes.rs:915-922); reference strategy YAMLs (`strategies/`, incl. the
base-rated one); BacktestRunner **trait** (tools.rs:116-137) — unwired (P1).

### 26.2 The pipeline the architecture assumes

```text
skill (methodology, risk defaults)
  → strategy document (DSL; studio or agent generated; threshold() marks tunables)
  → validate (type gate)
  → backtest (historical window via WindowService REST backfill)
  → sweep (threshold grid; middle-stability reading)
  → paper bot (sandboxed, live feed)
  → live gate (paper record ≥20 trades/≥48h/>-10R + opt-in + risk limits — gate.rs:53-61)
  → live (vault-sealed keys, protective orders, reconcile)
```

Every arrow exists **except** the agent's programmatic access to steps 3–4 (the
BacktestRunner) — wiring it reuses the route's data loading verbatim.

### 26.3 The BacktestRunner wiring (P1 fix, specified)

- Implement in api-gateway (`market_data.rs` neighbor, e.g. `research.rs` — NEW FILE):
  `BacktestRunner::run(document, symbol, from, to)` → parse+validate → candles via
  `WindowService` (REST backfill path — same as the route) → `backtester::replay` →
  report; `similar_setups(skill_id)` → map skill → its reference strategy (shipped
  YAML pairing) → same replay → `HistoricalBaseline`.
- Constraints: execution time bounds (clamp window/TF; the replay is in-process and
  synchronous — cap bars, reuse the route's clamps); results stamped with
  `{window, assumptions}`; thesis `historical_*` fields stop being permanently None.
- Explicitly out of scope now: order-flow backtesting (impossible without tick
  persistence — P9; the registry reports this as a capability window limit, and skills
  that require order flow must say so in their backtest expectations — rules text).

### 26.4 Later, and only later

Walk-forward splits; Monte-Carlo trade-order resampling (the seeded bootstrap pattern
from forecast.rs is the template — determinism preserved); divergence-vs-live alert
(writer for the existing no-op rule, alerts.rs:1091-1095). All read-only over stored
runs.

---

## 27. Monitoring Architecture

### 27.1 What exists (verified seeds)

Derived-event lane (`event_engine` — stateless per-bar diff, bounded buffer, `/events`
+ WS); alert delivery worker (`indicator_alerts` — preferences, killzone gating, dedup,
queue); platform alerts (`observability/alerts.rs` — fire-on-transition, webhook);
bot decision events (`bots.rs` BotEvents). **No AI involvement in any of it** — correct.

### 27.2 Target: skill-based monitors, AI on the trigger only

```text
Monitor = { skill_id, symbol(s), timeframes, delivery prefs }   -- user-created, DB row

loop (per symbol, reuses event_engine's watcher-per-symbol):
  on bar close → detect_events (existing) → match against the skill's declared
  trigger events (skill gains optional `monitors:` block naming event kinds + conditions)
  → on match: bounded AI invocation (same Agent::ask path, monitor-scoped prompt,
    the skill pinned, chart context absent) → thesis or NO-TRADE →
    alert payload = event + thesis summary + provenance
  → dedup window per (monitor, event kind) — the indicator_alerts dedup pattern
```

- **Cost control is architectural**: deterministic filter first (events are free —
  candle-only), AI second (only on trigger); per-monitor daily invocation cap; monitors
  count against the user's existing rate limiter bucket; provider retries (P13) land
  before this, because a monitor call is no longer inside a request lifecycle.
- **Capability gating**: a monitor whose skill requires unavailable data for its symbol
  refuses at creation with the resolver's explanation (same §19.2 mechanism).
- **What stays out**: continuous AI loops, AI-per-bar, AI watching order flow tick by
  tick. The event engine's candle-only diff model is the deliberate cost boundary.

---

## 28. Permission / Execution Architecture

### 28.1 Current state (verified)

Auth = JWT HS256 7-day, claims sub/email/exp (auth.rs); authorization = ownership
scoping only, **no roles/scopes** (P10/P18); agent = full user grant (drawings+memory
attached server-side, agent_routes.rs:205-229); execution side is the strong part:
live gate triple (paper record + venue opt-in + risk limits, gate.rs:4-9), vault-sealed
keys with canWithdraw refusal, kill switch, idempotency, reconcile.

### 28.2 Target permission ladder

```text
READ      candles/books/capabilities/skills read        (default for any session)
ANALYZE   quant tools, market state, research           (default for authenticated)
DRAW      chart mutations + propose_skill               (explicit grant; default ON
          for the first-party app session, opt-in per API token)
ALERT     monitors + alert preferences                  (explicit grant)
EXECUTE   bot create/pause/kill + broker ops            (existing routes ONLY;
          never exposed to the agent or MCP; step-up: re-auth when P18 lands)
```

Mechanics: a `scope` claim on the JWT (P18) + per-token scopes for MCP/API tokens;
`ToolContext` gains the grant set and the registry filters exposure; server-side
enforcement stays at the binding layer (route handler / MCP dispatch) — tools also
self-check for defense in depth. Ownership scoping (404-on-foreign) is unchanged and
orthogonal.

### 28.3 Execution stays outside the agent — permanently

The dependency rule (ai-agent may not name trading-engine types) is the enforcement;
the design adds a *policy* statement on top: no tool descriptor may carry an EXECUTE
permission; execution UX is bots + broker routes + the live gate. The agent's maximum
proximity to execution remains what it is today: generating strategy documents that a
human promotes through the gate. Live execution authorization remains an explicit human
act (opt-in + broker connect + bot create) — the architecture adds no path around it.

---

## 29. Keep / Extend / Refactor / Create Matrix

| Component | Path | Verdict | Why (evidence) |
|---|---|---|---|
| analytics-core (all engines) | crates/analytics-core | **Keep** | pure, deterministic, wasm-clean, honest fallbacks, property tests; the center of the architecture |
| MarketState | state.rs | **Extend** | add provenance block (§23); no restructure |
| WireCodec + generic Collector | exchanges/codec.rs, collector.rs | **Keep** | correct seam; venue-specifics isolated; reconnect/gap/book done once |
| Venue trait + Binance/Bybit venues | exchanges/venue.rs | **Keep + Extend** | add `profile()` (ProviderDataProfile) to the trait |
| MarketEventBus / HistoryRegistry / LiveRegistry / CandleBuilder / WindowService | bus/history/tape/candle_builder/window.rs | **Keep** | RAM discipline + unified read path is right; add span reporting for the registry |
| BackfillClient (venue-agnostic) | backfill.rs | **Keep** | already `Arc<dyn Venue>`; exchangeInfo parser is the known seam (P5) |
| strategy-dsl (+ ValidatedStrategy gate) | crates/strategy-dsl | **Keep** | type-level gate, closed grammar, deny-unknown — exactly right |
| strategy-runtime + backtester | crates/strategy-runtime, backtester | **Keep** | no-lookahead by construction, golden-tested |
| sandbox + sandbox-guest | crates/sandbox* | **Keep** | 4-import allowlist, fuel/mem/wall ceilings, embedded guest |
| pine-lite | crates/pine-lite | **Keep + Extend** | P8 host-provided derived series, later |
| ai-agent orchestrator + grounding + thesis | agent.rs, thesis.rs | **Keep + Extend** | plan-execute-verify insertion, capability summary, dynamic tool sets |
| ToolRegistry + ToolContext | tools.rs | **Extend** | descriptors (capabilities, permission, exposure); keep dispatch + parity test |
| Skills schema + retrieval | skills.rs | **Extend** | schema v2 (category + capability_requirements + artifact_kind); capability-filtered selection |
| SkillLibrary (shipped) | skills/*.yaml | **Extend** | add tool skills; annotate the two existing skills |
| DB skills tier | db/src/skills.rs, skills_routes.rs | **Keep** | works; needs agent visibility (P2 fix in resolve_agent) |
| LlmClient + providers | providers/, llm_client.rs | **Keep + Extend** | add bounded retries (P13) when monitoring lands |
| capabilities.rs deployment report | api-gateway/capabilities.rs | **Extend** | gain the analysis-view section from the resolver (§16.2) |
| event_engine + indicator_alerts | api-gateway/event_engine.rs, indicator_alerts.rs | **Keep + Extend** | become the monitor substrate (§27) |
| trading-engine (risk/gate/execution/secrets) | crates/trading-engine | **Keep** | strong; live-balance read + shutdown wiring + AAD granularity are route-level fixes (P19) |
| observability | crates/observability | **Keep** | one global registry is right; divergence-alert writer later |
| api-gateway structure (AppState/router) | lib.rs, main.rs | **Keep + Extend** | wire: resolver, BacktestRunner impl, merged skill library; fix P16/P17 limiter holes |
| WindowMarketData + DbDrawingWriter | market_data.rs | **Keep** | the capability-injection exemplars |
| frontend shell + chart-engine | frontend/ | **Keep** | engine-owns-math discipline is right; JS residues noted (P15) |
| builder.js visual builder | frontend/app/builder.js | **Keep** | schema-driven; degrade-to-text is honest |
| db schema (11 migrations) | crates/db/migrations | **Keep + Extend** | new tables only: monitors, (api tokens later) |
| xtask collect/backfill | tools/xtask | **Keep** | the deliberate-persistence valve |
| MCP gateway | crates/mcp-gateway | **Create** (phase 6) | thin adapter over ToolRegistry + resolver (§20) |
| capabilities crate | crates/capabilities | **Create** (phase 0) | vocabulary + descriptors + resolver (§16) |
| Provider registry/catalog | market-data + gateway | **Create** | replaces KNOWN_VENUES const (P20); assembles profiles |
| Monitors | db table + gateway worker | **Create** (phase 7) | §27 |
| Skill studio workspace | frontend + workspace routes | **Create** (phase 5) | §24.3 |
| API tokens w/ scopes | auth.rs + table | **Create** (with MCP) | §28.2 |

**Refactor** verdicts are deliberately rare: the only true refactors are (a) moving the
per-tool availability guards to consult the resolver (same behavior, one source), and
(b) the tool result shape gaining a structured provenance field (additive). Nothing
is rewritten.

---

## 30. Proposed Repository Structure

Diff against §3 (only additions/moves — everything not listed is untouched):

```text
crates/
  capabilities/            # NEW leaf crate (deps: analytics-core only)
    src/lib.rs             #   DataKind, Availability, Quality, DataRequirement
    src/descriptor.rs      #   CapabilityDescriptor + the built-in catalog (§16.1)
    src/profile.rs         #   ProviderDataProfile, SymbolClass
    src/resolver.rs        #   pure resolution + explanation strings
  mcp-gateway/             # NEW (phase 6; deps: ai-agent, capabilities, market-data)
    src/{lib,server,auth,domains/{market,quant,chart,skills}}.rs
  market-data/src/exchanges/
    profile.rs             # NEW: venue/codec → ProviderDataProfile contributions
    deriv_codec.rs         # NEW (phase 8)
  ai-agent/src/
    capability_view.rs     # NEW: resolver → compact prompt summary + result stamps
    tools/{mod,market,quant,chart,memory,research,generation}.rs  # split of tools.rs
                           #   (mechanical move, same functions — keeps diffs reviewable)
  api-gateway/src/
    research.rs            # NEW: BacktestRunner impl (P1)
    monitors.rs            # NEW (phase 7): monitor worker over event_engine
    capability_routes.rs   # NEW: extend /capabilities analysis view
  db/migrations/
    0012_monitors.sql      # NEW (phase 7)
    0013_api_tokens.sql    # NEW (with MCP)
skills/
  tool/                    # NEW: footprint.yaml, liquidity-structure.yaml,
                           #      value-area.yaml, delta-suite.yaml, chart-drawing.yaml,
                           #      research.yaml, memory.yaml
  trading/                 # moved: footprint/absorption.yaml, liquidity/sweep.yaml
                           # (gain capability_requirements; ids unchanged = same slug+v1)
frontend/app/              # unchanged; later: capability badges read /capabilities,
                           # skill workspace UI
docs/38-CAPABILITY-REGISTRY.md …  # new numbered docs per phase (repo convention)
```

Why a **crate** for capabilities and not a module: the resolver must be consumable by
ai-agent (which may not depend on market-data) and by api-gateway and (later)
mcp-gateway without dragging provider code along — a leaf crate is the only placement
consistent with Cargo.toml:18-31. Why tools.rs splits: 3,434 lines already strain
review; the descriptor additions make it worse; the split is mechanical (same module
paths re-exported, dispatch unchanged) and can ride along with the descriptor change.

---

## 31. Proposed Interfaces / Contracts

Sketches (signatures, not implementations). **Forbidden callers** are stated per
interface — that is where the architecture is enforced.

```rust
// ── crates/capabilities/src/resolver.rs ─────────────────────────────
pub trait CapabilityResolver: Send + Sync {
    fn resolve(&self, cap: CapabilityId, scope: &DataScope) -> Resolution;
    fn summary(&self, scope: &DataScope) -> CapabilitySummary;   // prompt-sized
}
pub struct DataScope { pub provider: ProviderId, pub symbol_class: SymbolClass,
                       pub symbol: String, pub timeframes: Vec<Timeframe> }
pub struct Resolution { pub availability: Availability, pub quality: Option<Quality>,
                        pub missing: Vec<DataKind>, pub explanation: String }
// Callers: ToolRegistry dispatch, agent prompt builder, skill selection, /capabilities,
// mcp-gateway. Forbidden: analytics-core (never knows providers), trading-engine.

// ── crates/market-data/src/exchanges/profile.rs ─────────────────────
pub trait ProviderProfile { fn profile(&self) -> ProviderDataProfile; }
impl ProviderProfile for BinanceVenue { … }   // and codecs contribute live-side facts
// Callers: api-gateway boot assembly. Forbidden: anything below market-data.

// ── crates/ai-agent/src/tools.rs (evolved registry) ─────────────────
pub struct ToolDescriptor { pub spec: ToolSpec, pub kind: ToolType,
    pub capabilities: Vec<CapabilityId>, pub permission: Permission,
    pub exposes: Exposure }                          // Exposure{ mcp: bool, domains }
pub enum Permission { Read, Analyze, Draw, Alert }   // no Execute — by construction
impl ToolRegistry {
    pub fn market_analysis() -> Self;                // kept; now builds descriptors too
    pub fn specs_for(&self, grants: &Grants, scope: &DataScope,
                     resolver: &dyn CapabilityResolver,
                     mode: ExposureMode) -> Vec<ToolSpec>;          // dynamic exposure
    pub async fn call(&self, name: &str, arguments: Value,
                      context: &ToolContext) -> Result<Value, ToolError>;  // unchanged
}
// Callers: agent loop, mcp-gateway dispatch. Forbidden: routes calling tool fns
// directly with elevated context (bindings must pass real grants).

// ── crates/ai-agent/src/skills.rs (schema v2) ───────────────────────
pub struct Skill { /* existing fields, plus: */
    pub category: SkillCategory,                     // Trading | Tool
    pub capability_requirements: CapabilityRequirements,  // required/preferred/optional/fallback
    pub artifact_kind: Option<ArtifactKind>,         // Thesis|StrategyDsl|PineScript|Monitor
    pub monitors: Option<Vec<MonitorTrigger>> }
// Loaders validate capability ids against the catalog at load/write time.

// ── crates/api-gateway/src/research.rs ──────────────────────────────
pub struct RouteBacktestRunner { windows: WindowService, db: Option<Arc<Database>> }
impl ai_agent::tools::BacktestRunner for RouteBacktestRunner { … }   // P1
// Callers: ToolContext construction in agent_routes/ws. Forbidden: ai-agent internals.

// ── crates/mcp-gateway/src/lib.rs (phase 6) ─────────────────────────
pub struct McpGateway { tools: Arc<ToolRegistry>, resolver: Arc<dyn CapabilityResolver>,
                        auth: Arc<ScopeVerifier> }
// serves JSON-RPC per MCP spec; each method = descriptor lookup + the same call().
// Callers: MCP clients over stdio/HTTP. Forbidden: direct market-data/engine access.

// ── unchanged-by-design contracts (restated so nobody "unifies" them) ──
// analytics-core: pure fns, no traits for engines — functions ARE the contract.
// LlmClient (llm_client.rs:307-318): complete(LlmRequest) — untouched.
// ValidatedStrategy (strategy-dsl/src/lib.rs): parse→validate→type gate — untouched.
// OrderGateway/ExecutionGate (trading-engine): untouched; no new callers permitted.
```

---

## 32. Migration Strategy

Wrap / adapt / extend — never rewrite. Concretely:

1. **The capability layer wraps, it does not replace.** Per-tool `trades.is_empty()`
   guards stay as the enforcement floor; the resolver becomes the *declared* answer
   consulted first. No tool behavior changes until its descriptor lands.
2. **Additive schema evolution.** Skill documents gain optional fields (v2); v1
   documents (the two shipped skills) load unchanged with `category: trading` default.
   Tool results gain an optional `provenance` field; old consumers ignore it.
   `MarketState` gains one block; the render keeps its budget.
3. **Compatibility rules**: existing tool names/schemas freeze (additive-only changes);
   existing routes unchanged; `capabilities.rs` gains a section, loses nothing; skill
   ids (`{slug}-v{major}`) unchanged for the shipped two; the wasm ABI untouched;
   migrations are new numbered files only (0012+, never edits).
4. **Rollback per phase** is "turn the new surface off": resolver absent → tools behave
   exactly as today (guards still there); no MCP → nothing to disable; monitors off =
   worker not spawned. Every phase ships behind its own wiring point in main.rs.
5. **Docs follow code, per phase** (repo convention: docs/38+, one per phase, each with
   its own "what this is / why / what changed" header — the docs/24-37 pattern).

---

## 33. Implementation Roadmap

Phase 0 is the foundation everything else cites. P1-fix (Phase A) is independent and
can run in parallel with Phase 0 — it fixes a broken promise and needs nothing new.

### Phase A — Wire the research lane (P1, P2) — *independent, do immediately*

- **Goal**: `backtest_strategy`/`backtest_similar_setups` work; user skills reach the
  agent. **Why now**: broken promises in the flagship flow; small, isolated.
- **Dependencies**: none new. **Modules/files**: api-gateway (`research.rs` NEW,
  `agent_routes.rs`, `ws.rs` ToolContext construction, `provider_routes.rs`
  resolve_agent skill merge, `lib.rs`). **Result**: thesis `historical_*` populated;
  user's DB skills selectable. **Risks**: replay cost inside a request → clamp
  window/TF/bars, reuse route clamps; skill merge collisions → existing "user copy
  wins" rule (skills_routes.rs:12). **Migration**: none. **DoD**: tools answer real
  reports in an integration test; scripted-client test shows a user skill selected;
  metrics unchanged shape.

### Phase 0 — Capability vocabulary + registry seed (the first implementation target, §37)

- **Goal**: `crates/capabilities` exists with the catalog covering §13.2; Binance +
  Bybit profiles declared in market-data; a resolver assembled at boot; `/capabilities`
  gains the analysis view; **no tool behavior change yet**.
- **Why first**: skills v2, dynamic tools, MCP, Deriv, and the Deriv-vs-Binance
  honesty story all *cite* it; without it each would invent its own answer.
- **Dependencies**: none. **Modules**: NEW crate; market-data (`exchanges/profile.rs`
  NEW); api-gateway (assembly, `capability_routes.rs` NEW or capabilities.rs
  extended). **Result**: `GET /capabilities` answers per-(provider, capability)
  availability + explanation. **Risks**: catalog overreach → cap the first catalog at
  the §13.2 rows; profile drift → one test per provider pins its profile. **Migration**:
  none. **DoD**: resolver unit tests (Binance footprint = available w/ live scope,
  Bybit REST = unavailable w/ explanation, candle-derived profile = derived); route
  test; docs/38.

### Phase 1 — Tool provenance standardization + MarketState provenance (P3 consumption, P4)

- **Goal**: every tool result carries structured provenance; `MarketState` renders
  availability per section; the `data_note` patch is replaced.
- **Dependencies**: Phase 0. **Modules**: ai-agent (tools.rs → split into tools/*,
  `capability_view.rs` NEW, render_state), analytics-core (state.rs provenance block —
  the only analytics-core change in the whole roadmap). **Result**: the model always
  sees available/derived/unavailable per datum; P4 closed. **Risks**: token budget →
  explanations are terse; render test pins size. **Migration**: additive fields.
  **DoD**: no-trades run shows `absorption: {available:false,…}`; Bybit-simulated run
  labels derived delta; docs/39.

### Phase 2 — Skill schema v2 + tool skills + capability-aware selection (P2-completion, §18-19)

- **Goal**: schema v2 (category, capability_requirements, artifact_kind); 7 tool skills
  shipped; the two existing skills annotated; selection = capability filter → existing
  scoring; refused/degraded skills are explainable.
- **Dependencies**: Phases 0-1 (resolver + provenance), Phase A (DB skills visible).
- **Modules**: ai-agent/skills.rs, agent.rs (selection + render), skills/tool/* NEW,
  skills/trading/ moved. **Result**: asking about a Deriv-like symbol refuses
  footprint skills by name. **Risks**: over-filtering (a strict `required` hiding a
  useful skill) → `preferred`/`fallback` guidance + tests per skill. **Migration**:
  v1 docs default category=trading. **DoD**: golden prompt tests; skill write
  validates capability ids (422 on unknown); docs/40.

### Phase 3 — Dynamic tool exposure + agent capability summary (P6)

- **Goal**: per-request tool sets from grants × availability × skill; capability
  summary table in the system prompt.
- **Dependencies**: Phases 0-2. **Modules**: ai-agent (agent.rs, tools registry),
  api-gateway (grants — full grant until Phase 6). **Result**: token usage per ask
  drops; adding tools stops degrading selection quality. **Risks**: hiding a tool the
  model needed → present-marked default while count < 30. **Migration**: none.
  **DoD**: scripted-client tests per exposure mode; prompt-size metric; docs/41.

### Phase 4 — Chart plan-execute-verify + inspection tools (P7) + skill studio UI (§24)

- **Goal**: drawing grounding via PriceRange; verify turn; `inspect_region`,
  `measure_range`; skill workspace (author + preview prompt block).
- **Dependencies**: Phases 1-2 (chart tool skill exists). **Modules**: ai-agent
  (agent.rs verify phase, tools/chart.rs), api-gateway (market_data.rs budgets,
  workspace routes), frontend (skill workspace UI). **Result**: AI drawings are
  planned, grounded, verified, budgeted. **Risks**: extra turn latency → verify is
  one cheap read; UX scope creep → UI is the existing workspace pattern.
  **Migration**: none. **DoD**: e2e: ask → fib drawn → verify frame in trace →
  drawing visible under AI layer; docs/42.

### Phase 5 — Permission tiers + auth groundwork (P10, P16, P17, P18-prep)

- **Goal**: Permission on descriptors + grants in ToolContext; scope claim on JWT;
  rate-limit the two bypass endpoints; global IP bucket on public compute; login
  throttling.
- **Dependencies**: Phase 3 (exposure machinery). **Modules**: auth.rs, rate_limit.rs,
  indicator_workspace_routes.rs, tools registry, db (0013_api_tokens.sql if tokens
  ship now, else with Phase 6). **Risks**: breaking the first-party app → app token
  carries all scopes; staged rollout. **DoD**: scope-less token cannot call DRAW
  tools; bypass endpoints limited; docs/43.

### Phase 6 — MCP gateway (§20)

- **Goal**: `crates/mcp-gateway`, stdio first; domains market/quant/chart(read+scoped
  write)/skills(read); capability listing; no execution surface.
- **Dependencies**: Phases 0-5 (registry, provenance, permissions — MCP must not
  freeze pre-registry answers). **Modules**: NEW crate; auth scopes. **Result**: an
  external MCP client runs the §41a footprint flow verbatim. **Risks**: protocol
  churn → pin SDK version; scope mistakes → default-deny domains. **DoD**: MCP
  conformance smoke test; per-domain listing matches registry; docs/44.

### Phase 7 — Monitors (§27)

- **Goal**: skill-declared triggers over the event engine; AI on trigger only;
  delivery through the existing alert lane; provider retries (P13) land here first.
- **Dependencies**: Phases 0-2 (skill requirements), existing event_engine +
  indicator_alerts. **Modules**: db (0012_monitors.sql NEW), api-gateway
  (monitors.rs NEW), ai-agent (monitor prompt path), providers (retry). **Risks**:
  cost runaway → per-monitor daily cap + global budget + dedup window; noisy skills →
  trigger kinds are closed enum. **DoD**: soak test: monitor fires on seeded sweep
  event, one alert, thesis attached, cap enforced; docs/45.

### Phase 8 — Deriv provider pilot (§22.2) + venue catalog (P20)

- **Goal**: Deriv adapter (ticks + candles + profile + symbol model); venue catalog
  replaces KNOWN_VENUES; the registry tells the Deriv story with zero engine changes.
- **Dependencies**: Phases 0-2 (the capability model *is* the pilot's spec).
- **Modules**: market-data (deriv_codec.rs NEW, profile, symbols), gateway
  (venue_routes catalog), frontend (venue picker reads catalog). **Risks**: Deriv
  candle volume semantics → declared as tick-count, verified against their docs at
  implementation time; symbol model mismatch → SymbolModel::Synthetic. **DoD**: the
  acceptance test of §22.2 runs green; `/capabilities` shows the Deriv matrix;
  docs/46.

### Phase 9+ — Deferred

pine-lite host series (P8); walk-forward / Monte-Carlo backtests; divergence-alert
writer; API-token UI; multi-user MCP HTTP transport; skill sharing — each gated on
demand evidence, none blocking the core architecture.

---

## 34. File-by-File Change Plan

Priorities: **P0** foundation/blocker · **P1** near-term value · **P2** architecture
payoff · **P3** later/optional. NEW FILE = does not exist today.

| File | Change | Why | Phase | Priority |
|---|---|---|---|---|
| crates/api-gateway/src/research.rs | **NEW FILE** — BacktestRunner impl over backtester+WindowService | P1 dead tools | A | **P0** |
| crates/api-gateway/src/agent_routes.rs | attach runner + merged skill library in ToolContext | P1/P2 | A | P0 |
| crates/api-gateway/src/ws.rs | same ToolContext wiring on the WS path | P1/P2 | A | P0 |
| crates/api-gateway/src/provider_routes.rs | resolve_agent merges DB skills | P2 | A | P0 |
| crates/capabilities/src/lib.rs | **NEW FILE** — DataKind/Availability/Quality | vocabulary | 0 | **P0** |
| crates/capabilities/src/descriptor.rs | **NEW FILE** — catalog (§13.2 rows) | registry | 0 | P0 |
| crates/capabilities/src/profile.rs | **NEW FILE** — ProviderDataProfile | provider truth | 0 | P0 |
| crates/capabilities/src/resolver.rs | **NEW FILE** — resolution + explanations | the join | 0 | P0 |
| crates/market-data/src/exchanges/profile.rs | **NEW FILE** — Binance/Bybit profiles | provider truth | 0 | P0 |
| crates/market-data/src/exchanges/venue.rs | add `profile()` to Venue (default = minimal) | seam | 0 | P0 |
| crates/api-gateway/src/capabilities.rs | add analysis-view section from resolver | ops+analysis one report | 0 | P1 |
| crates/analytics-core/src/state.rs | MarketState provenance block (only analytics change) | P4 | 1 | **P0** |
| crates/ai-agent/src/tools.rs | split into tools/{mod,market,quant,chart,memory,research,generation}.rs + descriptors + provenance stamps (mechanical move, same functions) | P3/P6 | 1 | P0 |
| crates/ai-agent/src/capability_view.rs | **NEW FILE** — prompt summary + stamping | P3 | 1 | P1 |
| crates/ai-agent/src/skills.rs | schema v2 + capability-filtered selection | §18/19 | 2 | P0 |
| crates/ai-agent/src/agent.rs | capability summary in prompt; selection; verify phase (Phase 4) | P6/P7 | 3,4 | P1 |
| skills/tool/*.yaml (≈7) | **NEW FILES** | §18 | 2 | P1 |
| skills/trading/{footprint/absorption, liquidity/sweep}.yaml | moved + annotated (ids unchanged) | §19 | 2 | P1 |
| crates/api-gateway/src/market_data.rs | drawing budgets; provenance unchanged | P7 | 4 | P2 |
| crates/api-gateway/src/indicator_workspace_routes.rs | route review/approve through limiter + user provider | P16 | 5 | **P1** |
| crates/api-gateway/src/rate_limit.rs | global/IP bucket for public compute; login throttle | P17 | 5 | P1 |
| crates/api-gateway/src/auth.rs | scope claim groundwork; (short-lived tokens later) | P10/P18 | 5 | P2 |
| crates/db/migrations/0012_monitors.sql | **NEW FILE** | §27 | 7 | P2 |
| crates/db/migrations/0013_api_tokens.sql | **NEW FILE** (may ride Phase 6) | §28 | 5/6 | P3 |
| crates/api-gateway/src/monitors.rs | **NEW FILE** — monitor worker | §27 | 7 | P2 |
| crates/ai-agent/src/providers/{bedrock,openai_compat}.rs | bounded retry w/ jitter | P13 | 7 | P2 |
| crates/mcp-gateway/src/*.rs | **NEW FILES** — server, auth, 4 domains | §20 | 6 | P2 |
| crates/market-data/src/exchanges/deriv_codec.rs | **NEW FILE** | §22.2 | 8 | P2 |
| crates/api-gateway/src/venue_routes.rs | catalog from provider registry (replaces KNOWN_VENUES const) | P20 | 8 | P2 |
| crates/api-gateway/src/bot_routes.rs | live balance read (real equity) | P19 | 8+ | P3 |
| crates/api-gateway/src/main.rs | wire stop_all on shutdown; assemble resolver/profiles | P19/Phase 0 | 0/8+ | P1 |
| crates/trading-engine/src/secrets.rs | AAD += account id (new seals; old still open) | P19 | 8+ | P3 |
| frontend/app/index.html + app.js | skill workspace UI; capability badges (read /capabilities) | §24/§16 | 4/1 | P2/P3 |
| crates/ai-agent/src/user_drawings.rs | fix stale doc comment (lines 11-16) | P12 | any | P3 |
| docs/38..46-*.md | **NEW FILES** — one per phase | convention | each | — |

Explicitly **unchanged**: strategy-dsl, strategy-runtime, backtester, sandbox,
sandbox-guest, pine-lite (until P8/Phase 9), chart-engine, builder.js, bus/history/
tape/candle_builder/window/collector/codec cores, trading-engine risk/gate/execution/
paper/live cores, observability, db migrations 0001-0011.

---

## 35. Architecture Anti-Patterns

Binding rules — what the architecture must **not** become (each maps to a real risk
observed in this audit or a trap the design deliberately avoids):

1. **AI as calculator.** No tool, skill, MCP method, or monitor may compute analytics
   outside analytics-core. (The codebase enforces this for tools; the rule now covers
   every new layer. Violation shape: "just format the delta in the prompt.")
2. **MCP as quant engine.** MCP translates; it never computes, fetches, or stores.
   Any compute needed by an MCP answer lands in an existing crate first.
3. **Derived presented as true.** No aliasing (tick-derived footprint ≠ footprint);
   provenance must survive every boundary — the Bybit direction-attributed split
   (§13.3.2) is the standing warning: provenance died at the seam.
4. **Silent degradation.** `[]` must never mean "unknown" (the MarketState absorption
   case, §13.3.3). Unavailable is a first-class answer with an explanation.
5. **Capability inference from data presence.** "Trades happened to be in the tape" is
   not a capability declaration; the resolver is the only answer.
6. **Tools as agents.** No tool may call the model, chain other tools, or loop.
7. **Per-request everything.** No all-tools-every-turn (P6), no re-derivation of
   capability answers per tool call (resolve once per request scope).
8. **Dynamic tool/artifact registration.** No runtime plugin loading, no
   generated-artifact-as-tool; static registration, dynamic *exposure* only.
9. **One mega-agent or premature multi-agent.** The bounded loop + terminal action is
   the shape; multi-agent decomposition waits for measured need.
10. **AI-managed execution.** No EXECUTE permission exists for tools; no agent path to
    OrderGateway; no "just this once" bot control via chat.
11. **Second math implementation in JS** (or anywhere) — engine-owns-geometry already
    holds on the chart (P15 residues are the warning); the same rule binds any new UI.
12. **Free-text capability claims in skills.** Skills declare typed requirements;
    prose rules interpret, never assert availability.
13. **Prompt-injected evidence.** Evidence comes from tool outputs with provenance,
    never from the user's prose or the model's prior (grounding already enforces this
    for prices; provenance extends it to data kinds).
14. **Persistence creep.** No new market-data tables "for convenience" — the 6GB
    decision stands until an explicit per-provider storage decision revisits it (P9).
15. **Global mutable registries with hidden state.** The registry is assembled at boot,
    immutable per boot, inspectable via /capabilities; no runtime mutation.

---

## 36. Architecture Invariants

Always true, before and after every phase:

1. analytics-core: pure, deterministic, no I/O, no async, wasm32-clean; NaN never
   crosses a boundary (None for warm-up); the ONLY math.
2. Dependency direction: analytics-core → (market-data | strategy-runtime | pine-lite
   | ai-agent | chart-engine) → api-gateway; nothing depends on api-gateway; ai-agent
   never names trading-engine/db/sandbox types (Cargo.toml:18-31 stays true with the
   new crates: capabilities is a leaf beside analytics-core; mcp-gateway sits at the
   gateway tier).
3. Data availability determines analytical capability — via the resolver, never via
   assumption; true ≠ derived is representable at every boundary.
4. AI never computes, never stores market data, never assumes data, never executes.
   NO TRADE / INSUFFICIENT DATA / LOW CONFIDENCE are first-class outputs
   (Bias::None, CheckStatus::Unknown already exist — thesis.rs:49-93).
5. Skills are model-independent declarative documents (YAML/JSON, versioned,
   append-only), resolvable without any LLM; the model consumes renders, never the
   source of truth.
6. Tools are deterministic thin wrappers; the same implementation serves internal,
   REST, and MCP callers; registration is static; exposure is dynamic.
7. Quant engines never call AI, never do I/O, never know providers exist.
8. Execution requires explicit human authorization (opt-in + gate + sealed keys);
   risk checks are always on (risk.rs:1-9); kill switch always available.
9. Every artifact (drawing, strategy, skill, monitor, thesis) carries provenance:
   created_by, version, and for data-derived things the capability + source.
10. The chart shell never computes market math; scenes are geometry (existing rule —
    docs/14/27).
11. Sandboxed execution for all untrusted strategy code (Decisions::Sandboxed is the
    only gateway path — decisions.rs:13-17).
12. New code explains itself in `//!` headers; docs drift is recorded in a ledger
    (docs/19 pattern), not silently tolerated.

---

## 37. First Implementation Target

**The smallest architectural foundation to implement first is the Capability
Descriptor + Provider Data Profile + Resolver — `crates/capabilities` plus one
`profile()` per provider — wired into the gateway and exposed through
`/capabilities`, with zero behavior change in tools.** (Phase 0, run in parallel with
the Phase A wiring of BacktestRunner, which is a broken-promise fix, not
architecture.)

Why this and not something else:

- **Everything cites it.** Data-aware skills (§19), dynamic tool exposure (§17.4),
  MarketState provenance (§23), MCP capability answers (§20.3), the Deriv pilot
  (§22.2), monitors (§27) — each *needs a queryable answer* to "what can this symbol
  support, and how honestly". Without the registry, every one of them invents a
  private version of the answer and the honesty guarantees fragment.
- **It is small and provable.** Four source files of pure types + one test file; the
  catalog is the §13.2 table transcribed; Binance/Bybit profiles restate facts the
  venue layer already documents (`taker_buy_base: Option`, aggTrades-only-on-Binance).
- **It is additive and reversible.** No tool changes behavior in Phase 0; the
  enforcement floor (per-tool guards) stays; the new surface is read-only
  (`/capabilities` extension).
- **It converts the audit's central finding into structure.** The three existing
  honesty seams (venue Option-split, tool `available:false`, deployment Readiness)
  become one mechanism instead of three conventions — and the Bybit
  derived-delta provenance leak (§13.3.2) gets its fix path.
- **It is the Deriv gate.** Deriv must never be "Binance minus fields"; the profile
  forces the provider to *declare* its truth before a single tool runs against it.

The first code after this document: `crates/capabilities/src/{lib,descriptor,profile,
resolver}.rs`, `crates/market-data/src/exchanges/profile.rs`, the boot assembly, the
`/capabilities` section — and in parallel `crates/api-gateway/src/research.rs`.

---

## 38. Open Questions / Decisions Requiring Human Input

1. **Deriv data truth** (blocks Phase 8): exact tick payload (price/epoch only?
   any volume field?), candle volume semantics (tick count vs contracts), historical
   tick availability/depth, symbol model for synthetic indices (fixed set? per-account?),
   and whether a tick-derived footprint variant is *desired* as its own capability or
   refused on principle. Requires reading Deriv's API contract — deliberately not
   guessed here.
2. **Tick storage policy** (blocks honest Deriv footprint + order-flow backtests):
   does the 6GB decision get a per-provider exception (e.g. 24h tick retention for
   watched symbols), or do tick analytics stay live-window-only? This is a product/
   cost decision, not an engineering one (P9).
3. **GEX scope**: the task names GEX; the repo has zero options surface (no chains,
   no Greeks, no OI). Confirm GEX is out of scope until an options-capable provider
   exists — otherwise a whole data tier (options chain + OI + Greeks) must enter the
   DataKind set and provider requirements.
4. **Skill sharing / marketplace**: single-user skills today; is multi-user sharing a
   goal (affects storage, moderation, and the propose-skill gate) or deliberately out?
5. **MCP transport & audience**: stdio-local (developer's own agents) vs streamable
   HTTP (remote clients)? The former ships Phase 6; the latter needs the API-token
   surface (0013) and a public-compute rate-limit decision (P17) first.
6. **Embeddings for skill retrieval**: deterministic scoring is honest and sufficient
   at ≤50 skills; confirm the embeddings threshold (skill count? retrieval miss rate?)
   or defer indefinitely.
7. **Backtest budgets**: how long may an agent-triggered backtest run inside a request
   (bars × TFs), and should heavy research queue instead (a `backtests_queued_total`
   metric name already exists — observability/metrics.rs:59 — implying a queued design
   was once intended; **NOT VERIFIED** whether a queue is still wanted)?
8. **Monitor pricing/caps**: per-monitor daily AI-invocation cap and global budget
   defaults — product decision (§27).
9. **JWT hardening timing**: scope claims ship with Phase 5; do short-lived tokens +
   refresh ship then too, or stay 7-day until multi-device demand appears (P18)?
10. **Sandbox-failure policy on live bots** (pre-existing, docs/19 row 26): halt-and-
    flatten vs continue — still undecided upstream; flagged here because monitors will
    eventually want to *report* it.
11. **Indicator workspaces on the deployment-primary model** (P16): fix to per-user
    provider resolution outright, or is the workspace surface deliberately
    deployment-funded?
12. **`/metrics` exposure**: public today by decision (docs/12:110) while exposing
    trading counters; re-confirm before MCP/API tokens make scraping attributable.

---

*End of document. Audit basis: direct inspection of the working tree on 2026-10-04;
subagent audits for ai-agent and api-gateway/trading/observability cross-checked
against direct reads; all line citations verified against the current tree.*





