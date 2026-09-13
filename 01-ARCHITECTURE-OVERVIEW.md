# 01 — Architecture Overview

## System diagram (logical)

```
                                   USER
                                     │
                     ┌───────────────┴───────────────┐
                     │        RUST FRONTEND           │
                     │   (Leptos/Dioxus + WASM)        │
                     │                                 │
                     │  ┌───────────┐  ┌────────────┐ │
                     │  │  Chart     │  │  App Shell │ │
                     │  │  Engine    │  │  (panels,  │ │
                     │  │ (WebGPU/   │  │  AI chat,  │ │
                     │  │  Canvas)   │  │  forms)    │ │
                     │  └───────────┘  └────────────┘ │
                     └───────────────┬─────────────────┘
                                     │ WebSocket / REST
                                     ▼
                     ┌─────────────────────────────────┐
                     │        RUST API GATEWAY          │
                     │        (Axum + Tokio)            │
                     └───────────────┬─────────────────┘
                                     │
       ┌─────────────────────────────┼──────────────────────────────┐
       ▼                             ▼                              ▼
┌─────────────┐             ┌────────────────┐             ┌────────────────┐
│ MARKET DATA  │             │  AI AGENT       │             │ TRADING ENGINE │
│ ENGINE       │             │  ENGINE         │             │                │
│              │             │                 │             │  Strategy      │
│ Exchange WS  │             │  Skills store   │             │  Runtime       │
│ collectors   │             │  Tool registry  │             │  (DSL exec)    │
│ Order book   │             │  LLM client(s)  │             │                │
│ Trade stream │◄───────────►│  Multi-TF       │◄───────────►│  Backtester    │
│ OHLCV builder│  analytics  │  reasoning      │  strategy    │  Paper trader  │
└──────┬───────┘   queries   └────────┬────────┘  specs       │  Live bot exec │
       │                              │                       │  Risk engine   │
       │                              ▼                       └───────┬────────┘
       │                    ┌──────────────────┐                      │
       │                    │  WASM SANDBOX     │                      │
       │                    │ (AI-generated     │◄─────────────────────┘
       │                    │  indicators/      │   compiled strategy specs
       │                    │  strategies)      │
       │                    └──────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│                    ANALYTICS CORE (shared crate)              │
│  Footprint · Delta · CVD · VWAP · POC/VAH/VAL · Volume        │
│  Profile · Imbalance · Absorption · Liquidity · Market        │
│  Structure · Indicators                                       │
│  — used by: backend, WASM frontend, backtester, sandbox —     │
└───────────────────────────────┬───────────────────────────────┘
                                 ▼
                    ┌─────────────────────────┐
                    │  PostgreSQL / Timescale  │
                    │  OHLCV · trades ·        │
                    │  order book snapshots ·  │
                    │  skills · strategies ·   │
                    │  backtests · users       │
                    └─────────────────────────┘
```

## Component responsibilities (one line each)

- **Market Data Engine** — connects to exchange WebSocket/REST feeds, normalizes trades/
  order book/OHLCV, persists to Postgres, publishes live ticks internally.
- **Analytics Core** — pure, deterministic Rust library computing every trading
  calculation from normalized market data; no I/O, no async, fully unit-testable.
- **Strategy DSL & Runtime** — the shared declarative representation of an
  indicator/strategy/bot and the engine that executes it against any data source
  (live, historical, simulated).
- **Backtesting Engine** — replays historical data through the Strategy Runtime and
  produces performance statistics.
- **Sandbox (WASM)** — compiles/executes AI-generated or user-authored strategy code in
  an isolated, resource-limited environment with an explicit capability allowlist.
- **AI Agent Engine** — orchestrates LLM calls, tool-calling against Analytics Core and
  Strategy Runtime, retrieves relevant Skills, and produces theses/specs/explanations.
- **Skills System** — versioned, structured trader-methodology documents that are
  retrieved and injected into agent context on demand.
- **Trading Engine (paper/live)** — turns an approved strategy into simulated or real
  order placement, subject to the Risk Engine's limits.
- **API Gateway** — the single REST/WebSocket surface the frontend talks to; owns auth,
  rate limiting, and request routing to the internal engines.
- **Frontend (Rust/WASM)** — the chart, order-flow visualization, AI chat panel, strategy
  editor, and account/settings UI.

## Data flow: one worked example

"Find me a long setup on BTC" →

1. Frontend sends the natural-language request over WebSocket to the API Gateway.
2. API Gateway routes it to the AI Agent Engine.
3. Agent retrieves the user's relevant Skills (e.g. "Liquidity Sweep + Absorption v2").
4. Agent calls Analytics Core tools per timeframe: `analyze_timeframe("1D")`,
   `("4H")`, `("1H")`, `("5M")` — each returns structured JSON (delta, CVD, POC,
   imbalances, absorption, liquidity levels, structure).
5. Agent synthesizes a multi-timeframe narrative and, if conditions match the skill's
   rules, calls `backtest_similar_setups()` against historical data for a base rate.
6. Agent returns an explainable thesis object (confidence, entry, stop, target, R:R,
   invalidation, skill used, historical win rate) to the Gateway.
7. Gateway pushes it to the Frontend, which renders the thesis panel and highlights the
   relevant chart regions.
8. If the user clicks "Create Bot," the same strategy specification is handed to the
   Strategy Runtime for backtest confirmation, then paper trading, then (opt-in) live.

## Cross-cutting contracts every doc must respect

- **Market data types** (`Candle`, `Trade`, `OrderBookSnapshot`, `FootprintCell`) are
  defined once in `analytics-core` and reused everywhere — no per-service duplicate
  structs.
- **Strategy documents** are the only artifact that crosses the AI/deterministic
  boundary. The LLM never emits raw Rust or raw SQL that gets executed directly.
- **All timestamps are UTC, nanosecond-precision `i64`** internally; presentation-layer
  formatting happens only at the frontend edge.
- **Every engine exposes health/readiness endpoints** from Phase 1 onward so the
  deployment/observability docs have something to hook into immediately.
