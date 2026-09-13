# 02 — Phased Roadmap & Milestones

Each phase lists: goal, the docs it draws on, concrete deliverables, and exit criteria.
An AI coding agent should not move to the next phase until exit criteria are met.

## MVP definition — read this before assigning work to an agent

**The MVP is Phases 0–5.** That slice alone is a complete, demoable product: real
market data in, real order-flow analytics, a strategy representation that backtests
deterministically, and an AI agent that answers trading questions with real numbers and
an explainable thesis. No money moves in the MVP — there is no live trading and no
paper-trading bot loop yet. That is deliberate: the platform's actual differentiator
(AI reasoning over real structured order-flow data) is fully provable without touching
execution risk at all.

**Phases 6–8 are post-MVP.** They turn a working analysis tool into an autonomous
trading system: paper bots (6), a real UI wrapped around it (7 — though see the MVP-cut
note below, a bare-bones UI is pulled forward), and live money + production hardening
(8). Do not let an agent start Phase 6 work believing it's still "core" — it's an
entirely different risk profile (real or simulated capital, not just data and reasoning)
and should be scoped, reviewed, and greenlit as its own decision.

**MVP scope cuts within Phases 0–5** — build the narrow version first, generalize later:

| Area | Full vision (later) | MVP cut (build first) |
|---|---|---|
| Exchanges/symbols | Multi-exchange, arbitrary symbol list | One exchange (Binance), one symbol (BTCUSDT) |
| Analytics (Phase 2) | Footprint, delta, CVD, volume profile, imbalance, absorption, liquidity, market structure, indicator library | Delta, CVD, VWAP, volume profile (POC/VAH/VAL) first — these alone support a real thesis. Footprint/imbalance/absorption/market-structure can land as a fast-follow within the same phase, but don't block Phase 3 on them if time-boxing matters |
| Strategy DSL (Phase 3) | Full condition grammar across arbitrary timeframe ladders | Single-timeframe conditions over the Phase-2 MVP fields (price, delta, CVD, POC/VAH/VAL) is enough to prove backtesting works end-to-end |
| Sandbox (Phase 4) | Full WASM isolation for any AI-generated strategy | Still required before any AI-generated (not hand-written) strategy executes — this is a safety boundary, not a nice-to-have, so it is **not** cut even in the MVP. What can be deferred is the exhaustive adversarial fuzz suite; the core allowlist + resource limits must exist from the start |
| AI Agent (Phase 5) | Multi-timeframe ladder, multi-agent decomposition, `backtest_similar_setups` base rates | Single-timeframe `analyze_timeframe` + one Skill + a thesis object is a real, demoable MVP. Multi-timeframe synthesis is the natural fast-follow, not a blocker |
| Frontend | Full Rust/WASM chart engine, three-mode strategy editor, DOM panel | A minimal chart (candlesticks + one overlay, even a simple web page hitting the REST/WS API) so the MVP is actually usable end-to-end, not just testable via CLI/API. Treat this as "MVP-0.5 of Phase 7," pulled forward rather than waiting for the full Phase 7 build |

**Practical instruction to the agent:** build Phases 0–5 with the MVP-cut scope above,
demo/validate the end-to-end flow (`analyze_timeframe` → thesis, plus a minimal chart to
see it), *then* decide — with the user, not autonomously — whether to broaden Phase 2's
analytics and Phase 5's multi-timeframe reasoning before or after starting Phase 6.

---

## Phase 0 — Foundations (repo, workspace, CI skeleton) · MVP

**Goal:** a buildable, empty-but-structured Rust workspace with CI, before any feature
code exists.

**Draws on:** `03-PROJECT-STRUCTURE.md`, `docs/17-DEPLOYMENT-INFRA.md` (CI section only)

**Deliverables**
- Cargo workspace with all crates from `03-PROJECT-STRUCTURE.md` stubbed out
  (`lib.rs`/`main.rs` with a placeholder, compiling).
- Postgres running locally via docker-compose; connection pool wired but unused.
- CI pipeline: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, on every PR.
- `README.md` at repo root explaining how to run the whole stack locally.

**Exit criteria:** `cargo build --workspace` and `cargo test --workspace` both succeed
in CI on a clean checkout.

---

## Phase 1 — Market Data Engine · MVP (one exchange, one symbol)

**Goal:** continuous, persisted, normalized market data from at least one exchange.

**Draws on:** `docs/04-MARKET-DATA-ENGINE.md`, `docs/13-DATABASE-SCHEMA.md`

**Deliverables**
- Binance WebSocket collector for trades, order book diffs, and candle aggregation.
- Normalization into the shared `Trade`/`OrderBookSnapshot`/`Candle` types.
- Persistence to Postgres/Timescale-style hypertables.
- A minimal internal pub/sub so other services can subscribe to live ticks.
- Backfill tool to pull historical OHLCV/trades for a symbol/date range.

**Exit criteria:** running the collector for 24h against BTCUSDT produces a queryable,
gap-checked candle table at 1m/5m/1h/4h/1d resolutions, plus raw trades and order-book
snapshots.

---

## Phase 2 — Analytics Core · MVP (delta/CVD/VWAP/volume profile first)

**Goal:** every trading calculation implemented once, as a pure Rust library, fully
unit-tested against known reference values.

**Draws on:** `docs/05-ANALYTICS-ENGINE.md`

**Deliverables**
- Footprint, delta, CVD, VWAP, POC/VAH/VAL, volume profile, imbalance, absorption,
  liquidity-level detection, basic market-structure (swing highs/lows, BOS/CHoCH).
- A small library of classic indicators (EMA, SMA, RSI, ATR) for parity/testing.
- Golden-file tests: fixed input datasets with pre-computed expected outputs.

**Exit criteria:** `analytics-core` has no I/O dependencies, compiles to native and
`wasm32-unknown-unknown` targets, and its test suite covers every function listed above.

---

## Phase 3 — Strategy DSL & Backtesting Engine · MVP (single-timeframe conditions)

**Goal:** a shared strategy representation that can be replayed deterministically
against historical data.

**Draws on:** `docs/06-STRATEGY-DSL.md`, `docs/07-BACKTESTING-ENGINE.md`

**Deliverables**
- DSL schema (rules, conditions, timeframes, risk block) + parser/validator.
- Event-driven backtest engine: replay candles/trades → evaluate DSL → simulate
  orders/positions/PnL.
- Performance report generator (win rate, profit factor, Sharpe, max drawdown, R
  distribution).
- CLI tool: `backtest run --strategy my_strategy.yaml --symbol BTCUSDT --from ... --to ...`

**Exit criteria:** a hand-written sample strategy (liquidity sweep + absorption) backtests
over at least 6 months of BTCUSDT data and produces a performance report matching
manually-verified spot checks on a handful of trades.

---

## Phase 4 — Sandbox (WASM execution) · MVP (core isolation, not full fuzz suite)

**Goal:** any Strategy DSL document — including ones an LLM will later generate — can be
compiled and executed in an isolated environment.

**Draws on:** `docs/08-SANDBOX-WASM.md`

**Deliverables**
- WASM compilation target for the Strategy Runtime.
- Capability allowlist enforcement (no fs/network/shell/env access from sandboxed code).
- Resource limits: execution time, memory, instruction count.
- Fuzz/adversarial test suite: intentionally malformed or resource-exhausting strategy
  specs must fail safely.

**Exit criteria:** the Phase 3 sample strategy runs identically whether executed natively
or inside the WASM sandbox, and at least 10 adversarial inputs are rejected without
crashing the host process.

---

## Phase 5 — AI Agent Engine & Skills System · MVP (single-timeframe thesis)

**Goal:** natural-language trading requests produce explainable, data-grounded theses.

**Draws on:** `docs/09-AI-AGENT-SYSTEM.md`, `docs/10-SKILLS-SYSTEM.md`

**Deliverables**
- Tool registry exposing Analytics Core functions to the agent
  (`get_footprint`, `get_volume_profile`, `analyze_timeframe`, `analyze_multi_timeframe`,
  `detect_liquidity`, `detect_absorption`, etc.).
- LLM client abstraction supporting at least one provider, swappable.
- Skills storage format (YAML/JSON, versioned) + retrieval-by-relevance.
- Multi-timeframe reasoning pipeline producing the explainable-thesis object defined in
  `docs/09-AI-AGENT-SYSTEM.md`.
- Natural-language → Strategy DSL generation, validated before it ever reaches the
  sandbox.

**Exit criteria:** given "find me a long setup on BTC using my liquidity-sweep skill,"
the agent returns a thesis object with real numbers pulled from Analytics Core (not
hallucinated), citing which skill and which timeframe conditions fired.

**MVP wrap-up (end of Phase 5):** at this point, stand up the minimal chart described in
the MVP-cut table above and wire it to `/agent/ask` so the thesis is visible next to a
real candlestick chart, not just returned as JSON. This is the actual "is the MVP done"
checkpoint — a person should be able to open a page, look at BTCUSDT, ask a trading
question, and get back a grounded, explainable answer.

---

## Phase 6 — Trading Engine: Paper Trading · POST-MVP

**Goal:** an approved strategy can run continuously against live data in simulation.

**Draws on:** `docs/11-BOT-TRADING-ENGINE.md`, `docs/15-RISK-COMPLIANCE.md`

**Deliverables**
- Paper-trading executor consuming live ticks through the same Strategy Runtime used in
  backtesting.
- Simulated order/position/PnL tracking with realistic fill assumptions (slippage,
  partial fills optional at this stage).
- Risk Engine: per-trade and per-account risk limits, kill-switch.
- Notifications/audit log of every simulated trade.

**Exit criteria:** the Phase 3 sample strategy runs as a paper bot for at least 48h
against live BTCUSDT data without crashing, respecting configured risk limits.

---

## Phase 7 — API Gateway & Frontend · POST-MVP (full build; minimal chart pulled into MVP above)

**Goal:** a usable web application exposing charting, AI chat, strategy creation, and
backtest/paper-trading dashboards.

**Draws on:** `docs/12-API-GATEWAY.md`, `docs/14-FRONTEND-CHART-ENGINE.md`

**Deliverables**
- REST endpoints for account/auth, strategies, backtests, skills.
- WebSocket channels for live candles/order-flow/AI-chat streaming.
- Rust/WASM (or React/TS fallback — see decision gate in doc 14) chart rendering
  candlesticks, footprint, and volume profile.
- AI chat panel wired to the Phase 5 agent, with chart-region highlighting.
- Strategy editor: natural language, visual builder, and raw-DSL modes.

**Exit criteria:** an end-to-end user session — log in, view BTCUSDT footprint chart,
ask the AI for a setup, review the thesis, run a backtest, launch a paper bot — completes
without manual intervention.

---

## Phase 8 — Live Trading, Observability, Hardening · POST-MVP (real money)

**Goal:** production readiness for real-money execution, with full observability.

**Draws on:** `docs/15-RISK-COMPLIANCE.md`, `docs/16-TESTING-STRATEGY.md`,
`docs/17-DEPLOYMENT-INFRA.md`, `docs/18-OBSERVABILITY.md`

**Deliverables**
- Exchange order-execution adapters with idempotency and reconciliation.
- Full metrics/logging/tracing/alerting per `docs/18-OBSERVABILITY.md`.
- Load/chaos testing of the market-data and WebSocket fan-out paths.
- Documented incident runbooks and a manual kill-switch reachable from the UI.

**Exit criteria:** live trading is gated behind explicit per-venue opt-in, a funded
account can place and reconcile a real order under the Risk Engine's limits, and an
on-call engineer can diagnose an incident using dashboards alone (no ad-hoc log diving).

---

## Cross-phase, always-on requirements

- Every phase's code ships with tests (`docs/16-TESTING-STRATEGY.md`).
- Every new external-facing endpoint is documented in `docs/12-API-GATEWAY.md` as it's
  built, not retroactively.
- No phase introduces a second implementation of anything already in
  `analytics-core` — if a calculation is missing, add it there.
