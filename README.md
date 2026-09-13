# AI-Native Order-Flow Trading Terminal — Build Roadmap

This folder is a complete, ordered specification for building the platform described in
the source research document (Rust-first, AI-native, order-flow trading terminal). It is
written to be handed directly to AI coding agents (Claude Code, or similar) as the
source of truth for implementation, one document at a time.

## How to use this with AI coding agents

1. Feed documents **in numeric order**. Each one assumes everything before it exists.
2. Treat each `docs/NN-*.md` file as one epic/milestone. Ask the agent to fully
   implement, test, and document one file before moving to the next.
3. `02-ROADMAP.md` is the master checklist — it maps every doc to a phase and gives
   exit criteria. Use it to track progress and to re-orient an agent that lost context.
4. `03-PROJECT-STRUCTURE.md` defines the exact repository/workspace layout every doc
   refers to. Generate this skeleton first, before any feature work.
5. Each doc includes: purpose, responsibilities, data structures/interfaces, and
   done-criteria. Interfaces are the contract — implementation details are the agent's
   to decide as long as the interface and behavior hold.

## MVP boundary

`02-ROADMAP.md` now has an explicit **MVP definition** section: Phases 0–5 (with the
scope cuts it lists) are the MVP — real data, real analytics, deterministic backtesting,
and an AI agent that produces a grounded, explainable thesis, wrapped in a minimal chart
page. No paper/live trading in the MVP. Phases 6–8 are post-MVP and add the paper-bot
loop, the full frontend, and live-money execution, in that order. Point an agent at the
roadmap's MVP table before assigning any phase so it knows the narrow first pass to build
before generalizing.

## Reading order

| # | Document | What it locks down |
|---|---|---|
| 0 | `00-VISION-AND-PRINCIPLES.md` | Product vision, non-negotiable architectural principles |
| 1 | `01-ARCHITECTURE-OVERVIEW.md` | System-wide component map and data flow |
| 2 | `02-ROADMAP.md` | Phased delivery plan with milestones and exit criteria |
| 3 | `03-PROJECT-STRUCTURE.md` | Cargo workspace / repo layout, crate boundaries |
| 4 | `docs/04-MARKET-DATA-ENGINE.md` | Ingestion of OHLCV, trades, order book from exchanges |
| 5 | `docs/05-ANALYTICS-ENGINE.md` | Footprint, delta, CVD, volume profile, market structure |
| 6 | `docs/06-STRATEGY-DSL.md` | The shared representation for indicators/strategies/bots |
| 7 | `docs/07-BACKTESTING-ENGINE.md` | Deterministic historical simulation |
| 8 | `docs/08-SANDBOX-WASM.md` | Safe execution of AI-generated code |
| 9 | `docs/09-AI-AGENT-SYSTEM.md` | LLM orchestration, tool-calling, multi-timeframe reasoning |
| 10 | `docs/10-SKILLS-SYSTEM.md` | Persistent trader-methodology knowledge base |
| 11 | `docs/11-BOT-TRADING-ENGINE.md` | Paper trading and live execution |
| 12 | `docs/12-API-GATEWAY.md` | REST/WebSocket surface for the frontend |
| 13 | `docs/13-DATABASE-SCHEMA.md` | PostgreSQL/TimescaleDB schema |
| 14 | `docs/14-FRONTEND-CHART-ENGINE.md` | Rust/WASM chart + application UI |
| 15 | `docs/15-RISK-COMPLIANCE.md` | Risk limits, kill-switches, audit trail |
| 16 | `docs/16-TESTING-STRATEGY.md` | Unit/integration/property/replay testing approach |
| 17 | `docs/17-DEPLOYMENT-INFRA.md` | Environments, CI/CD, observability infra |
| 18 | `docs/18-OBSERVABILITY.md` | Metrics, logging, tracing, alerting |

## Non-negotiables (see `00-VISION-AND-PRINCIPLES.md` for full detail)

- **Rust is the deterministic core.** Every calculation (delta, CVD, footprint, volume
  profile, indicators, backtests, order simulation, risk) lives in one shared Rust
  analytics crate used by backend, WASM frontend, and the backtester alike.
- **LLMs reason, they never calculate.** The agent selects tools, interprets structured
  results, and produces natural-language theses — it never computes trading math itself.
- **One strategy representation, four execution targets.** The same Strategy DSL document
  drives chart indicators, backtests, paper trading, and live bots — never four
  reimplementations that can drift out of sync.
- **AI-generated code never runs unsandboxed.** Every AI-authored indicator/strategy goes
  through validation and executes only inside the WASM sandbox.
- **Skills are data, not prompt text.** The trader's methodology lives in versioned
  skill documents that are retrieved contextually, not baked into a giant system prompt.

## Suggested execution cadence for the agent(s)

Work phase-by-phase per `02-ROADMAP.md`. Do not start Phase N+1 until Phase N's exit
criteria in the roadmap are met and committed.

---

# Running the stack locally

Status: **Phase 0 complete** — workspace skeleton, CI, and database wiring. Feature
crates are stubs; see `02-ROADMAP.md` for what lands when.

## 1. Prerequisites

- Rust 1.85+ (`rustup toolchain install stable`)
- Access to the managed Postgres instance (Northflank). No Docker required.

## 2. Configure the database

Copy the example env file and paste in the connection URL from the Northflank dashboard:

```bash
cp .env.example .env
# then edit .env:
#   DATABASE_URL=postgres://<user>:<password>@<host>:<port>/<db>?sslmode=require
```

`.env` is gitignored. Never commit real credentials.

Optional overrides: `DB_MAX_CONNECTIONS` (default 10), `DB_ACQUIRE_TIMEOUT_SECS`
(default 10), `RUST_LOG` (default `info`).

## 3. Apply migrations

```bash
cargo xtask migrate
```

Migrations live in `crates/db/migrations/` and are embedded at compile time by
`sqlx::migrate!`. They are written for plain PostgreSQL 13+ — no extensions required.
The TimescaleDB hypertable conversion is commented out at the bottom of
`0001_init.sql`; enable it only if your instance has the extension.

## 4. Run the gateway

```bash
cargo run -p api-gateway
# then:
curl http://127.0.0.1:8080/readyz
# => {"status":"ok","database":"up"}
```

`/healthz` is liveness (always ok). `/readyz` is readiness (probes the database).
The gateway starts even with no database configured, reporting `database: "down"` with
HTTP 503 — intentional, so a bad connection string doesn't cause a crash-loop.

## 5. Collect and backfill market data (Phase 1)

Backfill historical candles:

```bash
cargo xtask backfill --symbol BTCUSDT --timeframe 1m \
  --from 2026-01-01 --to 2026-07-01 --dry-run     # fetch + report only
cargo xtask backfill --symbol BTCUSDT --timeframe 1m \
  --from 2026-01-01 --to 2026-07-01               # also upsert into Postgres
```

Run the live collector (Ctrl-C to stop):

```bash
cargo xtask collect --symbol BTCUSDT          # stream + persist
cargo xtask collect --symbol BTCUSDT --no-persist   # stream only, no DB needed
```

### Choosing a backfill source

| `--source` | Speed | Matches live collector exactly? |
|---|---|---|
| `klines` (default) | ~1 request per 1000 candles | No — buy/sell split comes from `takerBuyBaseVolume` |
| `trades` | ~1 request per 1000 trades | **Yes** — feeds the same `CandleBuilder` |

Use `klines` for multi-month windows. Use `trades` for the short windows in the
"backfilled == live" regression test; it is hard-capped at 24h because replaying raw
trades over months would be hundreds of thousands of requests.

### What the collector does

- One combined WebSocket; `SUBSCRIBE` control messages; re-subscribes after every
  reconnect (without that, a dropped socket silently becomes "no data").
- Candles are built **from the trade stream**, not exchange klines, so the
  `buy_volume`/`sell_volume` split is consistent with delta/CVD downstream.
- Order book is maintained from the diff stream using Binance's snapshot+resync
  algorithm (buffer → drop stale → bridge at `lastUpdateId + 1` → apply).
- Trade-id gaps and book sequence gaps are **counted and logged**, never swallowed.
  Resyncing the affected window is still a manual step at this phase.
- Health is reported every 30s while collecting (connected / messages / gaps / reconnects).

## 6. Verify a clean checkout

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p analytics-core --target wasm32-unknown-unknown
```

The last command is a hard guarantee, not a nicety: `analytics-core` is the shared
deterministic core, and if it stops compiling to WASM the "one implementation of the
math" principle has silently broken.

## Repo layout

See `03-PROJECT-STRUCTURE.md` for the authoritative crate map and dependency rules.
In short:

| Crate | Phase | Purpose | Status |
|---|---|---|---|
| `analytics-core` | 2 | Pure math, no I/O, native + wasm32 | types done; math lands in Phase 2 |
| `market-data` | 1 | Exchange collectors, normalization | **Phase 1 done** |
| `strategy-dsl` | 3 | Schema, parser, validator | stub |
| `strategy-runtime` | 3 | Executes DSL against any data source | stub |
| `backtester` | 3 | Deterministic replay + reporting | stub |
| `sandbox` | 4 | WASM isolation for AI-generated strategies | stub |
| `ai-agent` | 5 | LLM orchestration, tools, skills, thesis | stub |
| `trading-engine` | 6/8 | Paper + live execution, risk | stub |
| `api-gateway` | 7 | Axum REST + WebSocket | health endpoints only |
| `db` | 1+ | sqlx models + migrations | pool + market-data repos done |

Helper binaries: `tools/xtask` (`cargo xtask <cmd>`), `tools/strategy-cli` (Phase 3).

xtask commands: `migrate`, `db-status`, `backfill`, `collect`.
