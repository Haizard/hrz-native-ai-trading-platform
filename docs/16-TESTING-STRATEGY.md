# 16 — Testing Strategy

## Purpose
Define the testing bar every phase in `02-ROADMAP.md` is held to, so "done" means
something consistent across the whole project.

## Layers

1. **Unit tests** — every function in `analytics-core`, `strategy-dsl`, and
   `strategy-runtime` has direct unit tests, including edge cases (empty data, single
   candle, zero volume, NaN-guarding).

2. **Golden-file / reference tests** — for calculations where "correct" is checkable
   against an independently derived expected value (delta, CVD, VWAP, volume profile),
   check in fixed input+expected-output fixtures. See `docs/05-ANALYTICS-ENGINE.md`.

3. **Property-based tests** — invariants that must hold regardless of input, e.g.:
   - `VAH >= POC >= VAL` always.
   - Sum of footprint cell volumes equals candle volume.
   - CVD across a full session equals the sum of per-candle deltas.
   - A `StrategyDocument` that passes validation never causes the runtime to panic.

4. **Integration tests** — per crate boundary: market-data → db round-trip,
   strategy-dsl → strategy-runtime → backtester end-to-end on a fixture dataset,
   sandbox execution parity with native execution (`docs/08-SANDBOX-WASM.md`).

5. **No-look-ahead / correctness tests for the backtester** — a dedicated test
   harness that fails if a strategy can observe data beyond the current simulated
   candle close.

6. **Adversarial / fuzz tests for the sandbox** — malformed, oversized, or
   resource-exhausting `StrategyDocument`s must fail safely (see
   `docs/08-SANDBOX-WASM.md` for the required cases).

7. **Replay/regression tests** — pin a set of known historical market windows and known
   expected backtest results for the reference sample strategy; any change to
   `analytics-core` or `strategy-runtime` that shifts these results must be a deliberate,
   reviewed change, not a silent regression — wire this as a CI check that fails on
   unexpected diffs.

8. **Load/chaos tests** (Phase 7/8) — WebSocket fan-out under many concurrent clients;
   exchange disconnect/reconnect handling; simulated exchange API failures during live
   order placement.

## CI gates (apply from Phase 0 onward)
- `cargo fmt --check`
- `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace`
- `wasm-pack test` (or equivalent) for `analytics-core` and the WASM chart module
- Coverage is tracked but not gated numerically at first; use it to spot untested
  branches in risk-relevant code (sandbox, risk engine, order execution) rather than
  chasing a percentage.

## What "done" means for any doc in this roadmap
A phase/doc is not complete until:
- Its own "Done criteria" section is satisfied.
- New code has unit + (where applicable) integration/property tests.
- CI is green.
- Any new external-facing contract (API route, DSL field, tool signature) is reflected
  in the relevant doc so the next phase's agent has accurate information to build on.
