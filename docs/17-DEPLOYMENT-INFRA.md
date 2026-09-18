# 17 — Deployment & Infrastructure

## Purpose
Get the platform running reliably across local dev, staging, and production
environments, with CI/CD from Phase 0 onward.

## Environments
- **Local dev**: `docker-compose.yml` running Postgres (+ Timescale extension if used);
  all Rust services run natively via `cargo run` against it.
- **Staging**: mirrors production topology at smaller scale; used for the load/chaos
  tests in `docs/16-TESTING-STRATEGY.md` and for paper-trading validation before any
  live-trading feature ships.
- **Production**: real market data, real users; live trading only enabled per the
  gating in `docs/15-RISK-COMPLIANCE.md`.

## CI/CD (set up in Phase 0, per `02-ROADMAP.md`)
- On every PR: `fmt`, `clippy`, `test`, the wasm test target, the sandbox guest's
  dependency graph, and the **chart engine** — `xtask build-frontend` builds the wasm
  that actually ships and `tools/wasm_abi_check.mjs` instantiates those same bytes in
  Node. That last one is the only check that can see a renamed export, which is not a
  compile error anywhere; it is a blank canvas in a browser nobody is watching.
- On merge to main: build release binaries/containers for each service crate, build the
  frontend WASM bundle, run the regression/replay tests from
  `docs/16-TESTING-STRATEGY.md`, then deploy to staging automatically; production
  deploy is a manual promotion step.
- Database migrations run automatically as a pre-deploy step, never manually against
  production.

### The container image, and how far it has been verified

`Dockerfile` is a two-stage build: `rust:1.98-bookworm` compiles `api-gateway` and `xtask`
in release, and `debian:bookworm-slim` runs them. `docker-entrypoint.sh` applies pending
migrations (`xtask migrate`, guarded by `RUN_MIGRATIONS`) and then execs the command —
which is why `xtask` is in the image and not only the gateway.

**The image has not been built.** There is no container runtime on the development machine
and no deploy workflow yet, so the checks below are static — and
`crates/api-gateway/tests/packaging.rs` is what holds them. It exists because this file's
predecessor was two lines that were each true when written and had quietly stopped being
true, and the failure was silent: the page 404'd and the agent started with an empty skill
library.

What is checked, and would otherwise be found by a build:

- The base tag `rust:1.98-bookworm` exists — verified against the registry (amd64 and
  arm64, pushed 2026-09-09). It was an unexamined assumption for the life of the file.
- The committed `frontend/app/chart_engine.wasm` is **byte-identical** to a fresh
  `xtask build-frontend`. Nothing in the Dockerfile rebuilds it, so the image ships
  whatever is committed; a stale engine would deploy with no error at all.
- Every binary the build line produces is copied into the runtime stage, and every binary
  the entrypoint invokes is one of them.
- `EXPOSE`, `BIND_ADDR` and the healthcheck all name the same port. A healthcheck on the
  wrong port marks a perfectly healthy container unhealthy, forever.
- `docker-entrypoint.sh` is LF, starts with a shebang, and is the file `ENTRYPOINT` names.

What remains unverified, and can only be settled by an actual build: that the release
profile compiles on **Linux** (the local `--locked` build is Windows), that `rustls` needs
no system library the slim image lacks, and that the built image starts and answers
`/healthz`.

## Service topology (suggested; adjust as load characteristics become clear)
```
market-data (1+ instances, one per exchange or sharded by symbol set)
        │
   internal pub/sub / shared Postgres
        │
api-gateway (stateless, horizontally scalable)
        │
ai-agent, trading-engine, backtester (can scale independently; backtester benefits most
from horizontal scaling for parallel parameter sweeps)
        │
Postgres/Timescale (primary + read replica once read load justifies it)
```
Do not introduce a message broker (Kafka/NATS/etc.) preemptively — the in-process
`tokio::broadcast` pub/sub from `docs/04-MARKET-DATA-ENGINE.md` is sufficient until a
concrete, measured cross-process scaling need appears (e.g. multiple `api-gateway`
instances needing the same live tick stream) — at that point, introduce the smallest
tool that solves the specific problem, and document why here.

## Secrets management
- Exchange API keys, LLM provider keys, database credentials: never committed, injected
  via environment variables or a secrets manager appropriate to the hosting platform.
  The sandbox must never have access to any of these (see `docs/15-RISK-COMPLIANCE.md`).

## Environment reference

Every variable the code reads, and what its absence actually does. The "absent" column is
the point of this table: **almost nothing here refuses to boot**, so a missing variable
shows up as one feature quietly answering 503 rather than as a crash. That is deliberate —
`main` still serves `/healthz` when the database is down, so the platform can report
"up but not ready" instead of crash-looping — but it means the environment is worth
checking deliberately rather than inferring from a healthy container.

### Required

| Variable | Absent |
|---|---|
| `DATABASE_URL` | `main` logs `database unavailable`, still serves `/healthz`, and every route that touches the database answers **503**. `RUN_MIGRATIONS=true` additionally refuses to start the entrypoint. |
| `JWT_SECRET` | Must be **≥ 32 bytes**; a shorter one is refused rather than accepted quietly. Without it `/auth/*` answers 503 and so does every route needing a token — `/agent/*` included, since those call a paid model. |

### Required for a feature

| Variable | Needed by | Absent |
|---|---|---|
| `AWS_BEDROCK_MODEL_ID` | the AI agent | `/agent/*` answers 503 naming the variable. No default: the model id is a decision, not a guess. |
| `AWS_ACCESS_KEY_ID` | the AI agent | as above |
| `AWS_SECRET_ACCESS_KEY` | the AI agent | as above |
| `AWS_BEDROCK_REGION` | the AI agent | defaults to `us-east-1` |
| `AWS_SESSION_TOKEN` | the AI agent | optional; only for temporary credentials |
| `BINANCE_API_KEY` | **live trading** | the venue still opts in, but `GET /venues` reports `credentials_configured: false` and an order is refused. |
| `BINANCE_API_SECRET` | **live trading** | as above |
| `MARKET_FEED` | live bots | defaults to `off`. Bots start and receive nothing, and say so — a bot that is `running` with no feed looks identical to one that is running and finding no setups. Set to `binance`. |
| `MARKET_SYMBOLS` | what the charts can show | comma-separated, defaults to `BTCUSDT`. This is the watchlist `GET /symbols` reports and the set of feeds started at boot. Because market data is **not persisted**, the buffer is empty on a cold start — so a route that listed only what it had buffered would answer `[]`, and a page with no instruments could never ask for the one that fills it. Add every symbol you want charted; each one costs a socket and ~7 MB of RAM (a candle buffer plus a trade tape), not disk. |
| `MARKET_REST_URL` | chart history older than the buffer | defaults to `https://api.binance.com`. Only used to fetch windows the in-memory buffer does not cover. |

Exchange credentials are named **`{VENUE}_API_KEY` / `{VENUE}_API_SECRET`** with the venue
upper-cased — `binance` → `BINANCE_API_KEY`. The list of venues is closed
(`venue_routes::KNOWN_VENUES`), so a typo in the prefix produces a venue that is opted in
and can never trade, rather than an error.

`credentials_configured` is reported **separately** from `opted_in` because the two
failures look identical from outside — "I opted in and it still refused" — and only one of
them is fixable from a browser.

### Optional

| Variable | Default | Notes |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1:8080`; the image sets `0.0.0.0:8080` | the local default is loopback, which would make a container unreachable |
| `RUST_LOG` | `info` | tracing-subscriber `EnvFilter` syntax |
| `LOG_FORMAT` | human-readable lines | `json` for machine-readable logs in deployment |
| `RUN_MIGRATIONS` | `true` | the entrypoint applies migrations, then execs the command; sqlx takes an advisory lock so replicas serialize |
| `ALERT_WEBHOOK_URL` | unset | unset means alerts go to the log and the audit trail only |
| `AGENT_REQUESTS_PER_MINUTE` | `30` | per-user limit on `/agent/*` |
| `AGENT_BURST` | `5` | deliberately smaller than the minute budget |
| `DB_MAX_CONNECTIONS` | `10` | |
| `DB_ACQUIRE_TIMEOUT_SECS` | `10` | |
| `DB_SLOW_STATEMENT_MS` | `5000` | sqlx's own default is 1s, which fires on ordinary chunked upserts against a managed instance — and every warning dumps the whole statement |
| `SKILLS_DIR` | `skills` | where the agent's methodology documents are read from |
| `FRONTEND_DIR` | `frontend/app` | the workstation the gateway serves at `/` |
| `BINANCE_BASE_URL` | mainnet | override for a testnet |

### Read by the build, not the process

`SANDBOX_GUEST_WASM`, `OUT_DIR`, `CARGO`, `CARGO_MANIFEST_DIR` are set by `cargo`/`build.rs`
and baked in at compile time. They are not deployment configuration and setting them has no
effect.

### Not read at all

`.env.example` lists `LLM_PROVIDER` and `LLM_API_KEY` under "secondary / fallback
providers, if we add them later". They are commented out and no code reads them. They are
kept as a note about intent, not as configuration — but a variable in an env template that
nothing reads is the same shape as a metric with no writer, so this is the place to say so.

## Backups & disaster recovery
- Automated Postgres backups with a tested restore procedure (test the restore, not just
  the backup job).
- The Market Data Engine's backfill tool (`docs/04-MARKET-DATA-ENGINE.md`) doubles as a
  disaster-recovery mechanism for lost historical candle/trade data, provided the
  exchange's historical API retains the needed window.

## Done criteria
- A fresh clone of the repo can be brought up locally with a single documented command
  sequence (`docker-compose up` + `cargo run` per service, or a top-level dev script).
- Staging deploys happen automatically on merge to main with zero manual steps.
- A restore-from-backup drill has been performed at least once and documented.
