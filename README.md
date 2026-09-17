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
| 19 | `docs/19-AFTER-THE-ROADMAP.md` | Phase 8 split by risk, known debt, and the accuracy loop |

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

Status: **Phases 0–8 built.** Market data, `analytics-core`, the strategy
DSL/runtime/backtester, the WASM sandbox, the AI agent with 13 tools, the paper and live
trading bots, the REST/WebSocket gateway, and the Phase 8 hardening layer — metrics,
alerts, runbooks, and the exchange execution path.

One exit criterion is deliberately open: **a funded account placing and reconciling a
real order.** That needs live keys, and the platform will not ask for them. Everything
up to that point is proved against a venue double that dedups by client id exactly as
Binance does. See `docs/19-AFTER-THE-ROADMAP.md`, which also lists the debt that is
still open and the accuracy loop no phase covers.

## 1. Prerequisites

- Rust 1.85+ (`rustup toolchain install stable`)
- The `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- Access to the managed Postgres instance (Northflank). No Docker required.

The WASM target is needed for **any** workspace build, not just the frontend: the
`sandbox` crate's build script compiles the sandbox guest to WASM and embeds the module
in the host binary. Building without it fails with a message that says exactly this.

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

# The sandbox guest must link no YAML parser. YAML is default-off in the workspace
# [workspace.dependencies] entry; if this prints anything, something on the guest's
# path re-enabled strategy-dsl/yaml and widened the sandbox's attack surface.
cargo tree -p sandbox-guest --target wasm32-unknown-unknown | grep -E 'serde_yaml|unsafe-libyaml'
```

The `analytics-core` command is a hard guarantee, not a nicety: it is the shared
deterministic core, and if it stops compiling to WASM the "one implementation of the
math" principle has silently broken.

The last command should print **nothing** and exit non-zero; CI asserts exactly that.
The YAML gate is the one place in this workspace where a dependency change is a security
change, so it is checked against Cargo's *resolved* graph rather than the manifests —
feature unification would otherwise re-enable it silently.

## 7. The sandbox (Phase 4)

```bash
cargo test -p sandbox              # 22 adversarial + 5 equivalence + 1 doctest
cargo test -p sandbox --test equivalence    # native vs WASM, byte-identical results
cargo test -p sandbox --test adversarial    # malformed / hostile inputs fail safely
```

Two things are worth knowing before touching this:

- **The guest is built by `sandbox`'s build script**, not committed as a `.wasm`. The
  module embedded in the host binary is always the one the current source produces.
  The nested `cargo build` uses its own `--target-dir` (`target/sandbox-guest`) because
  Cargo holds an exclusive lock on the target directory for the duration of a build.
- **`serde_json`'s `float_roundtrip` feature is load-bearing.** Without it the default
  float parser is one ULP off for some inputs, and every sandboxed trade disagrees with
  its native twin in the last decimal. See the implementation notes in `docs/08`.

## 8. Checking the LLM provider (Phase 5)

```bash
~/.workbuddy-ai/binaries/python/envs/default/Scripts/python.exe tools/bedrock_check.py
```

Verifies the three properties the agent design depends on, none of which is safe to
assume: that `AWS_BEDROCK_MODEL_ID` exists in the region (Bedrock often needs a full id
or an inference-profile ARN), that the model supports the Converse API's tool use, and
that it can consume a `toolResult` and answer. Needs `boto3`; run it before changing the
model id.

Verified against `qwen.qwen3-coder-next` in `us-east-1` on 2026-09-13 — all three pass.

One finding from that run shapes the agent: **the model reformats the numbers it is
given** (`103250.5` came back as `$103,250.50`). The explainable-thesis object must
therefore carry the structured values straight from the tool results — never recover
numbers by parsing the model's prose, and never string-match prose to check a claim.

## Repo layout

See `03-PROJECT-STRUCTURE.md` for the authoritative crate map and dependency rules.
In short:

| Crate | Phase | Purpose | Status |
|---|---|---|---|
| `analytics-core` | 2 | Pure math, no I/O, native + wasm32 | **Phase 2 done** |
| `market-data` | 1 | Exchange collectors, normalization | **Phase 1 done** |
| `strategy-dsl` | 3 | Schema, parser, validator | **Phase 3 done** |
| `strategy-runtime` | 3 | Executes DSL against any data source | **Phase 3 done** |
| `backtester` | 3 | Deterministic replay + reporting | **Phase 3 done** |
| `sandbox` | 4 | WASM isolation for AI-generated strategies | **Phase 4 done** |
| `sandbox-guest` | 4 | The interpreter compiled to WASM; embedded by `sandbox`'s build script | **Phase 4 done** |
| `ai-agent` | 5 | LLM orchestration, tools, skills, thesis | **Phase 5 done** |
| `observability` | 8 | Metrics registry, alert rules, structured logs | **Phase 8 done** |
| `trading-engine` | 6/8 | Paper + live execution, risk, the exchange adapter | **Phase 6/8 done** |
| `api-gateway` | 7 | Axum REST + WebSocket | **Phase 7 done** |
| `db` | 1+ | sqlx models + migrations | done through `0002` |

`observability` is a leaf crate for a reason worth knowing: `docs/03` forbids
`api-gateway` from depending on `trading-engine`, so without it the same metric names
and alert rules would exist in five copies and drift.

**There is exactly one metric registry per process, and it is `Registry::global_handle()`.**
A binary that serves `/metrics` must inject that handle, not `Arc::new(Registry::new())`.
This is not a style preference: the libraries (`trading-engine`, `market-data`) write to
the global, so a service that builds its own serves a registry those crates never touch —
and the alert task evaluates that same empty one, which silently disables every alert that
reads a counter. Tests inject their own registry so they can assert on their own numbers.

Helper binaries: `tools/xtask` (`cargo xtask <cmd>`), `tools/strategy-cli`, `tools/paper-cli`.

xtask commands: `migrate`, `db-status`, `backfill`, `collect`.

## Strategy CLI

```bash
# Check a document without executing it. Reports every field-level problem at once.
cargo run -p strategy-cli -- validate --strategy strategies/liquidity-sweep.yaml

# Replay it over a window and write a JSON performance report.
cargo run -p strategy-cli -- backtest run \
  --strategy strategies/liquidity-sweep-btcusdt-5m.yaml \
  --symbol BTCUSDT --from 2026-03-13 --to 2026-09-13 \
  --source-timeframe 5m \
  --report-out reports/liquidity-sweep-btcusdt-5m-2026h1.json

# Re-check a recorded run against the candle table. Exits non-zero on any finding.
cargo run -p strategy-cli -- verify \
  --report reports/liquidity-sweep-btcusdt-5m-2026h1.json --trades 40
```

`--source-timeframe` (default `1m`) is the resolution a declared timeframe is aggregated
from when the database holds no candles at that resolution of its own. Aggregation is
exact for OHLCV, including the buy/sell volume split, so a multi-timeframe document can
run off a single backfilled series.

`verify` is the independent half of the backtest: it reads the trades a run recorded and
asks whether the stored market data supports them — that the entry is the bar's open moved
by the recorded slippage, that the decision was made on a *closed* bar, that the level the
trade says it exited at was actually reached. `replay_golden.rs` cannot do this, because its
fixture was generated by the implementation it would be testing.

`strategies/liquidity-sweep.yaml` is the example from `docs/06-STRATEGY-DSL.md`, kept
verbatim. `strategies/liquidity-sweep-btcusdt-5m.yaml` is the same thesis calibrated to
real data — see the implementation notes in `docs/06` for why the spec's version needs
a reclaim condition and a realistic imbalance threshold to trade at all.

---

# Docker

`Dockerfile` is a two-stage build (Rust builder → `debian:bookworm-slim`) producing
the **api-gateway** and **xtask** binaries. It builds with `--locked`, so a stale
`Cargo.lock` fails the build rather than silently resolving new versions.

## Run locally

```bash
docker compose up --build
curl http://localhost:8080/healthz     # {"status":"ok","database":"unknown"}
curl http://localhost:8080/readyz      # {"status":"ok","database":"up"}
```

`docker compose up -d postgres` alone gives you a local database without touching
the gateway — useful if you want to keep running the services with `cargo run`.

## Why these choices

- **`BIND_ADDR=0.0.0.0:8080` is set in the image.** The binary defaults to
  `127.0.0.1`, which inside a container means unreachable from the outside.
- **Migrations run from the entrypoint**, not as a deploy step. sqlx takes a Postgres
  advisory lock, so concurrent migrators serialize instead of racing — safe even with
  several replicas. Set `RUN_MIGRATIONS=false` to skip.
- **The healthcheck hits `/healthz`, not `/readyz`.** `/healthz` is liveness only and
  doesn't touch the database, so a transient DB blip won't flap the container.
- **`.env` is in `.dockerignore`.** Secrets are injected as environment variables by
  the platform, never baked into an image layer.

# Deploying to Northflank

Northflank auto-detects the root `Dockerfile`, so no build config file is needed.

1. **Create the service** — *Create* → *Service* → *Deployment*, point it at
   `Haizard/hrz-native-ai-trading-platform`, branch `main`. Build type: Dockerfile.
2. **Port** — add an HTTP port `8080`. This matches `EXPOSE 8080` and the
   `BIND_ADDR` the image sets.
3. **Health check** — path `/healthz`, port `8080`.
4. **Environment variables** (do not put these in the repo). The full reference — including
   what each one's *absence* does — is `docs/17-DEPLOYMENT-INFRA.md`:

   | Variable | Value | Needed for |
   |---|---|---|
   | `DATABASE_URL` | the Northflank Postgres addon's **internal** connection string | everything |
   | `JWT_SECRET` | `openssl rand -base64 48`; **≥ 32 bytes or startup refuses it** | `/auth/*`, and every route needing a token |
   | `MARKET_FEED` | `binance` | bots receiving candles at all |
   | `AWS_BEDROCK_MODEL_ID` | `qwen.qwen3-coder-next` | `/agent/*` |
   | `AWS_BEDROCK_REGION` | `us-east-1` | `/agent/*` (defaults to this) |
   | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | Bedrock credentials | `/agent/*` |
   | `BINANCE_API_KEY` / `BINANCE_API_SECRET` | exchange credentials | **live trading** — without them a venue opts in but reports `credentials_configured: false` |
   | `RUST_LOG` | `info` | log verbosity |
   | `RUN_MIGRATIONS` | `true` for the first deploy | the entrypoint applying migrations |

   `BIND_ADDR` is already set in the image; override it only if you change the port.
   `ALERT_WEBHOOK_URL` is optional — unset means alerts go to the log and the audit trail
   only, which the gateway says at startup.

   **Nothing here refuses to boot when missing**, by design: `main` serves `/healthz` even
   with no database so the platform reports "up but not ready" rather than crash-looping.
   The cost is that a missing variable shows up as one feature answering 503, not as a
   failed deploy — so check `GET /readyz` and the startup lines rather than assuming a
   green container means a configured one.

5. **Database** — add a Postgres addon and link it to the service so
   `DATABASE_URL` is injected automatically, or paste the connection string manually.
   Use the internal host, not the public one, so traffic stays inside the cluster.
6. **Deploy.** The entrypoint applies migrations, then starts the gateway.

## Notes

- Migrations are idempotent (`CREATE TABLE` runs once; sqlx tracks applied versions in
  `_sqlx_migrations`), so repeated deploys are safe.
- The image has no shell tooling beyond `curl` and `ca-certificates`. `xtask` is
  available inside the container for one-off jobs:
  `xtask backfill --symbol BTCUSDT --timeframe 1m --from ... --to ...`
- **Collectors are long-running processes, not web services.** The `collect` command
  should run as a separate Northflank *Worker* service with no exposed port, not inside
  the gateway container. That separation lands with the Phase 1 hardening work.
