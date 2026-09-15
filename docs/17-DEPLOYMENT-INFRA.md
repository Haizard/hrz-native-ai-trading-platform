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
