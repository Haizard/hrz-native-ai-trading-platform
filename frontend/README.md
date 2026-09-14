# Frontend

The trading workstation: a Rust/WASM chart engine and a vanilla-JS shell.

## Decision gate (docs/14) — RESOLVED 2026-09-14

**Decision: Rust/WASM chart engine, vanilla-JavaScript shell.** No Node, no bundler, no
framework.

**Rationale:** the deployment is Rust-only and worth keeping that way; a React shell would
add a Node build stage, a package manager and a second dependency tree to audit for a UI
that is mostly forms. Nothing about the chart gets easier with React — scales, price/time
transforms, footprint aggregation and volume profile are arithmetic over
`analytics-core`, and `docs/14` forbids reimplementing them in TypeScript. The full write-up
is in `docs/14-FRONTEND-CHART-ENGINE.md`.

The non-negotiable still holds, and now has an enforcement point:

> There must never be a second implementation of the trading math in JavaScript.
> The engine returns positioned rectangles; the shell fills them.

## Layout

```
frontend/
├── app/
│   ├── index.html          # the shell: panes, forms, canvas host
│   ├── app.js              # fetch + draw. No market arithmetic, ever.
│   └── chart_engine.wasm   # build artifact, not committed on purpose
├── chart-engine/           # Rust crate: cdylib (wasm) + rlib (host tests)
└── mvp/                    # the Phase 5 stopgap page, kept at /mvp
```

## Building the wasm

```sh
cargo run -p xtask -- build-frontend
```

This compiles `frontend/chart-engine` for `wasm32-unknown-unknown` and copies the artifact
into `frontend/app/`, which the gateway serves at `/chart_engine.wasm`.
`tools/wasm_abi_check.mjs` (Node) checks the JSON-over-`alloc`/`dealloc` ABI that neither
the native unit tests nor the browser exercise.

## Status

Built: candlestick/heikin-ashi/bar/line/area/footprint chart modes, volume profile,
VWAP/POC/VAH/VAL levels, delta/CVD strip, thesis overlay, backtest and paper-bot panes,
session auth, and the strategy editor in **natural-language and raw-DSL modes**.

Not built: the **visual builder** editor mode (the third of `docs/14`'s three), the
DOM/order-book panel — it has no feed, `/ws/orderbook/{symbol}` answers 503 — and
token-streaming AI chat.
