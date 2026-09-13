# 00 — Vision & Architectural Principles

## Product vision

Not "another TradingView." This is an **AI-native order-flow trading terminal**: an
advanced order-flow chart (candlesticks, footprint, volume profile, delta/CVD, DOM) where
an AI agent understands market structure the same way a trader does — by reading
structured order-flow data, not screenshots — and can research, create, test, explain,
and (with explicit approval) execute trading strategies on the user's behalf, using the
user's own stated methodology rather than generic technical-analysis heuristics.

The user should be able to say things like:

> "Find me a long setup based on my strategy. Check 1D, 4H, 1H and 5M. I want a
> liquidity sweep followed by strong positive delta divergence, absorption around the
> previous POC, and confirmation on the 5M."

...and get back an explainable trade thesis with confidence, entry/stop/target,
invalidation conditions, and historical win rate for similar setups — generated from
real structured market data, not a hallucinated guess.

## The eight architectural principles

1. **Rust is the deterministic core.** Market-data ingestion, order-book processing,
   footprint/delta/CVD/volume-profile math, indicators, the strategy engine,
   backtesting, risk, and bot execution are all Rust. One implementation of the trading
   mathematics is shared by the backend, the WASM frontend, and the backtester — this
   guarantees "what you see on the chart == what the backtester tested == what the bot
   executed."

2. **The LLM is the reasoning layer, never the calculator.** The agent is never asked to
   compute a volume profile from raw trades. It calls a Rust tool (`get_volume_profile`),
   receives structured numeric output, and reasons/explains from there. This keeps the
   system fast, cheap, deterministic, and auditable.

3. **Structured data is the primary AI input; screenshots are secondary.** The agent
   should reason over JSON market-state objects (price, delta, CVD, POC, VAH/VAL,
   imbalances, absorption events, liquidity levels, swing highs/lows). A rendered chart
   image may be attached for genuinely visual questions, but it is never the only input.

4. **Multi-timeframe context is a first-class concept.** Every analysis request should be
   able to walk 1D → 4H → 1H → 5M (or any configured ladder), building macro context down
   to entry trigger, and produce a synthesized narrative across timeframes.

5. **One strategy representation, four execution targets.** A strategy/indicator is
   defined once in the Strategy DSL (see `docs/06-STRATEGY-DSL.md`) and can run
   unchanged as: a chart overlay, a backtest, a paper-trading bot, or a live bot.

6. **AI-generated logic never executes unsandboxed.** Natural language → DSL
   specification → validator → compiler → WASM sandbox → execution. No LLM output is
   ever `eval`'d or shelled out directly.

7. **Trader methodology is data (Skills), not a giant prompt.** Skills are versioned,
   structured documents (rules, conditions, examples, invalidation criteria, preferred
   markets/timeframes) retrieved contextually per request, not concatenated wholesale
   into every system prompt.

8. **Explainability is mandatory.** Every AI trade signal must be traceable: which skill
   fired, which conditions were satisfied on which timeframe, what the historical base
   rate for similar setups was, and what would invalidate the thesis. "BUY" with no
   reasoning is not an acceptable output anywhere in the system.

## Technology decisions locked in for this roadmap

| Layer | Choice | Rationale (from research doc) |
|---|---|---|
| Backend core | **Rust** (Tokio + Axum) | High-throughput market data, low-latency WS, CPU-heavy analytics |
| Analytics/math | **Rust**, one shared crate (`analytics-core`) | Single source of truth for chart, backtester, and bots |
| Strategy/indicator representation | **Rust-executed Strategy DSL** (declarative + optional Rust SDK for power users) | Not a Pine Script clone; safer, typed, shared across execution targets |
| AI-generated code sandbox | **Rust + WASM** | Deterministic, resource-limited, no filesystem/network/shell access |
| Backtesting | **Rust**, event-driven, parallelizable | Millions of candles/trades; needs to parallelize across symbols/timeframes |
| Database | **PostgreSQL** (+ Timescale-style hypertables for time-series) | Mature, well-understood, good Rust ecosystem (sqlx/sea-orm) |
| Frontend application shell | **Rust/WASM (Leptos or Dioxus)**, with React+TS as an accepted fallback for Phase 1 if agent velocity matters more than architectural purity | See `docs/14-FRONTEND-CHART-ENGINE.md` for the decision gate |
| Chart rendering | **Rust/WASM → Canvas/WebGL/WebGPU**, not per-cell DOM elements | Footprint/volume-profile charts can have hundreds of thousands of visual primitives |
| LLM access | **External APIs** (Claude, OpenAI, etc.) called from a Rust orchestration layer | Rust never "contains" the LLM; it calls it as a tool-using client |
| Real-time transport | **WebSocket**, binary framing for high-frequency chart/order-flow updates | Low overhead vs. JSON for tick-rate data |

## What this roadmap deliberately defers

- Which specific LLM provider(s) to integrate first — treated as pluggable from Phase 4
  onward (`docs/09-AI-AGENT-SYSTEM.md`).
- Which exchanges beyond the first (Binance, per the source research) to integrate —
  the market-data engine is designed exchange-agnostic from the start.
- Mobile/desktop native shells — web-first; Dioxus keeps that door open later without a
  rewrite.
- Live-money execution — paper trading is the Phase 6 target; live trading is gated
  behind the risk/compliance work in `docs/15-RISK-COMPLIANCE.md` and explicit user
  opt-in per venue.
