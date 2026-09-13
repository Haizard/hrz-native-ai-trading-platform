# 03 — Project Structure (Cargo Workspace & Repo Layout)

Generate this skeleton in **Phase 0**, before any feature work. Every later doc assumes
these crate names and boundaries.

```
ai-trading-platform/
├── Cargo.toml                       # workspace root
├── docker-compose.yml                # postgres, and later: redis if needed
├── docs/                             # THIS roadmap (kept in-repo for agent reference)
├── crates/
│   ├── analytics-core/               # Phase 2 — pure math, no I/O, native + wasm32
│   │   └── src/
│   │       ├── types.rs              # Candle, Trade, OrderBookSnapshot, FootprintCell
│   │       ├── delta.rs
│   │       ├── cvd.rs
│   │       ├── vwap.rs
│   │       ├── volume_profile.rs     # POC / VAH / VAL / HVN / LVN
│   │       ├── footprint.rs
│   │       ├── imbalance.rs
│   │       ├── absorption.rs
│   │       ├── liquidity.rs
│   │       ├── market_structure.rs   # swing highs/lows, BOS/CHoCH
│   │       └── indicators/           # EMA, SMA, RSI, ATR, ...
│   │
│   ├── market-data/                  # Phase 1 — exchange collectors, normalization
│   │   └── src/
│   │       ├── exchanges/binance.rs
│   │       ├── orderbook.rs
│   │       ├── candle_builder.rs
│   │       └── backfill.rs
│   │
│   ├── strategy-dsl/                  # Phase 3 — schema, parser, validator
│   │   └── src/
│   │       ├── schema.rs
│   │       ├── parser.rs
│   │       └── validator.rs
│   │
│   ├── strategy-runtime/              # Phase 3 — executes DSL against any data source
│   │   └── src/
│   │       ├── engine.rs
│   │       ├── context.rs             # MarketContext passed to strategies
│   │       └── signal.rs
│   │
│   ├── backtester/                    # Phase 3
│   │   └── src/
│   │       ├── replay.rs
│   │       ├── simulator.rs           # order/position/PnL simulation
│   │       └── report.rs
│   │
│   ├── sandbox/                       # Phase 4 — WASM compilation + capability limits
│   │   └── src/
│   │       ├── compiler.rs
│   │       ├── runtime.rs             # wasmtime/wasmer host
│   │       └── capabilities.rs
│   │
│   ├── ai-agent/                      # Phase 5
│   │   └── src/
│   │       ├── llm_client.rs          # provider-agnostic trait + impls
│   │       ├── tools.rs               # tool registry over analytics-core/strategy-runtime
│   │       ├── skills.rs
│   │       ├── multi_timeframe.rs
│   │       └── thesis.rs              # explainable-thesis object
│   │
│   ├── trading-engine/                # Phase 6/8 — paper + live execution
│   │   └── src/
│   │       ├── paper.rs
│   │       ├── live.rs
│   │       ├── risk.rs
│   │       └── exchange_adapters/binance.rs
│   │
│   ├── api-gateway/                   # Phase 7 — Axum HTTP + WebSocket
│   │   └── src/
│   │       ├── routes/
│   │       ├── ws.rs
│   │       └── auth.rs
│   │
│   └── db/                            # Phase 1+ — sqlx models, migrations
│       ├── migrations/
│       └── src/models.rs
│
├── frontend/                          # Phase 7
│   ├── app/                           # Leptos/Dioxus application shell (or React+TS)
│   └── chart-engine/                  # Rust/WASM chart renderer (separate wasm crate)
│
└── tools/
    ├── xtask/                         # repo automation (codegen, migrations, etc.)
    └── strategy-cli/                  # `backtest run ...` CLI from Phase 3
```

## Crate dependency rules (enforced, not just conventional)

- `analytics-core` depends on **nothing** internal — it's the leaf. No `tokio`, no `sqlx`,
  no network. Must compile for `wasm32-unknown-unknown`.
- `market-data` and `db` may depend on `analytics-core` for shared types, plus async/db
  crates.
- `strategy-dsl` depends only on `analytics-core` (for types) and serde.
- `strategy-runtime` depends on `strategy-dsl` and `analytics-core`.
- `backtester` and `sandbox` depend on `strategy-runtime`.
- `ai-agent` depends on `strategy-dsl` and `analytics-core` (for tool signatures) — it
  must never depend on `sandbox` or `trading-engine` directly; it only ever emits DSL
  documents, which those layers consume independently.
- `trading-engine` depends on `strategy-runtime`, `sandbox`, `db`.
- `api-gateway` depends on everything above; nothing depends on `api-gateway`.
- `frontend/chart-engine` depends on `analytics-core` compiled to WASM — the same crate,
  not a reimplementation.

## Naming & style conventions for agents to follow

- One public error enum per crate (`thiserror`), no `anyhow` in library crates (fine in
  binaries/CLIs).
- All public structs implement `serde::Serialize`/`Deserialize` where they cross a
  process boundary (API, sandbox, DSL).
- Async code uses `tokio`; no mixing of async runtimes.
- Every crate has a `tests/` integration-test directory in addition to inline unit tests.
