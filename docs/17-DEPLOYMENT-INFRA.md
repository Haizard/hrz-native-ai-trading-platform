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
